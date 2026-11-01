//! Metadata-only native client launch contract for the protected authority.
//!
//! A client home is never an authority home. This module neither reads native
//! credentials nor copies history. Protected installation verification is a
//! separate owner boundary; settings and marker presence can only require the
//! managed path, never certify an installation.

use crate::{config, database::Database};
use std::{
    fs,
    path::{Component, Path, PathBuf},
};

pub(crate) const KEY_MANAGED_CLIENTS_ENABLED: &str = "codex_session_authority_enabled";
pub(crate) const KEY_ENV: &str = "AAG_SESSION_API_KEY";
pub(crate) const URL_ENV: &str = "AAG_SESSION_BROKER_URL";
const MARKER_FILENAME: &str = "aag_client_metadata.json";

/// Only the session-authority owner module can mint this after verifying the
/// service principal, committed installation and protected pinned images.
pub(crate) struct VerifiedClientAuthority {
    pub(super) root: PathBuf,
    pub(super) protected_shim: PathBuf,
    pub(super) pinned_native: PathBuf,
}

/// No Debug/Serialize implementation: this value owns the active gateway key.
pub(crate) struct ManagedClientContract {
    root: PathBuf,
    shim: PathBuf,
    native: PathBuf,
    port: u16,
    api_key: String,
}

#[derive(Clone, Copy)]
pub(crate) enum ClientKind {
    Cli,
    Desktop,
}

/// A launch plan is consumed by the launcher; it must never cross IPC directly.
pub(crate) struct ClientLaunchPlan {
    pub(crate) home: PathBuf,
    pub(crate) program: PathBuf,
    pub(crate) arguments: Vec<String>,
    pub(crate) environment: Vec<(String, String)>,
}

/// Marker inspection is read-only and deliberately one-way. Any authority
/// object, including a malformed or linked one, prevents unmanaged fallback.
fn required_at(root: &Path, enabled: bool) -> Result<bool, String> {
    match fs::symlink_metadata(root.join(".authority")) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(enabled),
        Err(_) => Err("session_client_authority_inspection_failed".into()),
    }
}

/// Presence can only close legacy mutation lanes. It never certifies that an
/// authority is installed or allows a managed client to start.
pub(crate) fn authority_object_present() -> Result<bool, String> {
    required_at(&config::get_home_dir().join(".codex-cli"), false)
}

pub(crate) fn managed_launch_required(db: &Database) -> Result<bool, String> {
    let enabled = db
        .get_setting(KEY_MANAGED_CLIENTS_ENABLED)
        .map_err(String::from)?;
    required_at(
        &config::get_home_dir().join(".codex-cli"),
        enabled.as_deref() == Some("1"),
    )
}

/// Capture active runtime credentials only after the protected installation
/// owner validates its own committed state. Saved listener/auth settings are
/// deliberately absent from this interface.
pub(crate) fn get_managed_contract(
    db: &Database,
    actual_port: u16,
    actual_key: String,
) -> Result<Option<ManagedClientContract>, String> {
    if !managed_launch_required(db)? {
        return Ok(None);
    }
    let root = config::get_home_dir().join(".codex-cli");
    let authority = super::owner_client::verify_client_authority(&root)?;
    ManagedClientContract::from_verified_authority(authority, actual_port, actual_key).map(Some)
}

/// Admission remains closed until the owner supplies a protected installation
/// attestation. Neither a user-writable setting nor an unverified file can mint
/// a ManagedClientContract. The pending Windows qualification does not enable it.
pub(crate) fn require_contract<'a>(
    db: &Database,
    contract: Option<&'a ManagedClientContract>,
) -> Result<Option<&'a ManagedClientContract>, String> {
    if contract.is_some() || !managed_launch_required(db)? {
        return Ok(contract);
    }
    Err("session_client_authority_unavailable".into())
}

impl ManagedClientContract {
    pub(crate) fn from_verified_authority(
        authority: VerifiedClientAuthority,
        port: u16,
        api_key: String,
    ) -> Result<Self, String> {
        if !config::same_dir(&authority.root, &config::get_home_dir().join(".codex-cli")) {
            return Err("session_client_authority_root_invalid".into());
        }
        Self::checked(
            authority.root,
            authority.protected_shim,
            authority.pinned_native,
            port,
            api_key,
        )
    }

