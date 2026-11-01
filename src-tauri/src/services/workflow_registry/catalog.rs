//! Static descriptors plus redacted summaries from the existing MCP owner.
#![allow(non_camel_case_types)]
use super::types::{Capability, Capability_list, Observation};
use crate::{database::Database, services::mcp::Mcp_service};
use serde::Deserialize;

#[derive(Deserialize)]
struct Static_catalog {
    schema_version: u8,
    capabilities: Vec<Capability>,
}

fn unknown(stage: &str, source: &str, summary: &str) -> Observation {
    Observation {
        stage: stage.into(),
        status: "unknown".into(),
        observed_at: None,
        expires_at: None,
        evidence_source: source.into(),
        summary: summary.into(),
    }
}

pub fn list(db: &Database, mcp: &Mcp_service) -> Result<Capability_list, String> {
    let catalog: Static_catalog =
        serde_json::from_str(include_str!("../../../../shared/workflow_catalog.json"))
            .map_err(|_| "workflow:catalog_invalid".to_string())?;
    if catalog.schema_version != 1 {
        return Err("workflow:catalog_schema_invalid".into());
    }
    let mut capabilities = catalog.capabilities;
    for capability in &mut capabilities {
        capability.probe_supported = matches!(
            capability.id.as_str(),
            "pi_cli" | "cline_cli" | "kimi_cli" | "minimax_cli"
        );
        capability.observations = ["installed", "connected", "discovered", "callable", "probe"]
            .into_iter()
            .map(|stage| {
                unknown(
                    stage,
                    "static_catalog",
                    if capability.host_scope == "host" {
                        "Requires an authorized host observation."
                    } else {
                        "This stage has not been observed."
                    },
                )
            })
            .collect();
    }
    for server in mcp
        .list(db)
        .map_err(|_| "workflow:mcp_registry_unavailable".to_string())?
    {
        let mut observations = vec![
            Observation {
                stage: "configured".into(),
                status: "configured".into(),
                observed_at: Some(chrono::Utc::now().to_rfc3339()),
                expires_at: None,
                evidence_source: "mcp_registry".into(),
                summary: "Definition exists in the AAG MCP registry; callable state is unverified."
                    .into(),
            },
            unknown(
                "callable",
                "mcp_registry",
                "A configured MCP server has not been called or verified.",
            ),
        ];
        if server.validation_error.is_some() {
            observations[0].status = "invalid".into();
            observations[0].summary = "Registry definition failed validation.".into();
        }
        capabilities.push(Capability {
            id: format!("mcp_{}", server.id),
            name: server.name,
            kind: "mcp_server".into(),
            group: "mcp".into(),
            owner: "mcp_registry".into(),
            access_method: "mcp_registry".into(),
            host_scope: "local".into(),
            source_ref: "mcp_registry".into(),
            mcp_server_id: Some(server.id),
            summary: "Registered MCP server. User descriptions are omitted.".into(),
            probe_supported: false,
            observations,
        });
    }
    Ok(Capability_list {
        schema_version: 1,
        capabilities,
        next_cursor: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_descriptions_are_omitted_from_workflow_catalog() {
        let db = Database::memory().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let mcp = Mcp_service::new(directory.path().to_path_buf());
        mcp.save(&db, crate::services::mcp::Mcp_server_input {
            id: "fixture_tools".into(), name: "Fixture tools".into(),
            server_config: Some(serde_json::json!({"command":"fixture_executable"})),
            description: Some("private_description_sentinel https://private.example.test bearer synthetic_secret".into()),
            create_only: true, expected_revision: None,
            enabled_claude: false, enabled_codex: false,
        }).unwrap();
        let result = list(&db, &mcp).unwrap();
        let serialized = serde_json::to_string(&result).unwrap();
        assert!(!serialized.contains("private_description_sentinel"));
        assert!(!serialized.contains("private.example.test"));
        assert!(!serialized.contains("synthetic_secret"));
        let capability = result
            .capabilities
            .iter()
            .find(|item| item.id == "mcp_fixture_tools")
            .unwrap();
        assert_eq!(
            capability.summary,
            "Registered MCP server. User descriptions are omitted."
        );
    }

    #[test]
    fn static_descriptors_do_not_claim_callability() {
        let db = Database::memory().expect("memory database");
        let mcp = Mcp_service::new(std::env::temp_dir());
        let result = list(&db, &mcp).expect("catalog");
        assert_eq!(result.schema_version, 1);
        assert!(result
            .capabilities
            .iter()
            .any(|item| item.id == "github_revision_review"));
        assert!(result
            .capabilities
            .iter()
            .any(|item| item.id == "pi_cli" && item.probe_supported));
        assert!(result.capabilities.iter().all(|item| item
            .observations
            .iter()
            .all(|observation| observation.status != "callable")));
    }
}
