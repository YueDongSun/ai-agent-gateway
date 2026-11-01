//! Isolated Claude Desktop third-party gateway configuration.
//! Upstream and OAuth credentials never enter this profile; it uses AAG's local bearer.
use crate::{config, database::Database, error::AppError};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const PROFILE_ID: &str = "00000000-0000-4000-8000-000000008989";
const PROFILE_NAME: &str = "AAG Gateway";

pub(crate) struct DesktopMirrorPaths {
    pub local_app_data: PathBuf,
    pub user_data: PathBuf,
}

fn snapshot_object(snapshot: &config::private_fs::MirrorFileSnapshot) -> Result<Value, AppError> {
    let value = match snapshot.contents.as_deref() {
        Some(bytes) => serde_json::from_slice(bytes)?,
        None => json!({}),
    };
    if !value.is_object() {
        return Err(AppError::Config("claude_desktop_profile_invalid".into()));
    }
    Ok(value)
}

#[cfg(test)]
fn read_object(path: &Path) -> Result<Value, AppError> {
    snapshot_object(&config::private_fs::read_mirror_file(path)?)
}

pub(crate) fn write_gateway_profile(
    db: &Database,
    mirror_home: &Path,
) -> Result<DesktopMirrorPaths, AppError> {
    let proxy = db.get_proxy_config()?;
    if proxy.auth_token.trim().is_empty() {
        return Err(AppError::Config(
            "claude_desktop_gateway_auth_required".into(),
        ));
    }
    if !proxy.is_loopback_bind() {
        return Err(AppError::Config(
            "claude_desktop_loopback_gateway_required".into(),
        ));
    }
    if config::same_dir(mirror_home, &config::get_claude_config_dir()) {
        return Err(AppError::Config(
            "claude_desktop_native_home_refused".into(),
        ));
    }
    let local_app_data = mirror_home.join("desktop_local");
    let user_data = local_app_data.join("Claude-3p");
    let library = user_data.join("configLibrary");
    // Precreation prevents Desktop's legacy migration from importing native APPDATA/Claude-3p data.
    for directory in [mirror_home, &local_app_data, &user_data, &library] {
        config::private_fs::prepare_managed_private_dir(directory)?;
    }
    let deployment_path = user_data.join("claude_desktop_config.json");
    let meta_path = library.join("_meta.json");
    let profile_path = library.join(format!("{PROFILE_ID}.json"));
    let deployment_snapshot = config::private_fs::read_mirror_file(&deployment_path)?;
    let meta_snapshot = config::private_fs::read_mirror_file(&meta_path)?;
    let profile_snapshot = config::private_fs::read_mirror_file(&profile_path)?;
    let mut deployment = snapshot_object(&deployment_snapshot)?;
    deployment["deploymentMode"] = json!("3p");
    let mut meta = snapshot_object(&meta_snapshot)?;
    let entries = meta
        .as_object_mut()
        .unwrap()
        .entry("entries")
        .or_insert_with(|| json!([]));
    let entries = entries
        .as_array_mut()
        .ok_or_else(|| AppError::Config("claude_desktop_profile_entries_invalid".into()))?;
    if let Some(entry) = entries
        .iter_mut()
        .find(|entry| entry.get("id").and_then(Value::as_str) == Some(PROFILE_ID))
    {
        entry["name"] = json!(PROFILE_NAME);
    } else {
        entries.push(json!({"id":PROFILE_ID,"name":PROFILE_NAME}));
    }
    meta["appliedId"] = json!(PROFILE_ID);
    let mut profile = snapshot_object(&profile_snapshot)?;
    profile["inferenceProvider"] = json!("gateway");
    profile["inferenceCredentialKind"] = json!("static");
    profile["inferenceGatewayBaseUrl"] = json!(format!(
        "http://{}:{}/claude-desktop",
        proxy.client_endpoint_host(),
        proxy.port
    ));
    profile["inferenceGatewayApiKey"] = json!(proxy.auth_token);
    profile["inferenceGatewayAuthScheme"] = json!("bearer");
    profile["disableDeploymentModeChooser"] = json!(true);
    profile["modelDiscoveryEnabled"] = json!(true);
    // A nonempty administrator list overrides discovery in Claude Desktop.
    // Remove the old fixed-role whitelist so the current gateway catalog owns
    // the picker, including additions, removals and the selected default.
    profile.as_object_mut().unwrap().remove("inferenceModels");
    let documents = [
        (deployment_path, deployment),
        (meta_path, meta),
        (profile_path, profile),
    ];
    let serialized = documents
        .iter()
        .map(|(_, value)| serde_json::to_vec_pretty(value))
        .collect::<Result<Vec<_>, _>>()?;
    let snapshots = [&deployment_snapshot, &meta_snapshot, &profile_snapshot];
    let updates = documents
        .iter()
        .zip(serialized.iter())
        .zip(snapshots)
        .map(
            |(((path, _), contents), previous)| super::config_writes::MirrorConfigFileUpdate {
                path,
                contents,
                app_type: "claude_desktop",
                previous,
            },
        )
        .collect::<Vec<_>>();
    super::config_writes::write_mirror_files(&updates)?;
    Ok(DesktopMirrorPaths {
        local_app_data,
        user_data,
    })
}

