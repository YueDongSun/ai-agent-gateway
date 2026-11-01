//! Merge AAG's selected route without adopting or deleting DSH-owned providers.
use super::config::failure;
use crate::error::AppError;
use serde_json::{json, Map, Value};

pub(super) fn apply(
    existing: &mut Value,
    desired: &Value,
    previous: Option<Value>,
) -> Result<Value, AppError> {
    let settings = existing
        .as_object_mut()
        .ok_or_else(|| failure("invalid_managed_settings"))?;
    let desired_providers = desired
        .pointer("/llm-pi-ai/providers")
        .and_then(Value::as_object);
    if desired_providers.is_some_and(|providers| providers.len() != 1) {
        return Err(failure("invalid_llm_settings"));
    }
    let next_route = desired_providers.and_then(|providers| providers.iter().next());
    let previous = previous.unwrap_or_else(|| json!({}));
    let previous_key = previous.get("provider_key").and_then(Value::as_str);
    let mut restore_default = previous
        .get("restore_default")
        .cloned()
        .unwrap_or(Value::Null);
    let previous_default = previous
        .get("managed_default")
        .filter(|value| !value.is_null());
    let current_default = settings.get("agent-default-model").cloned();

    // Only remove the exact value last written by AAG. A modified value belongs
    // to the user again; a new AAG selection must not overwrite it silently.
    if let Some(key) = previous_key {
        if let Some(providers) = settings
            .get_mut("llm-pi-ai")
            .and_then(Value::as_object_mut)
            .and_then(|adapter| adapter.get_mut("providers"))
            .and_then(Value::as_object_mut)
        {
            if providers.get(key) == previous.get("managed_provider") {
                providers.remove(key);
            }
        }
    }

    if let Some((key, provider)) = next_route {
        let adapter = settings
            .entry("llm-pi-ai")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or_else(|| failure("invalid_managed_settings"))?;
        let providers = adapter
            .entry("providers")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or_else(|| failure("invalid_managed_settings"))?;
        if providers.contains_key(key) {
            return Err(failure("upstream_key_conflict"));
        }
        providers.insert(key.clone(), provider.clone());
        let managed_default = desired
            .get("agent-default-model")
            .cloned()
            .ok_or_else(|| failure("invalid_llm_settings"))?;
        // Both a first managed write (no recorded previous default) and any
        // later change capture the live default so removal can restore it.
        if previous_default.is_none() || current_default.as_ref() != previous_default {
            restore_default = current_default.unwrap_or(Value::Null);
        }
        settings.insert("agent-default-model".into(), managed_default.clone());
        Ok(
            json!({"provider_key":key,"managed_provider":provider,"managed_default":managed_default,"restore_default":restore_default}),
        )
    } else {
        if previous_default.is_some() && current_default.as_ref() == previous_default {
            if restore_default.is_null() {
                settings.remove("agent-default-model");
            } else {
                settings.insert("agent-default-model".into(), restore_default);
            }
        }
        // Removing an empty owned providers map must not remove other adapter
        // preferences. An untouched DSH-owned adapter is never normalized.
        if previous_key.is_some() {
            if let Some(adapter) = settings.get_mut("llm-pi-ai").and_then(Value::as_object_mut) {
                if adapter
                    .get("providers")
                    .and_then(Value::as_object)
                    .is_some_and(Map::is_empty)
                {
                    adapter.remove("providers");
                }
                if adapter.is_empty() {
                    settings.remove("llm-pi-ai");
                }
            }
        }
        Ok(json!({}))
    }
}

const BINDING_SCHEMA: u64 = 2;

fn is_schema_v2(value: &Value) -> bool {
    value.get("schema").and_then(Value::as_u64) == Some(BINDING_SCHEMA)
}

fn marker_matches_settings(settings: &Value, marker: &Value) -> bool {
    match (
        marker.get("provider_key").and_then(Value::as_str),
        marker.get("managed_provider"),
    ) {
        (Some(key), Some(managed)) => {
            settings
                .pointer("/llm-pi-ai/providers")
                .and_then(Value::as_object)
                .and_then(|providers| providers.get(key))
                == Some(managed)
        }
        _ => false,
    }
}

