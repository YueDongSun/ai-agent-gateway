//! AAG-owned capability registry and workflow observations.
#![allow(non_camel_case_types)]
mod catalog;
mod config;
mod decider;
pub mod ipc;
mod jev;
mod luna;
mod probes;
mod process;
mod routing;
pub mod types;

use crate::{database::Database, services::mcp::Mcp_service};
use std::{collections::HashMap, path::PathBuf, sync::Mutex};
use types::{Capability_list, Health_observation, Health_snapshot, Observation, Route_preview};

pub struct Workflow_service {
    config_root: PathBuf,
    observations: Mutex<HashMap<String, Observation>>,
}

impl Workflow_service {
    pub fn new(config_root: PathBuf) -> Self {
        Self {
            config_root,
            observations: Mutex::new(HashMap::new()),
        }
    }

    pub fn config_root(&self) -> &std::path::Path {
        &self.config_root
    }

    pub fn list_capabilities(
        &self,
        db: &Database,
        mcp: &Mcp_service,
    ) -> Result<Capability_list, String> {
        let mut result = catalog::list(db, mcp)?;
        let observations = self
            .observations
            .lock()
            .map_err(|_| "workflow:state_unavailable".to_string())?;
        let now = chrono::Utc::now();
        for capability in &mut result.capabilities {
            if let Some(observation) = observations.get(&capability.id) {
                let mut observation = observation.clone();
                if observation
                    .expires_at
                    .as_deref()
                    .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                    .is_some_and(|expires_at| expires_at < now)
                {
                    observation.status = "stale".into();
                    observation.summary = "The previous local probe has expired.".into();
                }
                capability
                    .observations
                    .retain(|existing| existing.stage != observation.stage);
                capability.observations.push(observation);
            }
        }
        Ok(result)
    }

    pub fn health(&self, db: &Database, mcp: &Mcp_service) -> Result<Health_snapshot, String> {
        let list = self.list_capabilities(db, mcp)?;
        Ok(Health_snapshot {
            schema_version: 1,
            owner_status: "available".into(),
            next_cursor: None,
            observations: list
                .capabilities
                .into_iter()
                .flat_map(|capability| {
                    capability
                        .observations
                        .into_iter()
                        .map(move |observation| Health_observation {
                            capability_id: capability.id.clone(),
                            observation,
                        })
                })
                .collect(),
        })
    }

