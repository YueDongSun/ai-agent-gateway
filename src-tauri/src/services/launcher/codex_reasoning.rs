//! Codex picker capabilities. Unknown upstreams need an explicit declaration;
//! an alias can advertise only the effort levels accepted by all of its hops.

use crate::proxy::planner::{Model_planner, Resolution};
use crate::{database::Database, error::AppError, provider::Provider};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

const EFFORT_LEVELS: &[&str] = &[
    "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
];

#[derive(Clone, Default)]
pub(crate) struct Reasoning_levels {
    pub levels: Vec<String>,
    pub default: Option<String>,
}

impl Reasoning_levels {
    pub fn wire_levels(&self) -> Vec<Value> {
        self.levels
            .iter()
            .map(|effort| json!({"effort":effort,"description":effort}))
            .collect()
    }
}

pub(crate) fn valid_effort(effort: &str) -> bool {
    EFFORT_LEVELS.contains(&effort)
}

fn known_levels(model: &str) -> Reasoning_levels {
    let model = crate::proxy::strip_one_m_suffix(model).to_ascii_lowercase();
    let dated_model = |base: &str| {
        model == base
            || model
                .strip_prefix(base)
                .and_then(|suffix| suffix.strip_prefix('-'))
                .is_some_and(|suffix| {
                    suffix.len() == 10
                        && suffix
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || byte == b'-')
                })
    };
    let matches_any = |bases: &[&str]| bases.iter().any(|base| dated_model(base));
    // Exact documented model ids (and dated snapshots), never a blanket
    // family prefix: chat/pro variants have different effort contracts.
    let levels: &[&str] = if matches_any(&[
        "gpt-6-astra",
        "gpt-6.1-sol",
        "gpt-6-sol",
        "gpt-6-luna",
        "gpt-5.6-sol",
        "gpt-5.6-terra",
        "gpt-5.6-luna",
    ]) {
        &["low", "medium", "high", "xhigh", "max"]
    } else if matches_any(&["gpt-5.2", "gpt-5.4", "gpt-5.5"]) {
        &["none", "low", "medium", "high", "xhigh"]
    } else if matches_any(&[
        "gpt-5.1-codex-max",
        "gpt-5.2-codex",
        "gpt-5.3-codex",
        "gpt-5.4-mini",
        "gpt-5.4-nano",
    ]) {
        &["low", "medium", "high", "xhigh"]
    } else if dated_model("gpt-5.1") {
        &["none", "low", "medium", "high"]
    } else if matches_any(&[
        "gpt-5-codex",
        "gpt-5.1-codex",
        "gpt-5.1-codex-mini",
        "o1",
        "o3",
        "o3-mini",
        "o4-mini",
    ]) {
        &["low", "medium", "high"]
    } else if matches_any(&["gpt-5", "gpt-5-mini", "gpt-5-nano"]) {
        &["minimal", "low", "medium", "high"]
    } else {
        &[]
    };
    Reasoning_levels {
        levels: levels.iter().map(|v| (*v).into()).collect(),
        default: levels.contains(&"low").then(|| "low".into()),
    }
}

pub(crate) struct Reasoning_catalog {
    declarations: BTreeMap<String, Reasoning_levels>,
    targets: BTreeMap<String, Vec<String>>,
    planner: Option<Arc<dyn Model_planner>>,
}