/// Resolve the ownership marker that `apply` must treat as previous.
///
/// The sidecar marker is written in two phases around the managed
/// `settings.json` commit: `{schema, committed, pending}` before, and a sealed
/// `{schema, committed, pending: null}` after. A start interrupted between the
/// phases leaves the marker and settings describing different generations.
/// A pending marker is promoted only when settings still carries exactly the
/// provider value that interrupted start intended to write — any other disk
/// state (including a user edit) falls back to the committed marker, so the
/// exact-value ownership rule is never weakened. A legacy flat marker (schema
/// v1) is its own committed value.
pub(super) fn resolve_previous(recorded: &Option<Value>, settings: &Value) -> Option<Value> {
    let Some(recorded) = recorded.as_ref() else {
        return None;
    };
    if !is_schema_v2(recorded) {
        return Some(recorded.clone());
    }
    if let Some(pending) = recorded.get("pending").filter(|value| !value.is_null()) {
        if marker_matches_settings(settings, pending) {
            return Some(pending.clone());
        }
    }
    recorded
        .get("committed")
        .filter(|value| !value.is_null())
        .cloned()
}

pub(super) fn committed_binding(recorded: &Option<Value>) -> Value {
    match recorded.as_ref() {
        Some(value) if is_schema_v2(value) => {
            value.get("committed").cloned().unwrap_or(Value::Null)
        }
        Some(value) => value.clone(),
        None => Value::Null,
    }
}

pub(super) fn pending_binding(committed: &Value, next: &Value) -> Value {
    json!({"schema": BINDING_SCHEMA, "committed": committed, "pending": next})
}