#[cfg(test)]
mod tests {
    use super::super::test_support::isolated_home;
    use super::*;
    #[test]
    fn desktop_rejects_empty_auth_and_nonlocal_gateway_before_creating_files() {
        let home = isolated_home("desktop-invalid-gateway");
        let db = Database::memory().unwrap();
        let mirror = home.root.join("mirror");
        assert!(write_gateway_profile(&db, &mirror).is_err());
        assert!(!mirror.exists());
        let mut proxy = db.get_proxy_config().unwrap();
        proxy.auth_token = "fixture_token".into();
        proxy.host = "192.0.2.5".into();
        db.save_proxy_config(&proxy).unwrap();
        assert!(write_gateway_profile(&db, &mirror).is_err());
        assert!(!mirror.exists());
    }
    #[test]
    fn desktop_gateway_profile_is_isolated_static_and_preserves_unknown_fields() {
        let home = isolated_home("desktop-gateway");
        let db = Database::memory().unwrap();
        let mut proxy = db.get_proxy_config().unwrap();
        proxy.auth_token = "fixture_aag_bearer".into();
        db.save_proxy_config(&proxy).unwrap();
        let mirror = home.root.join(".claude-cli");
        let paths = write_gateway_profile(&db, &mirror).unwrap();
        assert_eq!(paths.local_app_data, mirror.join("desktop_local"));
        assert_eq!(
            read_object(&paths.user_data.join("claude_desktop_config.json")).unwrap()
                ["deploymentMode"],
            "3p"
        );
        let profile_path = paths
            .user_data
            .join("configLibrary")
            .join(format!("{PROFILE_ID}.json"));
        let mut profile = read_object(&profile_path).unwrap();
        assert_eq!(profile["inferenceProvider"], "gateway");
        assert_eq!(profile["inferenceCredentialKind"], "static");
        assert_eq!(profile["inferenceGatewayApiKey"], "fixture_aag_bearer");
        assert_eq!(profile["disableDeploymentModeChooser"], true);
        assert_eq!(profile["modelDiscoveryEnabled"], true);
        assert!(profile.get("inferenceModels").is_none());
        assert_eq!(
            profile["inferenceGatewayBaseUrl"],
            "http://127.0.0.1:8989/claude-desktop"
        );
        profile["customUserPreference"] = json!("preserved");
        // Upgrade the previous fixed-role profile, which otherwise overrides discovery.
        profile["modelDiscoveryEnabled"] = json!(false);
        profile["inferenceModels"] = json!([{"name":"claude-sonnet-aag"}]);
        config::private_fs::atomic_write(&profile_path, &serde_json::to_vec(&profile).unwrap())
            .unwrap();
        write_gateway_profile(&db, &mirror).unwrap();
        assert_eq!(
            read_object(&profile_path).unwrap()["customUserPreference"],
            "preserved"
        );
        let upgraded = read_object(&profile_path).unwrap();
        assert_eq!(upgraded["modelDiscoveryEnabled"], true);
        assert!(upgraded.get("inferenceModels").is_none());
        assert!(!home.root.join(".claude").exists());
        config::private_fs::verify_private_file(&profile_path).unwrap();
    }
}