impl Reasoning_catalog {
    pub fn from_db(db: &Database, selected: Option<&Provider>) -> Result<Self, AppError> {
        let current = if selected.is_none() {
            db.get_current_provider("codex")?
                .map(|id| db.get_provider_by_id(&id, "codex"))
                .transpose()?
                .flatten()
        } else {
            None
        };
        let selected = selected.or(current.as_ref());
        let provider_rows = db.get_all_proxy_providers()?;
        let planner =
            crate::services::proxy_runtime::planner::build_model_planner(db, &provider_rows)?;
        let providers: BTreeMap<_, _> = provider_rows
            .into_iter()
            .filter(|p| p.enabled && p.provider_type != "anthropic_passthrough")
            .map(|p| (p.id.clone(), p))
            .collect();
        let mut targets = BTreeMap::new();
        for route in db.get_all_routes()? {
            targets.insert(
                route.alias,
                route
                    .hops
                    .into_iter()
                    .filter(|hop| providers.contains_key(&hop.provider))
                    .map(|hop| hop.model)
                    .collect(),
            );
        }
        let mut declarations = BTreeMap::new();
        if let Some(entries) = selected
            .and_then(|p| p.settings_config.pointer("/modelCatalog/models"))
            .and_then(Value::as_array)
        {
            for entry in entries {
                let display = entry.get("display").and_then(Value::as_str).unwrap_or("");
                let model = entry
                    .get("model")
                    .and_then(Value::as_str)
                    .unwrap_or(display);
                if !display.is_empty() {
                    targets
                        .entry(display.into())
                        .or_insert_with(|| vec![model.into()]);
                }
                if let Some(raw_levels) = entry
                    .get("supported_reasoning_levels")
                    .or_else(|| entry.get("supportedReasoningEfforts"))
                {
                    let raw_levels = raw_levels
                        .as_array()
                        .ok_or_else(|| AppError::Config("codex_reasoning_levels_invalid".into()))?;
                    let mut levels = Vec::new();
                    for raw in raw_levels {
                        let effort = raw
                            .as_str()
                            .or_else(|| raw.get("effort").and_then(Value::as_str))
                            .filter(|value| valid_effort(value))
                            .ok_or_else(|| {
                                AppError::Config("codex_reasoning_levels_invalid".into())
                            })?;
                        if !levels.iter().any(|value| value == effort) {
                            levels.push(effort.to_string());
                        }
                    }
                    let default = entry
                        .get("default_reasoning_level")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .or_else(|| levels.iter().find(|v| v.as_str() == "low").cloned())
                        .or_else(|| levels.first().cloned());
                    if default
                        .as_ref()
                        .is_some_and(|value| !levels.contains(value))
                    {
                        return Err(AppError::Config("codex_reasoning_default_invalid".into()));
                    }
                    declarations.insert(
                        if display.is_empty() {
                            model.into()
                        } else {
                            display.into()
                        },
                        Reasoning_levels { levels, default },
                    );
                }
            }
        }
        let mut pool_targets = BTreeMap::<String, Vec<String>>::new();
        for provider in providers.values() {
            for (alias, model) in &provider.model_mapping {
                pool_targets
                    .entry(alias.clone())
                    .or_default()
                    .push(model.clone());
            }
        }
        for (alias, models) in pool_targets {
            targets.entry(alias).or_insert(models);
        }
        Ok(Self {
            declarations,
            targets,
            planner: Some(planner),
        })
    }

