//! Explicit native acceptance only. Ordinary test runs never launch desktops.
//! This exercises the real service and AAG database; window/process evidence
//! must be collected separately by the operator after the dispatch receipts.

use super::{claude_open_desktop, codex_open_desktop_window};
use crate::database::Database;
use std::{path::PathBuf, time::Duration};

#[test]
#[ignore = "launches the real desktop mirror twice; opt in with AAG_DESKTOP_MIRROR_ACCEPTANCE=codex or claude"]
fn desktop_mirror_launch_real_twice() {
    let client = std::env::var("AAG_DESKTOP_MIRROR_ACCEPTANCE").unwrap_or_default();
    assert!(
        matches!(client.as_str(), "codex" | "claude"),
        "set AAG_DESKTOP_MIRROR_ACCEPTANCE to exactly codex or claude"
    );
    assert!(
        crate::config::get_db_path().is_file(),
        "existing AAG database required"
    );
    let db = Database::init().expect("open the existing AAG database");
    if client == "codex" && std::env::var("AAG_FRESH_CODEX_MIRROR").as_deref() == Ok("1") {
        let fresh_home = crate::config::get_codex_mirror_dir(None);
        assert!(
            !fresh_home.exists(),
            "fresh mirror must not adopt an existing directory"
        );
        let previous = db
            .get_setting(crate::database::dao::settings::KEY_CODEX_CLI_HOME)
            .unwrap();
        crate::services::provider::codex::switch_codex_mirror(&db, fresh_home.to_str(), false)
            .expect("prepare fresh AAG mirror configuration");
        db.set_setting(
            "codex_cli_home_before_recovery",
            previous.as_deref().unwrap_or("default"),
        )
        .unwrap();
        db.set_setting(
            crate::database::dao::settings::KEY_CODEX_CLI_HOME,
            fresh_home.to_str().unwrap(),
        )
        .unwrap();
    }
    let proxy = db.get_proxy_config().expect("read AAG proxy configuration");
    let endpoint = format!("http://{}:{}", proxy.client_endpoint_host(), proxy.port);
    let snapshot = tauri::async_runtime::block_on(
        crate::services::proxy_runtime::snapshot::build_runtime_snapshot(&db, &proxy),
    )
    .expect("build current routing snapshot");
    let codex_models = snapshot.router.credentialless_models(
        crate::proxy::router::SURFACE_CODEX,
        snapshot.pinned_provider.as_deref(),
    );

    for attempt in 1..=2 {
        let (receipt, home_key, binary_key) = if client == "codex" {
            (
                codex_open_desktop_window(&db, Some(&codex_models), None),
                "codexHome",
                "codexBinary",
            )
        } else {
            (claude_open_desktop(&db), "claudeHome", "claudeBinary")
        };
        let receipt = receipt.expect("native desktop dispatch failed");
        assert!(receipt.get("mirror").and_then(|value| value.as_bool()) == Some(true));
        let directory = PathBuf::from(receipt[home_key].as_str().expect("mirror home receipt"));
        let binary = receipt[binary_key]
            .as_str()
            .expect("desktop binary receipt");
        let config_matches_aag = if client == "codex" {
            let text = std::fs::read_to_string(directory.join("config.toml"))
                .expect("read mirror Codex configuration");
            let config: toml_edit::DocumentMut = text
                .parse()
                .unwrap_or_else(|_| panic!("mirror Codex configuration is invalid"));
            config
                .get("model_providers")
                .and_then(|providers| providers.get("aag"))
                .and_then(|provider| provider.get("base_url"))
                .and_then(|value| value.as_str())
                == Some(format!("{endpoint}/v1").as_str())
        } else {
            let bytes = std::fs::read(directory.join("settings.json"))
                .expect("read mirror Claude configuration");
            let config: serde_json::Value = serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| panic!("mirror Claude configuration is invalid"));
            config
                .get("env")
                .and_then(|environment| environment.get("ANTHROPIC_BASE_URL"))
                .and_then(|value| value.as_str())
                == Some(endpoint.as_str())
        };
        assert!(
            config_matches_aag,
            "mirror endpoint must match the AAG database"
        );
        if client == "claude" {
            assert_eq!(receipt["gatewayMode"], true);
            let profile = PathBuf::from(receipt["desktopProfile"].as_str().unwrap());
            let deployment: serde_json::Value = serde_json::from_slice(
                &std::fs::read(profile.join("claude_desktop_config.json")).unwrap(),
            )
            .unwrap();
            let gateway: serde_json::Value = serde_json::from_slice(
                &std::fs::read(
                    profile.join("configLibrary/00000000-0000-4000-8000-000000008989.json"),
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(deployment["deploymentMode"], "3p");
            assert_eq!(gateway["inferenceProvider"], "gateway");
            assert_eq!(
                gateway["inferenceGatewayBaseUrl"],
                format!("{endpoint}/claude-desktop")
            );
            assert!(
                gateway["inferenceGatewayApiKey"].as_str() == Some(proxy.auth_token.as_str()),
                "gateway bearer matches AAG"
            );
            assert_eq!(gateway["disableDeploymentModeChooser"], true);
        }
        println!(
            "{}",
            serde_json::json!({
                "client": client,
                "attempt": attempt,
                "dispatch": "accepted",
                "binary": binary,
            "profile": receipt.get("desktopProfile").cloned().unwrap_or_else(|| serde_json::json!(directory.join("electron"))),
                "config_matches_aag": config_matches_aag,
                "window_observation_required": true,
            })
        );
        if attempt == 1 {
            std::thread::sleep(Duration::from_secs(1));
        }
    }
}