pub(super) fn commit_binding(next: &Value) -> Value {
    json!({"schema": BINDING_SCHEMA, "committed": next, "pending": Value::Null})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn selected(key: &str) -> Value {
        json!({"llm-pi-ai":{"providers":{key:{"apiKeyEnv":"AAG_DSH_API_KEY"}}},"agent-default-model":{"provider":key,"model":"managed"}})
    }

    #[test]
    fn selection_and_deselection_preserve_user_providers_and_restore_user_default() {
        let original = json!({"llm-pi-ai":{"providers":{"user_provider":{"api":"openai-completions"}},"user_option":true},"agent-default-model":{"provider":"user_provider","model":"user_model"},"other_preferences":{"value":42}});
        let mut settings = original.clone();
        let ownership = apply(&mut settings, &selected("aag"), None).unwrap();
        assert!(settings
            .pointer("/llm-pi-ai/providers/user_provider")
            .is_some());
        let ownership = apply(&mut settings, &selected("anthropic"), Some(ownership)).unwrap();
        assert!(settings.pointer("/llm-pi-ai/providers/aag").is_none());
        assert!(settings.pointer("/llm-pi-ai/providers/anthropic").is_some());
        apply(&mut settings, &json!({}), Some(ownership)).unwrap();
        assert_eq!(settings, original);
    }

    #[test]
    fn user_owned_conflicts_are_rejected_and_unselected_settings_untouched() {
        let original = json!({"llm-pi-ai":{"providers":{"aag":{"api":"user_choice"}}},"agent-default-model":{"provider":"user","model":"chosen"}});
        let mut settings = original.clone();
        apply(&mut settings, &json!({}), None).unwrap();
        assert_eq!(settings, original);
        assert!(apply(&mut settings, &selected("aag"), None)
            .unwrap_err()
            .to_string()
            .contains("upstream_key_conflict"));
        assert_eq!(settings, original);
    }

    #[test]
    fn edits_to_previously_managed_values_are_not_discarded() {
        let mut settings = json!({});
        let ownership = apply(&mut settings, &selected("aag"), None).unwrap();
        settings["llm-pi-ai"]["providers"]["aag"]["user_field"] = json!(true);
        settings["agent-default-model"] = json!({"provider":"user","model":"edited"});
        let edited = settings.clone();
        apply(&mut settings, &json!({}), Some(ownership)).unwrap();
        assert_eq!(settings, edited);
    }

    fn route_with(base_url: &str) -> Value {
        json!({"llm-pi-ai":{"providers":{"aag":{"apiKeyEnv":"AAG_DSH_API_KEY","baseURL":base_url}}},"agent-default-model":{"provider":"aag","model":"managed"}})
    }

    /// Documents the pre-recovery wedge: `apply` alone still refuses when the
    /// committed marker lags a settings generation it did not write. Only
    /// `resolve_previous` can bridge the generations; this test pins that the
    /// conflict path is real and that recovery must not rely on `apply`.
    #[test]
    fn stale_committed_marker_alone_still_conflicts_without_recovery() {
        let mut settings = json!({});
        let first = apply(&mut settings, &route_with("http://127.0.0.1:9/first"), None).unwrap();
        let mut crashed = settings.clone();
        let second = apply(
            &mut crashed,
            &route_with("http://127.0.0.1:9/second"),
            Some(first.clone()),
        )
        .unwrap();
        // Interrupted before the seal: committed still describes `first`.
        let previous = Some(first);
        let conflict = apply(
            &mut crashed,
            &route_with("http://127.0.0.1:9/third"),
            previous,
        );
        assert!(conflict
            .unwrap_err()
            .to_string()
            .contains("upstream_key_conflict"));
        assert_eq!(
            crashed
                .pointer("/llm-pi-ai/providers/aag/baseURL")
                .and_then(Value::as_str),
            Some("http://127.0.0.1:9/second"),
            "a refused apply must not modify settings"
        );
        assert!(second.get("provider_key").is_some());
    }

    #[test]
    fn interrupted_start_before_the_settings_commit_self_heals_from_committed() {
        let mut settings = json!({});
        let first = apply(&mut settings, &route_with("http://127.0.0.1:9/first"), None).unwrap();
        // Crash after the pending write, before settings.json was rewritten:
        // disk settings still carries the first generation.
        let second_intent = json!({"provider_key":"aag","managed_provider":{"apiKeyEnv":"AAG_DSH_API_KEY","baseURL":"http://127.0.0.1:9/second"},"managed_default":{"provider":"aag","model":"managed"},"restore_default":null});
        let recorded = Some(pending_binding(&first, &second_intent));
        let previous = resolve_previous(&recorded, &settings);
        assert_eq!(previous, Some(first));
        apply(
            &mut settings,
            &route_with("http://127.0.0.1:9/second"),
            previous,
        )
        .unwrap();
        assert_eq!(
            settings
                .pointer("/llm-pi-ai/providers/aag/baseURL")
                .and_then(Value::as_str),
            Some("http://127.0.0.1:9/second")
        );
    }

    #[test]
    fn interrupted_start_after_the_settings_commit_promotes_the_pending_marker() {
        let mut settings = json!({});
        let first = apply(&mut settings, &route_with("http://127.0.0.1:9/first"), None).unwrap();
        let mut crashed = settings.clone();
        let second = apply(
            &mut crashed,
            &route_with("http://127.0.0.1:9/second"),
            Some(first.clone()),
        )
        .unwrap();
        // Crash after settings.json committed but before the seal.
        let recorded = Some(pending_binding(&first, &second));
        let previous = resolve_previous(&recorded, &crashed);
        assert_eq!(previous, Some(second));
        apply(
            &mut crashed,
            &route_with("http://127.0.0.1:9/third"),
            previous,
        )
        .unwrap();
        assert_eq!(
            crashed
                .pointer("/llm-pi-ai/providers/aag/baseURL")
                .and_then(Value::as_str),
            Some("http://127.0.0.1:9/third"),
            "a crashed start must not wedge later starts"
        );
    }

    #[test]
    fn a_pending_marker_is_never_promoted_over_a_user_edit() {
        let mut settings = json!({});
        let first = apply(&mut settings, &route_with("http://127.0.0.1:9/first"), None).unwrap();
        settings["llm-pi-ai"]["providers"]["aag"]["user_field"] = json!(true);
        let edited_provider = settings
            .pointer("/llm-pi-ai/providers/aag")
            .cloned()
            .unwrap();
        let pending_intent = json!({"provider_key":"aag","managed_provider":{"apiKeyEnv":"AAG_DSH_API_KEY","baseURL":"http://127.0.0.1:9/second"},"managed_default":{"provider":"aag","model":"managed"},"restore_default":null});
        let recorded = Some(pending_binding(&first, &pending_intent));
        // The pending value does not match the edited disk state: fall back to
        // committed, whose exact-match removal then refuses the user edit.
        let previous = resolve_previous(&recorded, &settings);
        assert_eq!(previous, Some(first));
        apply(&mut settings, &json!({}), previous).unwrap();
        assert_eq!(
            settings.pointer("/llm-pi-ai/providers/aag"),
            Some(&edited_provider),
            "a user-edited managed provider must survive deselection"
        );
    }

    #[test]
    fn legacy_flat_and_sealed_v2_markers_resolve_to_their_committed_generation() {
        let mut settings = json!({});
        let ownership = apply(&mut settings, &selected("aag"), None).unwrap();
        assert_eq!(resolve_previous(&None, &settings), None);
        assert_eq!(
            resolve_previous(&Some(ownership.clone()), &settings),
            Some(ownership.clone())
        );
        assert_eq!(
            committed_binding(&Some(ownership.clone())),
            ownership.clone()
        );
        assert_eq!(committed_binding(&None), Value::Null);
        let sealed = commit_binding(&ownership);
        assert_eq!(
            resolve_previous(&Some(sealed), &settings),
            Some(ownership.clone())
        );
        apply(&mut settings, &json!({}), Some(ownership)).unwrap();
        assert!(settings.pointer("/llm-pi-ai/providers/aag").is_none());
        // A sealed deselection marker keeps apply on the unmanaged path.
        let deselected = commit_binding(&json!({}));
        assert_eq!(
            resolve_previous(&Some(deselected), &settings),
            Some(json!({}))
        );
    }
}