    fn checked(
        root: PathBuf,
        shim: PathBuf,
        native: PathBuf,
        port: u16,
        api_key: String,
    ) -> Result<Self, String> {
        if port == 0
            || api_key.trim().is_empty()
            || api_key.len() > 8192
            || api_key.chars().any(char::is_control)
        {
            return Err("session_client_active_gateway_invalid".into());
        }
        for path in [&root, &shim, &native] {
            if !path.is_absolute()
                || path
                    .components()
                    .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
                || path.to_string_lossy().chars().any(char::is_control)
            {
                return Err("session_client_path_invalid".into());
            }
        }
        if config::same_dir(&root, &config::get_codex_config_dir()) {
            return Err("session_client_native_home_forbidden".into());
        }
        Ok(Self {
            root,
            shim,
            native,
            port,
            api_key,
        })
    }

    pub(crate) fn plan(&self, client: ClientKind) -> ClientLaunchPlan {
        let name = match client {
            ClientKind::Cli => "cli",
            ClientKind::Desktop => "desktop",
        };
        let home = self.root.join("clients").join(name);
        let url = format!("ws://127.0.0.1:{}/aag/codex", self.port);
        let mut environment = vec![
            ("CODEX_HOME".into(), home.to_string_lossy().into_owned()),
            (URL_ENV.into(), url.clone()),
            (KEY_ENV.into(), self.api_key.clone()),
            (
                "AAG_NATIVE_CODEX_EXE".into(),
                self.native.to_string_lossy().into_owned(),
            ),
            ("NO_PROXY".into(), "localhost,127.0.0.1,::1".into()),
        ];
        let (program, arguments) = match client {
            ClientKind::Cli => (
                self.native.clone(),
                vec![
                    "--remote".into(),
                    url,
                    "--remote-auth-token-env".into(),
                    KEY_ENV.into(),
                    "-c".into(),
                    "history.persistence=\"none\"".into(),
                ],
            ),
            ClientKind::Desktop => {
                environment.push((
                    "CODEX_CLI_PATH".into(),
                    self.shim.to_string_lossy().into_owned(),
                ));
                (self.shim.clone(), Vec::new())
            }
        };
        ClientLaunchPlan {
            home,
            program,
            arguments,
            environment,
        }
    }

    pub(crate) fn validate_launch_files(&self) -> Result<(), String> {
        for path in [&self.shim, &self.native] {
            config::private_fs::validate_external_file_path(path)
                .map_err(|_| "session_client_protected_executable_invalid".to_string())?;
            if !path.is_file() {
                return Err("session_client_protected_executable_unavailable".into());
            }
        }
        Ok(())
    }

    /// Called only from an explicit, already-admitted launch. Models are public
    /// producer metadata; no source home, auth file, DB or history is read here.
    pub(crate) fn prepare_metadata(
        &self,
        db: &Database,
        client: ClientKind,
        models: &[String],
    ) -> Result<ClientLaunchPlan, String> {
        let plan = self.plan(client);
        validate_metadata_home(&plan.home, &self.root)?;
        if models.is_empty() {
            return Err("desktop_mirror_models_unavailable".into());
        }
        config::private_fs::prepare_private_dir(&plan.home).map_err(String::from)?;
        let (ids, catalog) = crate::services::launcher::codex_catalog::build_codex_model_catalog(
            db,
            None,
            Some(models),
        )
        .map_err(String::from)?;
        if ids.is_empty() {
            return Err("desktop_mirror_models_unavailable".into());
        }
        let model = crate::services::launcher::codex_catalog::default_model(db, &ids, None)
            .map_err(String::from)?;
        let text = metadata_config(&plan.home, self.port, &model)?;
        config::private_fs::atomic_write(
            &plan
                .home
                .join(crate::services::launcher::codex_catalog::CATALOG_FILENAME),
            &catalog,
        )
        .map_err(String::from)?;
        config::private_fs::atomic_write(&plan.home.join("config.toml"), text.as_bytes())
            .map_err(String::from)?;
        config::private_fs::atomic_write(
            &plan.home.join(MARKER_FILENAME),
            b"{\"schema_version\":1,\"metadata_only\":true}\n",
        )
        .map_err(String::from)?;
        validate_metadata_home(&plan.home, &self.root)?;
        Ok(plan)
    }
}

