//! Recommendation context from existing AAG state, never native credential files.
use super::upstreams::Upstream_option;
use crate::{database::Database, error::AppError};

/// A current native selection is authoritative even when unavailable. Returning
/// its ID lets onboarding explain the conflict instead of changing model family.
pub fn preferred_native(
    db: &Database,
    options: &[Upstream_option],
) -> Result<(Option<String>, Option<String>), AppError> {
    let Some(id) = db.get_current_provider("claude_code")? else {
        return Ok((None, None));
    };
    let source_id = format!("claude:{id}");
    let Some(provider) = db.get_provider_by_id(&id, "claude_code")? else {
        return Ok((Some(source_id), None));
    };
    let mut configured: Vec<String> = [
        provider.settings_config.pointer("/env/ANTHROPIC_MODEL"),
        provider.settings_config.get("model"),
    ]
    .into_iter()
    .flatten()
    .filter_map(|value| value.as_str())
    .map(str::trim)
    .filter(|value| !value.is_empty())
    .map(str::to_string)
    .collect();
    configured.sort();
    configured.dedup();
    let model = if configured.len() == 1
        && options
            .iter()
            .any(|option| option.id == source_id && option.models.contains(&configured[0]))
    {
        configured.pop()
    } else {
        None
    };
    Ok((Some(source_id), model))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Provider;
    use serde_json::json;
    fn option() -> Upstream_option {
        Upstream_option {
            id: "claude:current".into(),
            name: "Current".into(),
            protocol: "anthropic-messages".into(),
            models: vec!["sonnet".into(), "opus".into()],
            source_kind: "claude_native".into(),
            available: true,
            reason: None,
        }
    }
    #[test]
    fn recommends_only_the_actual_current_native_model() {
        let db = Database::memory().unwrap();
        assert_eq!(preferred_native(&db, &[option()]).unwrap(), (None, None));
        db.save_provider("claude_code",&Provider::new("current".into(),"Current".into(),
            json!({"env":{"ANTHROPIC_MODEL":"sonnet","ANTHROPIC_AUTH_TOKEN":"fixture_not_returned"}}))).unwrap();
        db.set_current_provider("current", "claude_code").unwrap();
        assert_eq!(
            preferred_native(&db, &[option()]).unwrap(),
            (Some("claude:current".into()), Some("sonnet".into()))
        );
    }
    #[test]
    fn retains_unavailable_current_source_and_does_not_guess_conflicting_model() {
        let db = Database::memory().unwrap();
        db.save_provider(
            "claude_code",
            &Provider::new(
                "current".into(),
                "Current".into(),
                json!({"env":{"ANTHROPIC_MODEL":"sonnet"},"model":"opus"}),
            ),
        )
        .unwrap();
        db.set_current_provider("current", "claude_code").unwrap();
        assert_eq!(
            preferred_native(&db, &[option()]).unwrap(),
            (Some("claude:current".into()), None)
        );
        assert_eq!(
            preferred_native(&db, &[]).unwrap(),
            (Some("claude:current".into()), None)
        );
    }
}