    pub fn probe_capability(
        &self,
        db: &Database,
        mcp: &Mcp_service,
        capability_id: &str,
    ) -> Result<Health_snapshot, String> {
        if capability_id.len() > 128
            || !capability_id
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '_')
        {
            return Err("workflow:capability_id_invalid".into());
        }
        let list = self.list_capabilities(db, mcp)?;
        if !list
            .capabilities
            .iter()
            .any(|capability| capability.id == capability_id)
        {
            return Err("workflow:capability_not_found".into());
        }
        let observation = if let Some(capability) = list
            .capabilities
            .iter()
            .find(|capability| capability.id == capability_id)
        {
            if !capability.probe_supported {
                probes::probe(&config::Workflow_config::default(), capability_id)
            } else {
                let config = config::Workflow_config::load(&self.config_root)?;
                config.validate_probe(capability_id)?;
                probes::probe(&config, capability_id)
            }
        } else {
            unreachable!()
        };
        if observation.observed_at.is_some() {
            self.observations
                .lock()
                .map_err(|_| "workflow:state_unavailable".to_string())?
                .insert(capability_id.into(), observation.clone());
        }
        self.health(db, mcp).map(|mut health| {
            health
                .observations
                .retain(|item| item.capability_id == capability_id);
            if observation.observed_at.is_none() {
                health.observations.push(Health_observation {
                    capability_id: capability_id.into(),
                    observation,
                });
            }
            health
        })
    }

    pub fn route_preview(&self, task: &str) -> Route_preview {
        match config::Workflow_config::load(&self.config_root) {
            Ok(config) => routing::preview(&config, task),
            Err(_) => Route_preview::unavailable(
                "Private workflow configuration is invalid or unavailable.",
            ),
        }
    }

    pub async fn evaluate_route(
        &self,
        task: &str,
        external_advice_confirmed: bool,
    ) -> Route_preview {
        match config::Workflow_config::load(&self.config_root) {
            Ok(config) => {
                let task_owned = task.to_owned();
                let config_for_policy = config.clone();
                let route = match tokio::task::spawn_blocking(move || {
                    routing::preview(&config_for_policy, &task_owned)
                })
                .await
                {
                    Ok(route) => route,
                    Err(_) => {
                        return Route_preview::unavailable(
                            "Authoritative policy worker is unavailable.",
                        )
                    }
                };
                if route.advice_status == "unavailable" {
                    return route;
                }
                let mut route = route;
                let authoritative_route = route.clone();
                if config.strands_advice_enabled {
                    let advice_task = task.to_owned();
                    let advice_config = config.clone();
                    let policy = route.clone();
                    route.task_advice = Some(
                        match tokio::task::spawn_blocking(move || {
                            decider::advise(&advice_config, &advice_task, &policy)
                        })
                        .await
                        {
                            Ok(advice) => advice,
                            Err(_) => decider::unavailable("worker_unavailable"),
                        },
                    );
                }
                if config.jev_enabled && external_advice_confirmed {
                    let jev_advice = jev::advise(&config, task, &authoritative_route).await;
                    route.jev_evidence = Some(jev_advice.clone());
                    route.model_advice = Some(if jev_advice.status == "advisory" {
                        jev_advice
                    } else {
                        luna::unavailable_advice()
                    });
                } else {
                    route.model_advice = Some(luna::unavailable_advice());
                }
                route
            }
            Err(_) => Route_preview::unavailable(
                "Private workflow configuration is invalid or unavailable.",
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_router_is_unavailable_without_guessed_policy() {
        let service =
            Workflow_service::new(std::env::temp_dir().join("missing_workflow_config_for_test"));
        let route = service.route_preview("Review a change");
        assert_eq!(route.advice_status, "unavailable");
        assert!(route.model.is_empty());
    }

    #[test]
    fn absent_probe_and_read_only_catalog_ignore_invalid_optional_settings() {
        let directory = tempfile::tempdir().unwrap();
        crate::config::private_fs::prepare_managed_private_dir(directory.path()).unwrap();
        crate::config::private_fs::atomic_write(
            &directory.path().join("workflow_config.json"),
            br#"{"router_path":"missing_router","project_dir":"missing_project","probe_paths":{"cline_cli":"missing_probe"},"strands_advice_enabled":true,"strands_advice_path":"missing_advice","jev_enabled":true,"jev_model":"invalid"}"#,
        ).unwrap();
        let service = Workflow_service::new(directory.path().to_path_buf());
        let db = Database::memory().unwrap();
        let mcp = Mcp_service::new(directory.path().to_path_buf());
        assert!(service.list_capabilities(&db, &mcp).is_ok());
        let health = service.probe_capability(&db, &mcp, "pi_cli").unwrap();
        assert!(health
            .observations
            .iter()
            .any(|item| item.observation.evidence_source == "workflow_config"
                && item.observation.status == "unknown"));
        assert_eq!(
            service
                .probe_capability(&db, &mcp, "cline_cli")
                .unwrap_err(),
            "workflow:configured_executable_invalid"
        );
        assert_eq!(
            service.route_preview("Review this module").advice_status,
            "unavailable"
        );
    }

    #[tokio::test]
    #[ignore = "requires the explicitly configured local model and shared adviser"]
    async fn local_strands_integration_preserves_authoritative_policy() {
        let config_bytes = std::fs::read(
            std::env::var("AAG_STRANDS_TEST_CONFIG").expect("explicit integration config"),
        )
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        crate::config::private_fs::prepare_managed_private_dir(directory.path()).unwrap();
        let mut manager: serde_json::Value = serde_json::from_slice(&config_bytes).unwrap();
        let advice_source = manager["strands_advice_config_path"].as_str().unwrap();
        let mut advice_settings: serde_json::Value =
            serde_json::from_slice(&std::fs::read(advice_source).unwrap()).unwrap();
        let advice_runtime = directory.path().join("advice_runtime");
        crate::config::private_fs::prepare_managed_private_dir(&advice_runtime).unwrap();
        advice_settings["runtime_dir"] = advice_runtime.to_string_lossy().into_owned().into();
        let advice_config_path = directory.path().join("strands_advice_config.json");
        crate::config::private_fs::atomic_write(
            &advice_config_path,
            &serde_json::to_vec(&advice_settings).unwrap(),
        )
        .unwrap();
        manager["strands_advice_config_path"] =
            advice_config_path.to_string_lossy().into_owned().into();
        crate::config::private_fs::atomic_write(
            &directory.path().join("workflow_config.json"),
            &serde_json::to_vec(&manager).unwrap(),
        )
        .unwrap();
        let service = Workflow_service::new(directory.path().to_path_buf());
        config::Workflow_config::load(directory.path())
            .expect("integration configuration boundary");
        let task = "Review the existing Rust module and explain its behavior. Do not modify files.";
        let preview = service.route_preview(task);
        assert_eq!(
            preview.advice_status, "deterministic",
            "{}",
            preview.summary
        );
        assert!(preview.task_advice.is_none());
        let evaluated = service.evaluate_route(task, false).await;
        let observed = evaluated.task_advice.as_ref().expect("local observation");
        assert!(
            matches!(
                observed.status.as_str(),
                "observation_only" | "below_confidence_floor"
            ),
            "{}",
            observed.reason_code
        );
        assert_eq!(
            observed.workflow_class.as_deref(),
            Some("artifact_assessment")
        );
        let mut original_value = serde_json::to_value(&preview).unwrap();
        let mut evaluated_value = serde_json::to_value(&evaluated).unwrap();
        for value in [&mut original_value, &mut evaluated_value] {
            value.as_object_mut().unwrap().remove("task_advice");
            value.as_object_mut().unwrap().remove("model_advice");
        }
        assert_eq!(original_value, evaluated_value);
        assert!(evaluated.jev_evidence.is_none());
        println!("{}", serde_json::to_string(observed).unwrap());
    }
}