fn validate_metadata_home(home: &Path, root: &Path) -> Result<(), String> {
    if ![root.join("clients/cli"), root.join("clients/desktop")]
        .iter()
        .any(|path| config::same_dir(home, path))
        || config::same_dir(home, &config::get_codex_config_dir())
    {
        return Err("session_client_metadata_home_invalid".into());
    }
    config::private_fs::validate_external_file_path(&home.join(MARKER_FILENAME))
        .map_err(String::from)?;
    if home.join(MARKER_FILENAME).exists() {
        let marker = config::private_fs::read_mirror_file(&home.join(MARKER_FILENAME))
            .map_err(String::from)?;
        let document: serde_json::Value =
            serde_json::from_slice(marker.contents.as_deref().unwrap_or_default())
                .map_err(|_| "session_client_metadata_marker_invalid".to_string())?;
        if document
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            != Some(1)
            || document
                .get("metadata_only")
                .and_then(serde_json::Value::as_bool)
                != Some(true)
        {
            return Err("session_client_metadata_marker_invalid".into());
        }
    }
    for name in [
        "sessions",
        "archived_sessions",
        "auth.json",
        "history.jsonl",
        "attachments",
    ] {
        match fs::symlink_metadata(home.join(name)) {
            Ok(_) => return Err("session_client_payload_storage_forbidden".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("session_client_payload_inspection_failed".into()),
        }
    }
    match fs::read_dir(home) {
        Ok(entries) => {
            for entry in entries {
                let entry =
                    entry.map_err(|_| "session_client_payload_inspection_failed".to_string())?;
                let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                if (name.starts_with("state_") && name.contains(".sqlite"))
                    || name.starts_with("history.")
                {
                    return Err("session_client_payload_storage_forbidden".into());
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err("session_client_payload_inspection_failed".into()),
    }
    Ok(())
}

fn metadata_config(home: &Path, port: u16, model: &str) -> Result<String, String> {
    let mut document = toml_edit::DocumentMut::new();
    document["model"] = toml_edit::value(model);
    document["model_provider"] = toml_edit::value("aag");
    document["sqlite_home"] =
        toml_edit::value(home.join("startup_index").to_string_lossy().as_ref());
    document["model_catalog_json"] = toml_edit::value(
        home.join(crate::services::launcher::codex_catalog::CATALOG_FILENAME)
            .to_string_lossy()
            .as_ref(),
    );
    document["history"]["persistence"] = toml_edit::value("none");
    document["model_providers"]["aag"]["name"] = toml_edit::value("AAG");
    document["model_providers"]["aag"]["base_url"] =
        toml_edit::value(format!("http://127.0.0.1:{port}/v1"));
    document["model_providers"]["aag"]["wire_api"] = toml_edit::value("responses");
    document["model_providers"]["aag"]["env_key"] = toml_edit::value(KEY_ENV);
    document["model_providers"]["aag"]["requires_openai_auth"] = toml_edit::value(false);
    Ok(document.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_mode_requires_authority_and_marker_cannot_enable_native_fallback() {
        let fixture = crate::services::provider::test_support::isolated_home("managed-required");
        let db = Database::memory().unwrap();
        assert!(require_contract(&db, None).unwrap().is_none());
        db.set_setting(KEY_MANAGED_CLIENTS_ENABLED, "1").unwrap();
        assert_eq!(
            require_contract(&db, None).err().unwrap(),
            "session_client_authority_unavailable"
        );
        fs::create_dir_all(fixture.root.join(".codex-cli/.authority")).unwrap();
        db.set_setting(KEY_MANAGED_CLIENTS_ENABLED, "0").unwrap();
        assert!(managed_launch_required(&db).unwrap());
        assert_eq!(
            require_contract(&db, None).err().unwrap(),
            "session_client_authority_unavailable"
        );
    }

    #[test]
    fn plans_use_one_active_endpoint_key_and_private_metadata_homes() {
        let fixture = crate::services::provider::test_support::isolated_home("managed-plans");
        let contract = ManagedClientContract::checked(
            fixture.root.join(".codex-cli"),
            fixture.root.join("shim.exe"),
            fixture.root.join("codex.exe"),
            43219,
            "fixture_actual_key".into(),
        )
        .unwrap();
        let cli = contract.plan(ClientKind::Cli);
        let desktop = contract.plan(ClientKind::Desktop);
        assert_eq!(cli.program, contract.native);
        assert_eq!(
            cli.arguments,
            [
                "--remote",
                "ws://127.0.0.1:43219/aag/codex",
                "--remote-auth-token-env",
                KEY_ENV,
                "-c",
                "history.persistence=\"none\""
            ]
        );
        assert_eq!(cli.home, contract.root.join("clients/cli"));
        assert_eq!(desktop.home, contract.root.join("clients/desktop"));
        for plan in [&cli, &desktop] {
            assert!(plan
                .environment
                .contains(&(KEY_ENV.into(), "fixture_actual_key".into())));
            assert!(plan
                .environment
                .contains(&(URL_ENV.into(), "ws://127.0.0.1:43219/aag/codex".into())));
        }
        let text = metadata_config(&desktop.home, contract.port, "fixture_model").unwrap();
        let document: toml_edit::DocumentMut = text.parse().unwrap();
        assert_eq!(document["history"]["persistence"].as_str(), Some("none"));
        assert_eq!(
            document["model_providers"]["aag"]["requires_openai_auth"].as_bool(),
            Some(false)
        );
        assert!(text.contains(KEY_ENV));
        assert!(!text.contains("fixture_actual_key"));
        assert!(!text.contains("experimental_bearer_token"));
    }

    #[test]
    fn metadata_preparation_rejects_native_home_and_payload_before_writes() {
        let fixture =
            crate::services::provider::test_support::isolated_home("managed-payload-guard");
        let root = fixture.root.join(".codex-cli");
        let home = root.join("clients/desktop");
        fs::create_dir_all(home.join("sessions")).unwrap();
        assert_eq!(
            validate_metadata_home(&home, &root).unwrap_err(),
            "session_client_payload_storage_forbidden"
        );
        assert!(!home.join("config.toml").exists());
        assert!(validate_metadata_home(&config::get_codex_config_dir(), &root).is_err());
    }

    #[test]
    fn metadata_preparation_never_copies_native_credentials_or_conversation_state() {
        let fixture =
            crate::services::provider::test_support::isolated_home("managed-metadata-only");
        let db = Database::memory().unwrap();
        let provider = serde_json::from_value(serde_json::json!({
            "id":"fixture", "name":"Fixture", "type":"openai",
            "baseURL":"https://example.invalid/v1", "apiKey":"fixture_upstream_secret",
            "models":["fixture_model"], "enabled":true
        }))
        .unwrap();
        db.save_proxy_provider(&provider).unwrap();
        let native = config::get_codex_config_dir();
        config::private_fs::prepare_private_dir(&native).unwrap();
        config::private_fs::atomic_write(&native.join("auth.json"), b"fixture_native_secret")
            .unwrap();
        config::private_fs::prepare_private_dir(&native.join("sessions")).unwrap();
        config::private_fs::atomic_write(
            &native.join("sessions/fixture.jsonl"),
            b"fixture_payload",
        )
        .unwrap();
        let contract = ManagedClientContract::checked(
            fixture.root.join(".codex-cli"),
            fixture.root.join("shim.exe"),
            fixture.root.join("codex.exe"),
            43219,
            "fixture_actual_key".into(),
        )
        .unwrap();
        let plan = contract
            .prepare_metadata(&db, ClientKind::Desktop, &["fixture_model".into()])
            .unwrap();
        let names: Vec<_> = fs::read_dir(&plan.home)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names.len(),
            3,
            "only config, model catalogue and metadata marker are created"
        );
        for entry in fs::read_dir(&plan.home).unwrap() {
            let bytes = fs::read(entry.unwrap().path()).unwrap();
            let text = String::from_utf8(bytes).unwrap();
            for secret in [
                "fixture_native_secret",
                "fixture_payload",
                "fixture_actual_key",
                "fixture_upstream_secret",
            ] {
                assert!(!text.contains(secret));
            }
        }
        let marker: serde_json::Value =
            serde_json::from_slice(&fs::read(plan.home.join(MARKER_FILENAME)).unwrap()).unwrap();
        assert_eq!(
            marker,
            serde_json::json!({"schema_version":1,"metadata_only":true})
        );
        assert_eq!(
            fs::read(native.join("auth.json")).unwrap(),
            b"fixture_native_secret"
        );
        assert_eq!(
            fs::read(native.join("sessions/fixture.jsonl")).unwrap(),
            b"fixture_payload"
        );
    }
}