    pub fn for_model(&self, model: &str) -> Reasoning_levels {
        if let Some(declared) = self.declarations.get(model) {
            return declared.clone();
        }
        let planned_targets = self.planner.as_ref().and_then(|planner| {
            match planner.resolve(model, Default::default()) {
                Ok(Resolution::Alias { hops, .. }) | Ok(Resolution::Precise { hops, .. }) => {
                    Some(hops.into_iter().map(|hop| hop.model).collect::<Vec<_>>())
                }
                Ok(Resolution::Exact(_)) => None,
                Err(_) => Some(Vec::new()),
            }
        });
        let targets = match (self.targets.get(model), planned_targets) {
            (Some(configured), Some(planned)) => configured
                .iter()
                .filter(|target| planned.contains(target))
                .cloned()
                .collect(),
            (None, Some(planned)) => planned,
            (Some(configured), None) => configured.clone(),
            (None, None) => return known_levels(model),
        };
        let mut resolved = targets.iter().map(|target| {
            self.declarations
                .get(target)
                .cloned()
                .unwrap_or_else(|| known_levels(target))
        });
        let Some(mut result) = resolved.next() else {
            return Reasoning_levels::default();
        };
        for next in resolved {
            let supported: BTreeSet<_> = next.levels.iter().collect();
            result.levels.retain(|level| supported.contains(level));
        }
        result.default = result
            .default
            .filter(|value| result.levels.contains(value))
            .or_else(|| result.levels.first().cloned());
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declarations_preserve_extended_tiers_and_explicit_unsupported() {
        let db = Database::memory().unwrap();
        let provider = Provider::new(
            "codex".into(),
            "Codex".into(),
            json!({"modelCatalog":{"models":[
                {"display":"custom","model":"vendor","supportedReasoningEfforts":["low","medium","high","max","ultra"],"default_reasoning_level":"medium"},
                {"display":"plain","model":"gpt-6-sol","supported_reasoning_levels":[]}
            ]}}),
        );
        let catalog = Reasoning_catalog::from_db(&db, Some(&provider)).unwrap();
        assert_eq!(
            catalog.for_model("custom").levels,
            ["low", "medium", "high", "max", "ultra"]
        );
        assert_eq!(
            catalog.for_model("custom").default.as_deref(),
            Some("medium")
        );
        assert!(catalog.for_model("plain").levels.is_empty());
        assert!(catalog.for_model("unknown").levels.is_empty());
    }

    #[test]
    fn aliases_use_concrete_capabilities_and_intersect_all_targets() {
        let catalog = Reasoning_catalog {
            declarations: BTreeMap::new(),
            targets: BTreeMap::from([
                ("5.6sol".into(), vec!["gpt-5.6-sol".into()]),
                (
                    "mixed".into(),
                    vec!["gpt-6-sol".into(), "gpt-5-codex".into()],
                ),
                (
                    "unknown_fallback".into(),
                    vec!["gpt-6-sol".into(), "unknown".into()],
                ),
            ]),
            planner: None,
        };
        assert_eq!(
            catalog.for_model("5.6sol").levels,
            ["low", "medium", "high", "xhigh", "max"]
        );
        assert_eq!(catalog.for_model("mixed").levels, ["low", "medium", "high"]);
        assert!(catalog.for_model("unknown_fallback").levels.is_empty());
    }

    #[test]
    fn malformed_declaration_is_reported_instead_of_advertising_a_default() {
        let db = Database::memory().unwrap();
        for declaration in [json!("high"), json!(["unknown_tier"])] {
            let provider = Provider::new(
                "codex".into(),
                "Codex".into(),
                json!({"modelCatalog":{"models":[{"display":"custom","supportedReasoningEfforts":declaration}]}}),
            );
            assert!(Reasoning_catalog::from_db(&db, Some(&provider)).is_err());
        }
    }

    #[test]
    fn role_names_use_planned_supply_instead_of_their_gpt_spelling() {
        let db = Database::memory().unwrap();
        let supplier = serde_json::from_value(json!({"expectedGeneration":0,"name":"Role fixture","baseUrl":"https://example.invalid","apiKey":"fixture","endpoints":[{"protocol":"openai-chat","models":[{"modelId":"unknown-upstream","tierOverride":3}]}]})).unwrap();
        crate::services::suppliers::save_supplier(&db, supplier).unwrap();
        db.set_setting(crate::services::mirror_models::KEY_ENABLED, "1")
            .unwrap();
        let catalog = Reasoning_catalog::from_db(&db, None).unwrap();
        assert!(catalog.for_model("gpt-5.6-sol").levels.is_empty());
        assert!(known_levels("gpt-5-chat-latest").levels.is_empty());
        assert!(known_levels("gpt-5-pro").levels.is_empty());
        assert!(known_levels("o1-preview").levels.is_empty());
        assert_eq!(
            known_levels("gpt-5.2").levels,
            ["none", "low", "medium", "high", "xhigh"]
        );
        assert_eq!(
            known_levels("gpt-5").levels,
            ["minimal", "low", "medium", "high"]
        );
    }
}
