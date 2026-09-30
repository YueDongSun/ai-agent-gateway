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
        if previous_default.is_none() {
            restore_default = current_default.unwrap_or(Value::Null);
        } else if current_default.as_ref() != previous_default {
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
}
