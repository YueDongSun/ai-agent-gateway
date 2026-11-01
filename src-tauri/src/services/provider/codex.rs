//! Codex adapter.
//!
//! Codex stores config at `~/.codex/config.toml` (TOML) and `~/.codex/auth.json`
//! (JSON). The provider's `settings_config` is a JSON object with shape:
//! `{ "auth": <auth.json content>, "config": "<TOML string>" }`.
//! Mirrors cc-switch `services/provider/live.rs:1012`.
//!
//! When the `codex_cli_home` setting is configured (independent CODEX_HOME),
//! writes go to that directory instead of the default `~/.codex`, isolating
//! the Codex CLI from the Codex desktop app's shared config.

use crate::config::{
    backup_file, get_backups_dir, get_codex_auth_path, get_codex_config_path, read_json_file,
    read_text_file, write_json_file, write_text_file,
};
use crate::error::AppError;
use crate::provider::Provider;
use crate::services::provider::toml_merge::{Toml_table, Toml_top_entry, Toml_value};
use crate::services::provider::LiveWriteResult;
use serde_json::Value;

/// Database-aware entrypoints also reject the unavailable enabled/uninstalled
/// state. Low-level compatibility writers can inspect only the authority object;
/// they must not open an ambient database to infer somebody else's settings.
pub(crate) fn require_unmanaged_codex(db: &crate::database::Database) -> Result<(), AppError> {
    if crate::services::session_authority::client_config::managed_launch_required(db)
        .map_err(AppError::Config)?
    {
        return Err(AppError::SessionClientAuthorityUnavailable);
    }
    Ok(())
}

fn require_legacy_codex_write() -> Result<(), AppError> {
    if crate::services::session_authority::client_config::authority_object_present()
        .map_err(AppError::Config)?
    {
        return Err(AppError::SessionClientAuthorityUnavailable);
    }
    Ok(())
}

/// Is Codex installed? (config dir exists)
#[allow(dead_code)]
pub fn detect() -> bool {
    get_codex_config_path().exists() || get_codex_auth_path().exists()
}

/// Read the current live config as `{config: <toml string>, auth: <auth.json value>}`,
/// or None if neither file exists.
#[allow(dead_code)]
pub fn read_live() -> Result<Option<Value>, AppError> {
    let config_path = get_codex_config_path();
    let auth_path = get_codex_auth_path();
    let config = read_text_file(&config_path)?;
    let auth = read_json_file(&auth_path)?;
    if config.is_none() && auth.is_none() {
        return Ok(None);
    }
    Ok(Some(serde_json::json!({
        "config": config.unwrap_or_default(),
        "auth": auth.unwrap_or(Value::Null),
    })))
}

/// Write `provider.settings_config` to the Codex mirror's `config.toml` +
/// `auth.json`, backing up existing files first. `settings_config` must be an
/// object with `auth` (any JSON) and `config` (TOML string) fields.
///
/// `codex_cli_home` optionally overrides the Codex config directory (isolated
/// CODEX_HOME); `None` uses the `~/.codex-cli` mirror default. The resolved
/// directory is never the native `~/.codex` — an override that points there
/// is refused (see the guard in `write_live`).
///
/// Retained for the direct cc-switch-style write path (tests / history /
/// `write_live_snapshot`). The live `provider_switch` flow no longer calls
/// this: a switch writes the mirror-proxy contract instead (see
/// `write_mirror_proxy_files`), so the mirror never carries login info.
pub fn write_live(
    provider: &Provider,
    codex_cli_home: Option<&str>,
) -> Result<LiveWriteResult, AppError> {
    require_legacy_codex_write()?;
    // Resolve the mirror CODEX_HOME. `get_codex_mirror_dir` falls back to
    // `~/.codex-cli` when no override is set (never the native `~/.codex`),
    // but a user could still point the override directly at `~/.codex`.
    // Refuse that case so a codex live write can never mutate the Codex
    // Desktop app's shared config — and thereby clobber the user's ChatGPT
    // login state in the native `~/.codex/auth.json`.
    let mirror_dir = crate::config::get_codex_mirror_dir(codex_cli_home);
    if mirror_dir == crate::config::get_codex_config_dir() {
        return Err(AppError::Config(
            "codex_cli_home must not resolve to the native ~/.codex: refusing to write the Codex Desktop app's shared config".into(),
        ));
    }
    let obj = provider.settings_config.as_object().ok_or_else(|| {
        AppError::Config("Codex provider settings_config must be a JSON object".into())
    })?;
    let auth = obj
        .get("auth")
        .ok_or_else(|| AppError::Config("Codex settings_config missing 'auth' field".into()))?;
    let config_str = obj.get("config").and_then(|v| v.as_str()).ok_or_else(|| {
        AppError::Config("Codex settings_config missing 'config' string field".into())
    })?;

    let config_path = mirror_dir.join("config.toml");
    let auth_path = mirror_dir.join("auth.json");
    let ts = chrono::Utc::now().timestamp();
    let backup_dir = get_backups_dir()?;

    let mut written = Vec::new();
    let mut backups = Vec::new();

    // Backup + write config.toml.
    if config_path.exists() {
        let bak = backup_dir.join(format!("config.toml.{ts}.bak"));
        backup_file(&config_path, &bak)?;
        backups.push(bak.to_string_lossy().into_owned());
    }
    write_text_file(&config_path, config_str)?;
    written.push(config_path.to_string_lossy().into_owned());

    // Backup + write auth.json.
    // NOTE: this writes whatever credentials the provider row stores. Only
    // the retained direct-style write path uses it — the live
    // `provider_switch` flow writes the credential-free mirror-proxy
    // contract via `write_mirror_proxy_files` instead.
    if auth_path.exists() {
        let bak = backup_dir.join(format!("auth.json.{ts}.bak"));
        backup_file(&auth_path, &bak)?;
        backups.push(bak.to_string_lossy().into_owned());
    }
    write_json_file(&auth_path, auth)?;
    written.push(auth_path.to_string_lossy().into_owned());

    Ok(LiveWriteResult {
        app_type: "codex".into(),
        provider_id: provider.id.clone(),
        written,
        backup: backups.into_iter().next(),
    })
}

/// Login-state files in a CODEX_HOME that keep a Codex mirror stuck
/// on a dead login even after `auth.json` is emptied: the desktop's
/// global-state cache at the CODEX_HOME root (outside the Chromium profile
/// the launcher uses). Conversation, history and attachment directories must
/// survive recovery, including native versions storing payloads under cache/.
const MIRROR_SESSION_STATE_FILES: &[&str] =
    &[".codex-global-state.json", ".codex-global-state.json.bak"];

fn remove_mirror_path(mirror_dir: &std::path::Path, name: &str) -> Result<(), AppError> {
    let path = mirror_dir.join(name);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(_) => return Ok(()), // already absent
    };
    if metadata.file_type().is_symlink() {
        return Err(AppError::Config(format!(
            "refusing to remove {}: target is a symlink or reparse point",
            name
        )));
    }
    if path.is_dir() {
        std::fs::remove_dir_all(&path).map_err(|e| AppError::io(&path, e))?;
    } else {
        std::fs::remove_file(&path).map_err(|e| AppError::io(&path, e))?;
    }
    Ok(())
}

/// Reset login state of a mirror CODEX_HOME: `auth.json`
/// becomes `{}` and the known global login-state files from a
/// previous login are removed. Native conversations and attachment storage are
/// preserved. This clears a dead login ("access token could not be
/// refreshed") instead of showing a clean login page. The fresh-mirror
/// path (open-desktop) calls this; the switch path never does — a login
/// session the user created inside the mirror must survive switches.
fn reset_mirror_session_state(mirror_dir: &std::path::Path) -> Result<(), AppError> {
    require_legacy_codex_write()?;
    // Same atomic + linked-path discipline as every other mirror write: a
    // crash mid-write must not leave a truncated auth.json behind. The
    // mirror is AAG-managed, so harden it first — the switch path applies
    // the same policy, and an auto-heal can run on a directory Codex
    // Desktop recreated with inherited loose ACLs.
    crate::config::private_fs::prepare_managed_private_dir(mirror_dir)?;
    let auth_path = mirror_dir.join("auth.json");
    crate::config::private_fs::validate_external_file_path(&auth_path)?;
    crate::config::private_fs::atomic_write(&auth_path, b"{}")?;
    for name in MIRROR_SESSION_STATE_FILES {
        remove_mirror_path(mirror_dir, name)?;
    }
    Ok(())
}

/// Classify the mirror's `auth.json` by SHAPE only — the contents are
/// never read by callers. `empty` = logged out ({} / null / missing);
/// `credentials_present` = something still stores a session there (older
/// AAG versions copied provider logins into this file).
pub(crate) fn classify_mirror_auth_json(mirror_dir: &std::path::Path) -> &'static str {
    match std::fs::read_to_string(mirror_dir.join("auth.json")) {
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(ref object)) if object.is_empty() => "empty",
            Ok(Value::Null) => "empty",
            Ok(_) => "credentials_present",
            Err(_) => "credentials_present",
        },
        Err(_) => "missing",
    }
}

/// Escape a value for a TOML basic string literal (the only quoting the
/// mirror config needs).
///
/// Retained for the mirror's manual-config read paths; the write path now
/// goes through `toml_merge`, which spells values with `toml_edit` and so
/// escapes control characters this function would have written raw.
#[allow(dead_code)]
pub(crate) fn escape_toml_basic_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// How the mirror's `config.toml` exposes AAG to Codex:
///
/// - `Exclusive` (default, historic): the top-level `model_provider` is
///   forced to `aag` — every request routes through AAG's proxy, and a
///   ChatGPT login made inside the mirror sits unused.
/// - `Coexist`: no top-level provider/model is written, so Codex defaults
///   to its builtin `openai` provider — a ChatGPT login works for official
///   models out of the box — while `[model_providers.aag]` stays declared
///   and a `[profiles.aag]` profile points at it. The user manually
///   switches to AAG in the Codex model/provider picker when they want
///   third-party models.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(non_camel_case_types)]
pub enum Codex_mirror_mode {
    Exclusive,
    Coexist,
}

impl Codex_mirror_mode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Codex_mirror_mode::Exclusive => "exclusive",
            Codex_mirror_mode::Coexist => "coexist",
        }
    }

    /// Parse the persisted setting. Unknown/absent values fail safe to the
    /// historic exclusive behavior.
    pub fn from_setting(value: Option<&str>) -> Self {
        if value == Some("coexist") {
            Codex_mirror_mode::Coexist
        } else {
            Codex_mirror_mode::Exclusive
        }
    }
}

/// Read the persisted mirror mode (`KEY_CODEX_MIRROR_MODE`; absent =
/// exclusive).
pub(crate) fn codex_mirror_mode_from_db(db: &crate::database::Database) -> Codex_mirror_mode {
    Codex_mirror_mode::from_setting(
        db.get_setting(crate::database::dao::settings::KEY_CODEX_MIRROR_MODE)
            .ok()
            .flatten()
            .as_deref(),
    )
}

/// The mirror CODEX_HOME contract, in one place:
///
/// - `config.toml` points exclusively at AAG's proxy (`wire_api =
///   "responses"`, `requires_openai_auth = false`, AAG's own
///   `experimental_bearer_token`) — Codex authenticates to AAG only, and
///   AAG holds the upstream keys.
/// - `auth.json` is NEVER written by AAG with anything but `{}`. AAG never
///   puts login information into the mirror: not the native `~/.codex`
///   ChatGPT login, and not the provider row's stored credentials. With
///   `purge_auth = false` (the provider-switch path) auth.json is not
///   touched at all, so a login session the user created INSIDE the mirror
///   survives switches and desktop launches. With `purge_auth = true`
///   (an explicit reset request) a stale auth.json —
///   including credential copies from older AAG versions — is purged
///   unconditionally, deliberately without a backup: the mirror must start
///   as a fresh, standalone, logged-out instance.
/// - `config.toml` is backed up before overwrite (it may hold a model setup
///   the user wants recoverable).
///
/// The mirror is an isolated AAG directory, hardened before any user
/// configuration is read or changed.
///
/// Called from both provider switching and desktop launch with
/// `purge_auth = false`, so every mirror write
/// path enforces the same proxy contract and the same never-write-credentials rule.
pub fn write_mirror_proxy_files(
    codex_cli_home: Option<&str>,
    proxy_base_url: &str,
    proxy_auth_token: &str,
    model: &str,
    mode: Codex_mirror_mode,
    purge_auth: bool,
) -> Result<LiveWriteResult, AppError> {
    write_mirror_proxy_files_with_effort(
        codex_cli_home,
        proxy_base_url,
        proxy_auth_token,
        model,
        mode,
        purge_auth,
        None,
        false,
    )
}

fn write_mirror_proxy_files_with_effort(
    codex_cli_home: Option<&str>,
    proxy_base_url: &str,
    proxy_auth_token: &str,
    model: &str,
    mode: Codex_mirror_mode,
    purge_auth: bool,
    reasoning_effort: Option<&str>,
    manage_reasoning_effort: bool,
) -> Result<LiveWriteResult, AppError> {
    require_legacy_codex_write()?;
    if reasoning_effort
        .is_some_and(|value| !crate::services::launcher::codex_reasoning::valid_effort(value))
    {
        return Err(AppError::Config("codex_reasoning_effort_invalid".into()));
    }
    let mirror_dir = crate::config::get_codex_mirror_dir(codex_cli_home);
    if crate::config::same_dir(&mirror_dir, &crate::config::get_codex_config_dir()) {
        return Err(AppError::Config(
            "codex_cli_home must not resolve to the native ~/.codex: refusing to write the Codex Desktop app's shared config".into(),
        ));
    }
    crate::config::private_fs::prepare_managed_private_dir(&mirror_dir)?;

    let config_path = mirror_dir.join("config.toml");
    let auth_path = mirror_dir.join("auth.json");
    for path in [&config_path, &auth_path] {
        crate::config::private_fs::validate_external_file_path(path)?;
    }
    // A normal launch/catalog refresh must retain a choice saved by Codex's
    // own model picker. Only an explicit manager switch/save owns that value;
    // a fresh mirror can still inherit the stored provider preference.
    let previous = crate::config::private_fs::read_mirror_file(&config_path)?;
    let existing = previous.contents.as_deref().unwrap_or_default();
    let previous_text = std::str::from_utf8(existing)
        .map_err(|_| AppError::Config("client_config_toml_not_utf8".into()))?;
    let previous_document = previous_text
        .trim_start_matches('\u{feff}')
        .parse::<toml_edit::DocumentMut>()
        .map_err(|_| AppError::Config("client_config_toml_invalid".into()))?;
    let saved_effort = previous_document.get("model_reasoning_effort").is_some()
        || previous_document
            .get("profiles")
            .and_then(|profiles| profiles.get("aag"))
            .and_then(|profile| profile.get("model_reasoning_effort"))
            .is_some();
    let reasoning_effort = if manage_reasoning_effort || !saved_effort {
        reasoning_effort
    } else {
        None
    };

    let mirror_config = render_mirror_proxy_config(
        &mirror_dir,
        existing,
        proxy_base_url,
        proxy_auth_token,
        model,
        mode,
        reasoning_effort,
        manage_reasoning_effort,
    )?;
    let (mut written, backups) = super::config_writes::write_mirror_files(&[
        super::config_writes::MirrorConfigFileUpdate {
            path: &config_path,
            contents: &mirror_config,
            app_type: "codex",
            previous: &previous,
        },
    ])?;

    if purge_auth {
        // Explicit reset clears mirror login and known global login-state
        // markers while preserving conversation, history, and attachment data.
        reset_mirror_session_state_guarded(codex_cli_home)?;
        written.push(auth_path.to_string_lossy().into_owned());
    }

    Ok(LiveWriteResult {
        app_type: "codex".into(),
        provider_id: String::new(),
        written,
        backup: backups.into_iter().next(),
    })
}

fn render_mirror_proxy_config(
    mirror_dir: &std::path::Path,
    existing: &[u8],
    proxy_base_url: &str,
    proxy_auth_token: &str,
    model: &str,
    mode: Codex_mirror_mode,
    reasoning_effort: Option<&str>,
    manage_reasoning_effort: bool,
) -> Result<Vec<u8>, AppError> {
    // The managed provider block, merged into whatever the file already
    // holds rather than rendered over it: the mirror's `config.toml` is a
    // user-owned file, and the previous full-template write destroyed every
    // comment and unrelated key in it on each provider switch. `toml_merge`
    // edits only the keys below and leaves the rest byte-identical.
    let provider_entries = vec![
        ("name".to_string(), Toml_value::Str("AAG Gateway".into())),
        (
            "base_url".to_string(),
            Toml_value::Str(proxy_base_url.to_string()),
        ),
        ("wire_api".to_string(), Toml_value::Str("responses".into())),
        ("requires_openai_auth".to_string(), Toml_value::Bool(false)),
        (
            "experimental_bearer_token".to_string(),
            Toml_value::Str(proxy_auth_token.to_string()),
        ),
    ];

    let catalog_path = mirror_dir
        .join(crate::services::launcher::codex_catalog::CATALOG_FILENAME)
        .to_string_lossy()
        .into_owned();
    let mut profile_entries = vec![
        ("model_provider".to_string(), Toml_value::Str("aag".into())),
        // Hosted Responses search is unavailable on translated AAG routes.
        // Coexist mode applies this only inside the managed AAG profile.
        ("web_search".to_string(), Toml_value::Str("disabled".into())),
        (
            "model_catalog_json".to_string(),
            Toml_value::Str(catalog_path),
        ),
    ];
    if !model.trim().is_empty() {
        profile_entries.push(("model".to_string(), Toml_value::Str(model.to_string())));
    }
    if let Some(effort) = reasoning_effort {
        profile_entries.push((
            "model_reasoning_effort".into(),
            Toml_value::Str(effort.into()),
        ));
    }
    let top_keys: Vec<Toml_top_entry> = match mode {
        Codex_mirror_mode::Exclusive => profile_entries.clone(),
        Codex_mirror_mode::Coexist => Vec::new(),
    };
    let tables: Vec<Toml_table> = vec![
        ("model_providers.aag".to_string(), provider_entries),
        ("profiles.aag".to_string(), profile_entries),
    ];

    // Merge the exact previous bytes so unrelated keys and comments survive.
    let mirror_config = super::toml_merge::merge(existing, &top_keys, &tables)?;
    clear_inactive_mirror_selection(
        mirror_config,
        existing,
        model,
        mode,
        manage_reasoning_effort && reasoning_effort.is_none(),
    )
}

/// Keep launcher/native-home comparisons on the shared guarded path policy.
pub(crate) fn same_codex_dir(a: &std::path::Path, b: &std::path::Path) -> bool {
    crate::config::same_dir(a, b)
}

/// Remove stale managed selection and the legacy top-level profile selector.
/// Current Codex rejects that legacy key and silently loads default settings;
/// named profile tables and unrelated settings remain intact for older clients.
fn clear_inactive_mirror_selection(
    bytes: Vec<u8>,
    previous: &[u8],
    model: &str,
    mode: Codex_mirror_mode,
    clear_reasoning_effort: bool,
) -> Result<Vec<u8>, AppError> {
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| AppError::Config("client_config_toml_not_utf8".into()))?;
    let (bom, body) = text
        .strip_prefix('\u{feff}')
        .map_or(("", text), |body| ("\u{feff}", body));
    let mut document = body
        .parse::<toml_edit::DocumentMut>()
        .map_err(|_| AppError::Config("client_config_toml_invalid".into()))?;
    if clear_reasoning_effort {
        if let Some(profile) = document
            .get_mut("profiles")
            .and_then(|profiles| profiles.get_mut("aag"))
            .and_then(toml_edit::Item::as_table_mut)
        {
            profile.remove("model_reasoning_effort");
        }
        if mode == Codex_mirror_mode::Exclusive
            && document
                .get("model_provider")
                .and_then(toml_edit::Item::as_str)
                == Some("aag")
        {
            document.remove("model_reasoning_effort");
        }
    }
    document.remove("profile");
    if model.trim().is_empty() || mode == Codex_mirror_mode::Coexist {
        document.remove("model");
    }
    if mode == Codex_mirror_mode::Coexist {
        document.remove("model_provider");
        document.remove("model_catalog_json");
        // Remove our exclusive-mode search override when returning to native
        // defaults. A standalone user search preference is not ours to clear.
        let previous_text = std::str::from_utf8(previous)
            .map_err(|_| AppError::Config("client_config_toml_not_utf8".into()))?;
        let previous_document = previous_text
            .trim_start_matches('\u{feff}')
            .parse::<toml_edit::DocumentMut>()
            .map_err(|_| AppError::Config("client_config_toml_invalid".into()))?;
        let previous_aag_search = previous_document
            .get("profiles")
            .and_then(|profiles| profiles.get("aag"))
            .and_then(|profile| profile.get("web_search"))
            .and_then(toml_edit::Item::as_str);
        let previous_aag_effort = previous_document
            .get("profiles")
            .and_then(|profiles| profiles.get("aag"))
            .and_then(|profile| profile.get("model_reasoning_effort"))
            .and_then(toml_edit::Item::as_str);
        if previous_document
            .get("model_provider")
            .and_then(toml_edit::Item::as_str)
            == Some("aag")
            && previous_aag_effort.is_some()
            && previous_document
                .get("model_reasoning_effort")
                .and_then(toml_edit::Item::as_str)
                == previous_aag_effort
        {
            document.remove("model_reasoning_effort");
        }
        if previous_document
            .get("model_provider")
            .and_then(toml_edit::Item::as_str)
            == Some("aag")
            && previous_document
                .get("web_search")
                .and_then(toml_edit::Item::as_str)
                == Some("disabled")
            && previous_aag_search == Some("disabled")
        {
            document.remove("web_search");
        }
    }
    Ok(format!("{bom}{document}").into_bytes())
}

/// Explicitly reset the mirror's login/session state (auth.json + global
/// state + cache/sessions carriers). Invoked only on demand via the
/// `codex_mirror_session_reset` command — neither the switch path nor
/// open-desktop resets a login the user created inside the mirror.
pub(crate) fn reset_mirror_session_state_guarded(
    codex_cli_home: Option<&str>,
) -> Result<(), AppError> {
    require_legacy_codex_write()?;
    let mirror_dir = crate::config::get_codex_mirror_dir(codex_cli_home);
    if crate::config::same_dir(&mirror_dir, &crate::config::get_codex_config_dir()) {
        return Err(AppError::Config(
            "codex_cli_home must not resolve to the native ~/.codex: refusing to reset the Codex Desktop app's shared session state".into(),
        ));
    }
    if !mirror_dir.exists() {
        return Ok(());
    }
    crate::config::private_fs::prepare_managed_private_dir(&mirror_dir)?;
    reset_mirror_session_state(&mirror_dir)
}

// -----------------------------------------------------------------------------
// Mirror auto-heal (server-side hook for the upstream-4xx auth-expired path)
// -----------------------------------------------------------------------------
//
// When the proxy surfaces an upstream `AuthExpired` 4xx, the Codex mirror is
// almost certainly stuck on a dead cached session in `~/.codex-cli/auth.json`.
// Resetting that state restores the clean login page — without that, every
// request hits the same dead-refresh-token path and Codex Desktop keeps
// surfacing its "access token could not be refreshed" dialog.
//
// The trigger lives at the response boundary in `proxy::server`; this module
// owns the filesystem side-effect. Two safety rails prevent the heal from
// interfering with a healthy mirror session:
//
// 1. **Shape guard**: only heal when `auth.json` is `credentials_present` —
//    the mirror is logged in but stale. `empty` / `missing` means the user
//    already saw the login page; no heal needed.
//
// 2. **M-time guard**: only heal when `auth.json` is older than
//    `MIRROR_HEAL_MIN_AUTH_AGE_SECS`. A fresh `auth.json` (the user just
//    signed in inside the mirror) must NOT be wiped — that would defeat
//    `purge_auth = false` semantics in `provider_switch`.
//
// 3. **Process-level OnceLock**: at most one heal per process. A second
//    desktop launch in the same process reuses the gate, but never wipes a
//    freshly re-created login.

use std::sync::OnceLock;

/// Mirror `auth.json` must be at least this old (seconds) before auto-heal
/// will touch it. Conservative default: 60 s — long enough that any in-flight
/// login flow inside the mirror window has time to complete its first write.
const MIRROR_HEAL_MIN_AUTH_AGE_SECS: u64 = 60;

/// Per-process gate. Holds the canonical mirror-dir path on which auto-heal
/// has already fired. `None` means heal has not run yet.
static MIRROR_HEAL_FIRED: OnceLock<String> = OnceLock::new();

/// Outcome of an auto-heal probe. Public so callers (and tests) can branch on
/// it without re-reading the filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MirrorHealOutcome {
    /// `auth.json` shape was `empty` / `missing`, or m-time was too fresh —
    /// the user is likely mid-login. No filesystem mutation.
    SkippedHealthyOrFresh,
    /// A previous heal already ran for this mirror dir this process.
    SkippedAlreadyFired,
    /// The native `~/.codex` was targeted — refused for safety. Same guard
    /// as the manual `codex_mirror_session_reset` command.
    RefusedNativeTarget,
    /// A protected authority object exists or cannot be safely inspected.
    /// A legacy response-triggered repair has no authority to mutate it.
    RefusedManagedAuthority,
    /// Heal ran: `auth.json` and the session carriers were reset.
    Healed,
}

/// Probe and (at most once per process) heal the Codex mirror's dead-session
/// state. Safe to call from any request handler — the per-process gate and
/// m-time guard make it a no-op unless the mirror is genuinely stuck on a
/// dead login AND no fresh login has been written recently.
///
/// Returns the outcome so the caller can log it; never panics, never writes
/// outside the mirror dir, never touches the native `~/.codex`.
pub(crate) fn maybe_heal_mirror_on_auth_expired(codex_cli_home: Option<&str>) -> MirrorHealOutcome {
    if require_legacy_codex_write().is_err() {
        return MirrorHealOutcome::RefusedManagedAuthority;
    }
    let mirror_dir = crate::config::get_codex_mirror_dir(codex_cli_home);
    if crate::config::same_dir(&mirror_dir, &crate::config::get_codex_config_dir()) {
        return MirrorHealOutcome::RefusedNativeTarget;
    }

    let key = mirror_dir.to_string_lossy().into_owned();
    if MIRROR_HEAL_FIRED.get().map(String::as_str) == Some(key.as_str()) {
        return MirrorHealOutcome::SkippedAlreadyFired;
    }

    if !mirror_dir.exists() {
        return MirrorHealOutcome::SkippedHealthyOrFresh;
    }
    match classify_mirror_auth_json(&mirror_dir) {
        "credentials_present" => {}
        _ => return MirrorHealOutcome::SkippedHealthyOrFresh,
    }

    // M-time guard: refuse to wipe a fresh auth.json. The user may be
    // signing in inside the mirror right now.
    let auth_path = mirror_dir.join("auth.json");
    let age_ok = match std::fs::metadata(&auth_path).and_then(|meta| meta.modified()) {
        Ok(modified) => match std::time::SystemTime::now().duration_since(modified) {
            Ok(age) => age.as_secs() >= MIRROR_HEAL_MIN_AUTH_AGE_SECS,
            Err(_) => false,
        },
        Err(_) => false,
    };
    if !age_ok {
        return MirrorHealOutcome::SkippedHealthyOrFresh;
    }

    // First call for this mirror dir in this process: fire the heal and
    // claim the gate. If another thread races past the gate first,
    // `set()` returns Err — treat that as `SkippedAlreadyFired`.
    let claim = MIRROR_HEAL_FIRED.set(key);
    if claim.is_err() {
        return MirrorHealOutcome::SkippedAlreadyFired;
    }

    if let Err(error) = reset_mirror_session_state(&mirror_dir) {
        // The stable external message keeps the log path-hashed like the
        // mirror_dir reference; AppError's Display embeds raw local paths.
        log::warn!(
            "codex mirror auto-heal: reset failed mirror_dir={} error={}",
            crate::diagnostics::safe_reference(&mirror_dir.to_string_lossy()),
            error.external_message()
        );
        // The gate is already set; surface the failure but don't roll it
        // back. A second hit this process is harmless either way.
    } else {
        log::info!(
            "codex mirror auto-heal: stale auth.json detected, reset to clean login state mirror_dir={}",
            crate::diagnostics::safe_reference(&mirror_dir.to_string_lossy())
        );
    }
    MirrorHealOutcome::Healed
}

#[cfg(test)]
mod auto_heal_tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    #[test]
    fn managed_authority_blocks_legacy_autoheal_without_consuming_its_gate() {
        let home =
            crate::services::provider::test_support::isolated_home("managed-legacy-autoheal");
        let mirror = home.root.join("custom_mirror");
        crate::config::private_fs::prepare_private_dir(&mirror).unwrap();
        crate::config::private_fs::atomic_write(
            &mirror.join("auth.json"),
            b"{\"fixture_session\":true}",
        )
        .unwrap();
        filetime_set(
            &mirror.join("auth.json"),
            std::time::SystemTime::now()
                - std::time::Duration::from_secs(MIRROR_HEAL_MIN_AUTH_AGE_SECS + 60),
        );
        fs::create_dir_all(home.root.join(".codex-cli/.authority")).unwrap();
        let gate_before = MIRROR_HEAL_FIRED.get().cloned();
        let outcome = maybe_heal_mirror_on_auth_expired(mirror.to_str());
        assert!(!matches!(outcome, MirrorHealOutcome::Healed));
        assert_eq!(MIRROR_HEAL_FIRED.get().cloned(), gate_before);
        assert_eq!(
            fs::read(mirror.join("auth.json")).unwrap(),
            b"{\"fixture_session\":true}"
        );
    }

    /// Build a temp mirror dir that is NOT the native `~/.codex`. The
    /// test process does not need to mutate the real mirror; each test
    /// case uses its own sub-directory so the per-process OnceLock
    /// gates do not collide.
    fn fresh_mirror_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aag-mirror-heal-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // AAG never writes to the native `~/.codex`; the guard inside
        // `same_codex_dir` checks the resolved path. The temp dir will
        // not match the native path, so the heal runs.
        dir
    }

    #[test]
    fn heal_runs_when_credentials_are_present_and_old() {
        let mirror = fresh_mirror_dir("runs");
        fs::write(
            mirror.join("auth.json"),
            r#"{"OPENAI_API_KEY":"stale","tokens":{"access_token":"dead","refresh_token":"dead"}}"#,
        )
        .unwrap();
        // Force m-time well past the minimum-age guard.
        let stale = std::time::SystemTime::now()
            - std::time::Duration::from_secs(MIRROR_HEAL_MIN_AUTH_AGE_SECS + 60);
        filetime_set(&mirror.join("auth.json"), stale);

        let outcome = maybe_heal_mirror_on_auth_expired(Some(mirror.to_str().unwrap()));
        assert_eq!(outcome, MirrorHealOutcome::Healed);
        let auth_after = fs::read_to_string(mirror.join("auth.json")).unwrap();
        assert_eq!(
            auth_after.trim(),
            "{}",
            "auth.json must be emptied after heal"
        );

        // Second call for the same dir this process: gate fires.
        let outcome2 = maybe_heal_mirror_on_auth_expired(Some(mirror.to_str().unwrap()));
        assert_eq!(outcome2, MirrorHealOutcome::SkippedAlreadyFired);

        let _ = fs::remove_dir_all(&mirror);
    }

    #[test]
    fn heal_skips_when_auth_is_already_empty() {
        let mirror = fresh_mirror_dir("empty");
        fs::write(mirror.join("auth.json"), "{}").unwrap();
        let outcome = maybe_heal_mirror_on_auth_expired(Some(mirror.to_str().unwrap()));
        assert_eq!(outcome, MirrorHealOutcome::SkippedHealthyOrFresh);
        let _ = fs::remove_dir_all(&mirror);
    }

    #[test]
    fn heal_skips_when_auth_is_too_fresh() {
        let mirror = fresh_mirror_dir("fresh");
        fs::write(
            mirror.join("auth.json"),
            r#"{"OPENAI_API_KEY":"just-signed-in"}"#,
        )
        .unwrap();
        // m-time is now() by default — well below the 60 s floor.
        let outcome = maybe_heal_mirror_on_auth_expired(Some(mirror.to_str().unwrap()));
        assert_eq!(outcome, MirrorHealOutcome::SkippedHealthyOrFresh);
        // The fresh auth.json must be preserved verbatim.
        let preserved = fs::read_to_string(mirror.join("auth.json")).unwrap();
        assert!(preserved.contains("just-signed-in"));
        let _ = fs::remove_dir_all(&mirror);
    }

    #[test]
    fn heal_refuses_native_codex_dir() {
        // Even if the native `~/.codex` had credentials, the heal must
        // refuse to touch it. We don't actually need to write to the
        // real native dir for this assertion — the dir-path guard runs
        // before any filesystem read.
        let native = crate::config::get_codex_config_dir();
        let outcome = maybe_heal_mirror_on_auth_expired(Some(native.to_str().unwrap()));
        assert_eq!(outcome, MirrorHealOutcome::RefusedNativeTarget);
    }

    /// Best-effort m-time setter that doesn't pull in the `filetime` crate.
    /// Uses `std::fs::File::set_modified` (stable since 1.75).
    fn filetime_set(path: &std::path::Path, time: std::time::SystemTime) {
        let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        if let Err(error) = file.set_modified(time) {
            // Some Windows filesystems disallow backward m-time; fall back
            // to the kernel-set time so the test still runs (it will then
            // fall into the `too fresh` branch, not the heal branch).
            eprintln!("note: could not set mtime ({error}); test may fall back");
        }
    }
}

/// Gather the mirror-proxy inputs from the database and write the
/// mirror contract and configured model catalogue. Desktop launch and
/// provider switching preserve existing mirror account state.
///
/// `purge_auth = false`: only configuration/catalogue files are written — a login session the
/// user created inside the mirror survives switches, and AAG still never
/// writes any credential.
pub fn switch_codex_mirror(
    db: &crate::database::Database,
    codex_cli_home: Option<&str>,
    purge_auth: bool,
) -> Result<LiveWriteResult, AppError> {
    apply_codex_mirror(db, codex_cli_home, purge_auth, None, None)
}

/// Final preparation from a captured Codex routing snapshot. The runtime owner
/// supplies exact viable ids after evaluating routes, credentials and any pin.
/// Preserve mirror account state while replacing both catalogue and default.
pub(crate) fn apply_codex_runtime_models(
    db: &crate::database::Database,
    codex_cli_home: Option<&str>,
    allowed_ids: &[String],
) -> Result<LiveWriteResult, AppError> {
    apply_codex_mirror(db, codex_cli_home, false, None, Some(allowed_ids))
}

/// Desktop has no supported profile-selection launch flag. An explicit AAG
/// launch therefore selects AAG at the top level in the existing isolated home.
/// Validate the complete catalogue and TOML before replacing either file; keep
/// account/session files untouched and persist the mode only after preparation.
pub(crate) fn prepare_codex_desktop_mirror(
    db: &crate::database::Database,
    codex_cli_home: Option<&str>,
    allowed_ids: Option<&[String]>,
) -> Result<LiveWriteResult, AppError> {
    require_unmanaged_codex(db)?;
    require_legacy_codex_write()?;
    let mirror_dir = crate::config::get_codex_mirror_dir(codex_cli_home);
    if crate::config::same_dir(&mirror_dir, &crate::config::get_codex_config_dir()) {
        return Err(AppError::Config(
            "codex_cli_home must not resolve to the native ~/.codex: refusing to write the Codex Desktop app's shared config".into(),
        ));
    }
    let (model_ids, catalog) =
        crate::services::launcher::codex_catalog::build_codex_model_catalog(db, None, allowed_ids)?;
    if model_ids.is_empty() {
        return Err(AppError::Config("desktop_mirror_models_unavailable".into()));
    }
    let model = crate::services::launcher::codex_catalog::default_model(db, &model_ids, None)?;
    let provider_id = db.get_current_provider("codex")?.unwrap_or_default();
    let proxy_cfg = db.get_proxy_config()?;
    let gateway_url = format!(
        "http://{}:{}/v1",
        proxy_cfg.client_endpoint_host(),
        proxy_cfg.port
    );

    crate::config::private_fs::prepare_managed_private_dir(&mirror_dir)?;
    let config_path = mirror_dir.join("config.toml");
    let catalog_path = mirror_dir.join(crate::services::launcher::codex_catalog::CATALOG_FILENAME);
    let previous_config = crate::config::private_fs::read_mirror_file(&config_path)?;
    let previous_catalog = crate::config::private_fs::read_mirror_file(&catalog_path)?;
    let mirror_config = render_mirror_proxy_config(
        &mirror_dir,
        previous_config.contents.as_deref().unwrap_or_default(),
        &gateway_url,
        &proxy_cfg.auth_token,
        &model,
        Codex_mirror_mode::Exclusive,
        // Desktop launch is a refresh path: a reasoning effort saved by
        // Codex's own picker survives the merge untouched.
        None,
        false,
    )?;
    let (written, backups) = super::config_writes::write_mirror_files(&[
        super::config_writes::MirrorConfigFileUpdate {
            path: &catalog_path,
            contents: &catalog,
            app_type: "codex",
            previous: &previous_catalog,
        },
        super::config_writes::MirrorConfigFileUpdate {
            path: &config_path,
            contents: &mirror_config,
            app_type: "codex",
            previous: &previous_config,
        },
    ])?;
    db.set_setting(
        crate::database::dao::settings::KEY_CODEX_MIRROR_MODE,
        Codex_mirror_mode::Exclusive.as_str(),
    )?;
    Ok(LiveWriteResult {
        app_type: "codex".into(),
        provider_id,
        written,
        backup: backups.into_iter().next(),
    })
}

fn apply_codex_mirror(
    db: &crate::database::Database,
    codex_cli_home: Option<&str>,
    purge_auth: bool,
    selected_provider: Option<&Provider>,
    allowed_ids: Option<&[String]>,
) -> Result<LiveWriteResult, AppError> {
    require_unmanaged_codex(db)?;
    let explicit_selection = selected_provider.is_some();
    let current_provider = if selected_provider.is_none() {
        db.get_current_provider("codex")?
            .map(|id| db.get_provider_by_id(&id, "codex"))
            .transpose()?
            .flatten()
    } else {
        None
    };
    let selected_provider = selected_provider.or(current_provider.as_ref());
    let reasoning_effort = selected_provider
        .and_then(|provider| provider.settings_config.get("model_reasoning_effort"))
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| AppError::Config("codex_reasoning_effort_invalid".into()))
        })
        .transpose()?;
    if reasoning_effort
        .is_some_and(|value| !crate::services::launcher::codex_reasoning::valid_effort(value))
    {
        return Err(AppError::Config("codex_reasoning_effort_invalid".into()));
    }
    let proxy_cfg = db.get_proxy_config()?;
    // A wildcard or unset bind is not a client target — the Codex CLI dials
    // this URL, so translate it to the loopback (same rule as the Claude and
    // Gemini mirrors).
    let gateway_url = format!(
        "http://{}:{}/v1",
        proxy_cfg.client_endpoint_host(),
        proxy_cfg.port
    );
    let (model_ids, catalog_bytes) =
        crate::services::launcher::codex_catalog::build_codex_model_catalog(
            db,
            selected_provider,
            allowed_ids,
        )?;
    let model =
        crate::services::launcher::codex_catalog::default_model(db, &model_ids, selected_provider)?;
    if explicit_selection {
        if let Some(effort) = reasoning_effort {
            let capabilities =
                crate::services::launcher::codex_reasoning::Reasoning_catalog::from_db(
                    db,
                    selected_provider,
                )?;
            if !capabilities
                .for_model(&model)
                .levels
                .iter()
                .any(|supported| supported == effort)
            {
                return Err(AppError::Config(
                    "codex_reasoning_effort_unsupported".into(),
                ));
            }
        }
    }
    crate::services::launcher::codex_catalog::write_prepared_codex_model_catalog(
        db,
        &crate::config::get_codex_mirror_dir(codex_cli_home),
        &catalog_bytes,
    )?;
    let mode = db.get_setting(crate::database::dao::settings::KEY_CODEX_MIRROR_MODE)?;
    let mut result = write_mirror_proxy_files_with_effort(
        codex_cli_home,
        &gateway_url,
        &proxy_cfg.auth_token,
        &model,
        Codex_mirror_mode::from_setting(mode.as_deref()),
        purge_auth,
        reasoning_effort,
        explicit_selection,
    )?;
    result.provider_id = match selected_provider {
        Some(provider) => provider.id.clone(),
        None => db.get_current_provider("codex")?.unwrap_or_default(),
    };
    Ok(result)
}

/// Prepare a selected provider before it becomes current in the database.
/// Reading the old current provider here would write its model during a switch.
pub fn switch_codex_mirror_for_provider(
    db: &crate::database::Database,
    codex_cli_home: Option<&str>,
    purge_auth: bool,
    provider: &Provider,
) -> Result<LiveWriteResult, AppError> {
    apply_codex_mirror(db, codex_cli_home, purge_auth, Some(provider), None)
}

#[cfg(test)]
mod mirror_tests {
    use super::*;

    fn desktop_fixture(db: &crate::database::Database) {
        let input = serde_json::from_value(serde_json::json!({
            "expectedGeneration":0, "name":"Desktop fixture", "baseUrl":"https://example.invalid",
            "apiKey":"fixture_upstream_secret", "endpoints":[{"protocol":"openai-responses",
            "models":[{"modelId":"Desktop_Model.V1"}]}]
        }))
        .unwrap();
        crate::services::suppliers::save_supplier(db, input).unwrap();
        db.set_setting(
            crate::database::dao::settings::KEY_CODEX_MIRROR_MODE,
            "coexist",
        )
        .unwrap();
    }

    #[test]
    fn desktop_launch_activates_aag_without_changing_auth_sessions_or_native_home() {
        let home = crate::services::provider::test_support::isolated_home("desktop-aag-activation");
        let db = crate::database::Database::memory().unwrap();
        desktop_fixture(&db);
        let native = home.root.join(".codex");
        let mirror = home.root.join("mirror");
        for directory in [&native, &mirror, &mirror.join("sessions")] {
            crate::config::private_fs::prepare_managed_private_dir(directory).unwrap();
        }
        crate::config::private_fs::atomic_write(&native.join("config.toml"), b"# native fixture\n")
            .unwrap();
        crate::config::private_fs::atomic_write(&mirror.join("config.toml"), b"# keep mirror preferences\nmodel_provider = \"openai\"\n[features]\nfixture_flag = true\n").unwrap();
        crate::config::private_fs::atomic_write(
            &mirror.join("auth.json"),
            b"{\"fixture_session\":true}",
        )
        .unwrap();
        crate::config::private_fs::atomic_write(
            &mirror.join("sessions/keep.jsonl"),
            b"fixture session history\n",
        )
        .unwrap();
        let result =
            prepare_codex_desktop_mirror(&db, mirror.to_str(), Some(&["Desktop_Model.V1".into()]))
                .unwrap();
        let text = std::fs::read_to_string(mirror.join("config.toml")).unwrap();
        let config: toml_edit::DocumentMut = text.parse().unwrap();
        assert_eq!(config["model_provider"].as_str(), Some("aag"));
        assert_eq!(config["model"].as_str(), Some("Desktop_Model.V1"));
        assert!(config["model_catalog_json"]
            .as_str()
            .unwrap()
            .ends_with("cc-switch-model-catalog.json"));
        assert_eq!(
            config["model_providers"]["aag"]["requires_openai_auth"].as_bool(),
            Some(false)
        );
        assert!(text.contains("# keep mirror preferences"));
        assert_eq!(config["features"]["fixture_flag"].as_bool(), Some(true));
        assert_eq!(
            db.get_setting(crate::database::dao::settings::KEY_CODEX_MIRROR_MODE)
                .unwrap()
                .as_deref(),
            Some("exclusive")
        );
        assert_eq!(
            std::fs::read(mirror.join("auth.json")).unwrap(),
            b"{\"fixture_session\":true}"
        );
        assert_eq!(
            std::fs::read(mirror.join("sessions/keep.jsonl")).unwrap(),
            b"fixture session history\n"
        );
        assert_eq!(
            std::fs::read(native.join("config.toml")).unwrap(),
            b"# native fixture\n"
        );
        assert!(result.backup.is_some());
    }

    #[test]
    fn desktop_launch_empty_or_unavailable_catalog_preserves_configuration_and_mode() {
        let home = crate::services::provider::test_support::isolated_home("desktop-empty-catalog");
        let db = crate::database::Database::memory().unwrap();
        desktop_fixture(&db);
        let mirror = home.root.join("mirror");
        crate::config::private_fs::prepare_managed_private_dir(&mirror).unwrap();
        let config = b"# existing mirror\nmodel_provider = \"openai\"\n";
        let catalog = br#"{"models":[{"slug":"preserved"}]}"#;
        crate::config::private_fs::atomic_write(&mirror.join("config.toml"), config).unwrap();
        crate::config::private_fs::atomic_write(
            &mirror.join("cc-switch-model-catalog.json"),
            catalog,
        )
        .unwrap();
        for allowed in [Vec::new(), vec!["not-offered".to_string()]] {
            let error =
                prepare_codex_desktop_mirror(&db, mirror.to_str(), Some(&allowed)).unwrap_err();
            assert!(
                matches!(error, AppError::Config(code) if code == "desktop_mirror_models_unavailable")
            );
            assert_eq!(std::fs::read(mirror.join("config.toml")).unwrap(), config);
            assert_eq!(
                std::fs::read(mirror.join("cc-switch-model-catalog.json")).unwrap(),
                catalog
            );
            assert_eq!(
                db.get_setting(crate::database::dao::settings::KEY_CODEX_MIRROR_MODE)
                    .unwrap()
                    .as_deref(),
                Some("coexist")
            );
        }
        let absent = home.root.join("not_created");
        assert!(prepare_codex_desktop_mirror(&db, absent.to_str(), Some(&[])).is_err());
        assert!(!absent.exists());
    }

    #[test]
    fn desktop_launch_invalid_existing_config_does_not_replace_catalog_or_mode() {
        let home = crate::services::provider::test_support::isolated_home("desktop-invalid-config");
        let db = crate::database::Database::memory().unwrap();
        desktop_fixture(&db);
        let mirror = home.root.join("mirror");
        crate::config::private_fs::prepare_managed_private_dir(&mirror).unwrap();
        crate::config::private_fs::atomic_write(&mirror.join("config.toml"), b"[invalid\n")
            .unwrap();
        let catalog = br#"{"models":[{"slug":"preserved"}]}"#;
        crate::config::private_fs::atomic_write(
            &mirror.join("cc-switch-model-catalog.json"),
            catalog,
        )
        .unwrap();
        assert!(prepare_codex_desktop_mirror(
            &db,
            mirror.to_str(),
            Some(&["Desktop_Model.V1".into()])
        )
        .is_err());
        assert_eq!(
            std::fs::read(mirror.join("config.toml")).unwrap(),
            b"[invalid\n"
        );
        assert_eq!(
            std::fs::read(mirror.join("cc-switch-model-catalog.json")).unwrap(),
            catalog
        );
        assert_eq!(
            db.get_setting(crate::database::dao::settings::KEY_CODEX_MIRROR_MODE)
                .unwrap()
                .as_deref(),
            Some("coexist")
        );
    }

    #[test]
    fn desktop_launch_change_does_not_change_ordinary_cli_coexist_preparation() {
        let home = crate::services::provider::test_support::isolated_home("desktop-cli-coexist");
        let db = crate::database::Database::memory().unwrap();
        desktop_fixture(&db);
        let mirror = home.root.join("mirror");
        apply_codex_runtime_models(&db, mirror.to_str(), &["Desktop_Model.V1".into()]).unwrap();
        let text = std::fs::read_to_string(mirror.join("config.toml")).unwrap();
        let config: toml_edit::DocumentMut = text.parse().unwrap();
        assert!(config.get("model_provider").is_none());
        assert_eq!(
            config["profiles"]["aag"]["model_provider"].as_str(),
            Some("aag")
        );
        assert_eq!(
            db.get_setting(crate::database::dao::settings::KEY_CODEX_MIRROR_MODE)
                .unwrap()
                .as_deref(),
            Some("coexist")
        );
    }

    #[test]
    fn desktop_launch_managed_authority_rejection_precedes_every_write() {
        let home =
            crate::services::provider::test_support::isolated_home("desktop-authority-guard");
        let db = crate::database::Database::memory().unwrap();
        db.set_setting("codex_session_authority_enabled", "1")
            .unwrap();
        let mirror = home.root.join("mirror");
        assert_eq!(
            String::from(
                prepare_codex_desktop_mirror(&db, mirror.to_str(), Some(&["fixture".into()]))
                    .unwrap_err()
            ),
            "session_client_authority_unavailable"
        );
        assert!(!mirror.exists());
    }

    #[test]
    fn runtime_refresh_preserves_native_picker_effort_until_explicit_manager_selection() {
        let home = crate::services::provider::test_support::isolated_home("codex-native-effort");
        let mirror = home.root.join("mirror");
        let db = crate::database::Database::memory().unwrap();
        let upstream = serde_json::from_value(serde_json::json!({"id":"fixture","name":"Fixture","type":"openai","baseURL":"https://example.invalid/v1","apiKey":"fixture","models":["gpt-6-sol"],"enabled":true})).unwrap();
        db.save_proxy_provider(&upstream).unwrap();
        let provider = Provider::new(
            "selected".into(),
            "Codex".into(),
            serde_json::json!({"model":"gpt-6-sol","model_reasoning_effort":"low"}),
        );
        db.save_provider("codex", &provider).unwrap();
        db.set_current_provider("selected", "codex").unwrap();
        apply_codex_mirror(&db, mirror.to_str(), false, None, None).unwrap();
        let config_path = mirror.join("config.toml");
        let mut document = std::fs::read_to_string(&config_path)
            .unwrap()
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        assert_eq!(document["model_reasoning_effort"].as_str(), Some("low"));
        document["model_reasoning_effort"] = toml_edit::value("high");
        std::fs::write(&config_path, document.to_string()).unwrap();
        apply_codex_runtime_models(&db, mirror.to_str(), &["gpt-6-sol".into()]).unwrap();
        let document = std::fs::read_to_string(&config_path)
            .unwrap()
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        assert_eq!(document["model_reasoning_effort"].as_str(), Some("high"));
        apply_codex_mirror(&db, mirror.to_str(), false, Some(&provider), None).unwrap();
        let document = std::fs::read_to_string(&config_path)
            .unwrap()
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        assert_eq!(document["model_reasoning_effort"].as_str(), Some("low"));
    }

    #[test]
    fn unsupported_effort_fails_before_any_catalog_or_config_write() {
        let home =
            crate::services::provider::test_support::isolated_home("codex-unsupported-effort");
        let mirror = home.root.join("mirror");
        let db = crate::database::Database::memory().unwrap();
        let upstream = serde_json::from_value(serde_json::json!({"id":"fixture","name":"Fixture","type":"openai","baseURL":"https://example.invalid/v1","apiKey":"fixture","models":["gpt-5-codex"],"enabled":true})).unwrap();
        db.save_proxy_provider(&upstream).unwrap();
        let provider = Provider::new(
            "selected".into(),
            "Codex".into(),
            serde_json::json!({"model":"5.6sol","model_reasoning_effort":"max","modelCatalog":{"models":[{"display":"5.6sol","model":"gpt-5-codex","provider":"fixture"}]}}),
        );
        let error =
            apply_codex_mirror(&db, mirror.to_str(), false, Some(&provider), None).unwrap_err();
        assert_eq!(String::from(error), "codex_reasoning_effort_unsupported");
        assert!(!mirror.exists());
    }

    #[test]
    fn selected_provider_effort_is_persisted_in_the_managed_mirror_profile() {
        let home =
            crate::services::provider::test_support::isolated_home("codex-reasoning-selection");
        let mirror = home.root.join("mirror");
        let db = crate::database::Database::memory().unwrap();
        let upstream = serde_json::from_value(serde_json::json!({"id":"fixture","name":"Fixture","type":"openai","baseURL":"https://example.invalid/v1","apiKey":"fixture","models":["gpt-5.6-sol"],"enabled":true})).unwrap();
        db.save_proxy_provider(&upstream).unwrap();
        let provider = Provider::new(
            "selected".into(),
            "Codex".into(),
            serde_json::json!({"model":"gpt-5.6-sol","model_reasoning_effort":"max"}),
        );
        db.save_provider("codex", &provider).unwrap();
        db.set_current_provider("selected", "codex").unwrap();
        let saved = db.get_provider_by_id("selected", "codex").unwrap().unwrap();
        assert_eq!(saved.settings_config["model_reasoning_effort"], "max");
        // Supply an admitted model set just as the runtime owner does; no
        // live inventory or upstream connection is necessary for this seam.
        apply_codex_mirror(
            &db,
            mirror.to_str(),
            false,
            Some(&saved),
            Some(&["gpt-5.6-sol".into()]),
        )
        .unwrap();
        let text = std::fs::read_to_string(mirror.join("config.toml")).unwrap();
        let document = text.parse::<toml_edit::DocumentMut>().unwrap();
        assert_eq!(document["model_reasoning_effort"].as_str(), Some("max"));
        assert_eq!(
            document["profiles"]["aag"]["model_reasoning_effort"].as_str(),
            Some("max")
        );
        let cleared = Provider::new(
            "selected".into(),
            "Codex".into(),
            serde_json::json!({"model":"gpt-5.6-sol"}),
        );
        db.set_setting(
            crate::database::dao::settings::KEY_CODEX_MIRROR_MODE,
            "exclusive",
        )
        .unwrap();
        apply_codex_mirror(
            &db,
            mirror.to_str(),
            false,
            Some(&cleared),
            Some(&["gpt-5.6-sol".into()]),
        )
        .unwrap();
        let text = std::fs::read_to_string(mirror.join("config.toml")).unwrap();
        let document = text.parse::<toml_edit::DocumentMut>().unwrap();
        assert!(document.get("model_reasoning_effort").is_none());
        assert!(document["profiles"]["aag"]
            .get("model_reasoning_effort")
            .is_none());
        apply_codex_mirror(
            &db,
            mirror.to_str(),
            false,
            Some(&saved),
            Some(&["gpt-5.6-sol".into()]),
        )
        .unwrap();
        db.set_setting(
            crate::database::dao::settings::KEY_CODEX_MIRROR_MODE,
            "coexist",
        )
        .unwrap();
        apply_codex_mirror(
            &db,
            mirror.to_str(),
            false,
            Some(&saved),
            Some(&["gpt-5.6-sol".into()]),
        )
        .unwrap();
        let text = std::fs::read_to_string(mirror.join("config.toml")).unwrap();
        let document = text.parse::<toml_edit::DocumentMut>().unwrap();
        assert!(document.get("model_reasoning_effort").is_none());
        assert_eq!(
            document["profiles"]["aag"]["model_reasoning_effort"].as_str(),
            Some("max")
        );
    }

    #[test]
    fn managed_authority_blocks_legacy_model_catalog_before_any_write() {
        let home = crate::services::provider::test_support::isolated_home("managed-legacy-model");
        let db = crate::database::Database::memory().unwrap();
        db.set_setting("codex_session_authority_enabled", "1")
            .unwrap();
        let error = apply_codex_runtime_models(&db, None, &["fixture_model".into()]).unwrap_err();
        assert_eq!(String::from(error), "session_client_authority_unavailable");
        assert!(!home.root.join(".codex-cli").exists());
    }

    #[test]
    fn managed_authority_blocks_direct_auth_write_and_reset_with_marker() {
        let home = crate::services::provider::test_support::isolated_home("managed-legacy-auth");
        let mirror = home.root.join("custom_mirror");
        crate::config::private_fs::prepare_private_dir(&mirror).unwrap();
        crate::config::private_fs::atomic_write(
            &mirror.join("auth.json"),
            b"{\"fixture_session\":true}",
        )
        .unwrap();
        crate::config::private_fs::atomic_write(
            &mirror.join("config.toml"),
            b"model = \"fixture_original\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(home.root.join(".codex-cli/.authority")).unwrap();
        let provider = Provider::new(
            "fixture".into(),
            "Fixture".into(),
            serde_json::json!({"auth":{"new":"fixture"},"config":"model = \"replaced\"\n"}),
        );
        assert_eq!(
            String::from(write_live(&provider, mirror.to_str()).unwrap_err()),
            "session_client_authority_unavailable"
        );
        assert_eq!(
            String::from(reset_mirror_session_state_guarded(mirror.to_str()).unwrap_err()),
            "session_client_authority_unavailable"
        );
        assert_eq!(
            String::from(
                write_mirror_proxy_files(
                    mirror.to_str(),
                    "http://127.0.0.1:8989/v1",
                    "fixture_key",
                    "fixture_model",
                    Codex_mirror_mode::Exclusive,
                    false
                )
                .unwrap_err()
            ),
            "session_client_authority_unavailable"
        );
        assert_eq!(
            std::fs::read(mirror.join("auth.json")).unwrap(),
            b"{\"fixture_session\":true}"
        );
        assert_eq!(
            std::fs::read(mirror.join("config.toml")).unwrap(),
            b"model = \"fixture_original\"\n"
        );
        assert!(!home.root.join("aag_state/backups").exists());
    }

    #[test]
    fn login_reset_preserves_conversation_history_and_attachment_storage() {
        let fixture = crate::config::private_fs::private_temp_dir(&std::env::temp_dir()).unwrap();
        let mirror = fixture.path().join("mirror");
        crate::config::private_fs::prepare_private_dir(&mirror).unwrap();
        for directory in ["sessions", "cache"] {
            crate::config::private_fs::prepare_private_dir(&mirror.join(directory)).unwrap();
        }
        let retained = [
            (
                "sessions/session.jsonl",
                b"synthetic immutable conversation\n".as_slice(),
            ),
            (
                "cache/attachment.bin",
                b"synthetic attachment\0\x01".as_slice(),
            ),
            ("history.jsonl", b"synthetic ordered history\n".as_slice()),
            (
                "state_5.sqlite",
                b"synthetic native state fixture".as_slice(),
            ),
        ];
        for (name, bytes) in retained {
            std::fs::write(mirror.join(name), bytes).unwrap();
        }
        crate::config::private_fs::atomic_write(
            &mirror.join("auth.json"),
            b"{\"fixtureLogin\":true}",
        )
        .unwrap();
        reset_mirror_session_state(&mirror).unwrap();
        assert_eq!(std::fs::read(mirror.join("auth.json")).unwrap(), b"{}");
        for (name, bytes) in retained {
            assert_eq!(std::fs::read(mirror.join(name)).unwrap(), bytes, "{name}");
        }
    }

    #[test]
    fn unified_switch_refreshes_catalog_and_selects_only_an_offered_default() {
        let home = crate::services::provider::test_support::isolated_home("codex-catalog-switch");
        let db = crate::database::Database::memory().unwrap();
        let input = serde_json::from_value(serde_json::json!({
            "expectedGeneration":0, "name":"Fixture", "baseUrl":"https://example.invalid",
            "apiKey":"fixture_upstream_secret", "endpoints":[{"protocol":"openai-responses",
            "models":[{"modelId":"Vendor_Model.V1"}]}]
        }))
        .unwrap();
        crate::services::suppliers::save_supplier(&db, input).unwrap();
        db.set_setting(
            crate::services::supplier_setup::KEY_UNIFIED_DEFAULT_MODEL,
            "stale-model",
        )
        .unwrap();
        let mirror = home.root.join("mirror");
        crate::config::private_fs::prepare_managed_private_dir(&mirror).unwrap();
        crate::config::private_fs::atomic_write(
            &mirror.join("config.toml"),
            b"profile = \"old\"\n[profiles.old]\nmodel_provider = \"openai\"\n",
        )
        .unwrap();
        switch_codex_mirror(&db, mirror.to_str(), false).unwrap();
        let text = std::fs::read_to_string(mirror.join("config.toml")).unwrap();
        let config = text.parse::<toml_edit::DocumentMut>().unwrap();
        assert!(config.get("profile").is_none());
        assert_eq!(config["model"].as_str(), Some("Vendor_Model.V1"));
        assert_eq!(
            config["profiles"]["aag"]["model"].as_str(),
            Some("Vendor_Model.V1")
        );
        let catalog_path = config["model_catalog_json"].as_str().unwrap();
        assert!(std::path::Path::new(catalog_path).is_absolute());
        let catalog: Value = serde_json::from_slice(&std::fs::read(catalog_path).unwrap()).unwrap();
        assert_eq!(catalog["models"][0]["slug"], "Vendor_Model.V1");
        assert_eq!(catalog["models"].as_array().unwrap().len(), 1);
        assert_eq!(
            config["profiles"]["old"]["model_provider"].as_str(),
            Some("openai")
        );
        assert!(!text.contains("fixture_upstream_secret"));
    }

    #[test]
    fn empty_supply_removes_stale_model_and_writes_empty_catalog() {
        let home = crate::services::provider::test_support::isolated_home("codex-cleared-catalog");
        let db = crate::database::Database::memory().unwrap();
        db.set_setting(
            crate::services::supplier_setup::KEY_UNIFIED_DEFAULT_MODEL,
            "",
        )
        .unwrap();
        let mirror = home.root.join("mirror");
        crate::config::private_fs::prepare_managed_private_dir(&mirror).unwrap();
        crate::config::private_fs::atomic_write(
            &mirror.join("config.toml"),
            b"model = \"stale-model\"\n",
        )
        .unwrap();
        switch_codex_mirror(&db, mirror.to_str(), false).unwrap();
        let config = std::fs::read_to_string(mirror.join("config.toml"))
            .unwrap()
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        assert!(config.get("model").is_none());
        assert!(config["profiles"]["aag"].get("model").is_none());
        let catalog: Value = serde_json::from_slice(
            &std::fs::read(config["model_catalog_json"].as_str().unwrap()).unwrap(),
        )
        .unwrap();
        assert_eq!(catalog, serde_json::json!({"models":[]}));
    }

    #[test]
    fn runtime_filter_replaces_blocked_default_and_is_idempotent() {
        let home = crate::services::provider::test_support::isolated_home("codex-runtime-filter");
        let db = crate::database::Database::memory().unwrap();
        let input = serde_json::from_value(serde_json::json!({
            "expectedGeneration":0, "name":"Fixture", "baseUrl":"https://example.invalid",
            "apiKey":"fixture", "endpoints":[{"protocol":"openai-responses", "models":[
                {"modelId":"blocked-default"}, {"modelId":"Usable_Model"}
            ]}]
        }))
        .unwrap();
        crate::services::suppliers::save_supplier(&db, input).unwrap();
        db.set_setting(
            crate::services::supplier_setup::KEY_UNIFIED_DEFAULT_MODEL,
            "blocked-default",
        )
        .unwrap();
        let mirror = home.root.join("mirror");
        switch_codex_mirror(&db, mirror.to_str(), false).unwrap();
        let allowed_ids = vec!["Usable_Model".to_string(), "not-in-inventory".to_string()];
        apply_codex_runtime_models(&db, mirror.to_str(), &allowed_ids).unwrap();
        let config_path = mirror.join("config.toml");
        let catalog_path = mirror.join(crate::services::launcher::codex_catalog::CATALOG_FILENAME);
        let config = std::fs::read_to_string(&config_path)
            .unwrap()
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        assert_eq!(config["model"].as_str(), Some("Usable_Model"));
        assert_eq!(
            config["profiles"]["aag"]["model"].as_str(),
            Some("Usable_Model")
        );
        let catalog: Value =
            serde_json::from_slice(&std::fs::read(&catalog_path).unwrap()).unwrap();
        assert_eq!(catalog["models"].as_array().unwrap().len(), 1);
        assert_eq!(catalog["models"][0]["slug"], "Usable_Model");

        let past =
            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        for path in [&config_path, &catalog_path] {
            std::fs::OpenOptions::new()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(past)
                .unwrap();
        }
        apply_codex_runtime_models(&db, mirror.to_str(), &allowed_ids).unwrap();
        for path in [&config_path, &catalog_path] {
            assert_eq!(std::fs::metadata(path).unwrap().modified().unwrap(), past);
        }
    }

    #[test]
    fn empty_runtime_allowlist_clears_models_even_when_supply_is_configured() {
        let home =
            crate::services::provider::test_support::isolated_home("codex-empty-runtime-filter");
        let db = crate::database::Database::memory().unwrap();
        let input = serde_json::from_value(serde_json::json!({
            "expectedGeneration":0, "name":"Fixture", "baseUrl":"https://example.invalid",
            "apiKey":"fixture", "endpoints":[{"protocol":"openai-responses",
                "models":[{"modelId":"configured-but-unusable"}]}]
        }))
        .unwrap();
        crate::services::suppliers::save_supplier(&db, input).unwrap();
        let mirror = home.root.join("mirror");
        switch_codex_mirror(&db, mirror.to_str(), false).unwrap();
        apply_codex_runtime_models(&db, mirror.to_str(), &[]).unwrap();
        let config = std::fs::read_to_string(mirror.join("config.toml"))
            .unwrap()
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        assert!(config.get("model").is_none());
        assert!(config["profiles"]["aag"].get("model").is_none());
        let catalog: Value = serde_json::from_slice(
            &std::fs::read(mirror.join(crate::services::launcher::codex_catalog::CATALOG_FILENAME))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(catalog, serde_json::json!({"models":[]}));
    }

    #[test]
    fn switching_to_coexist_removes_only_managed_top_level_selection() {
        let home =
            crate::services::provider::test_support::isolated_home("codex-coexist-transition");
        let mirror = home.root.join("mirror");
        for mode in [Codex_mirror_mode::Exclusive, Codex_mirror_mode::Coexist] {
            write_mirror_proxy_files(
                mirror.to_str(),
                "http://127.0.0.1:8989/v1",
                "fixture",
                "model",
                mode,
                false,
            )
            .unwrap();
        }
        let config = std::fs::read_to_string(mirror.join("config.toml"))
            .unwrap()
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        assert!(config.get("profile").is_none());
        assert!(config.get("model").is_none());
        assert!(config.get("model_provider").is_none());
        assert!(config.get("model_catalog_json").is_none());
        assert!(config.get("web_search").is_none());
        assert_eq!(
            config["profiles"]["aag"]["web_search"].as_str(),
            Some("disabled")
        );
        assert_eq!(
            config["profiles"]["aag"]["model_provider"].as_str(),
            Some("aag")
        );
    }

    #[test]
    fn coexist_preserves_first_party_search_while_aag_profile_disables_hosted_search() {
        let home = crate::services::provider::test_support::isolated_home("codex-coexist-search");
        let mirror = home.root.join("mirror");
        crate::config::private_fs::prepare_managed_private_dir(&mirror).unwrap();
        crate::config::private_fs::atomic_write(
            &mirror.join("config.toml"),
            b"web_search = \"live\"\n",
        )
        .unwrap();
        write_mirror_proxy_files(
            mirror.to_str(),
            "http://127.0.0.1:8989/v1",
            "fixture",
            "model",
            Codex_mirror_mode::Coexist,
            false,
        )
        .unwrap();
        let config = std::fs::read_to_string(mirror.join("config.toml"))
            .unwrap()
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        assert_eq!(config["web_search"].as_str(), Some("live"));
        assert_eq!(
            config["profiles"]["aag"]["web_search"].as_str(),
            Some("disabled")
        );
        write_mirror_proxy_files(
            mirror.to_str(),
            "http://127.0.0.1:8989/v1",
            "fixture",
            "model",
            Codex_mirror_mode::Exclusive,
            false,
        )
        .unwrap();
        let config = std::fs::read_to_string(mirror.join("config.toml"))
            .unwrap()
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        assert_eq!(config["web_search"].as_str(), Some("disabled"));
    }

    /// The mirror contract is credential-free: config.toml carries only
    /// AAG's proxy endpoint and token, never any provider credential. With
    /// `purge_auth = true` an explicit reset clears auth.json and known global
    /// login-state markers while preserving cache/session/history data; with
    /// `purge_auth = false` (switch) auth.json is left untouched so the user's
    /// own mirror login survives a switch.
    #[test]
    fn mirror_proxy_files_are_credential_free_and_purge_is_explicit() {
        // IsolatedHome sets AAG_HOME_DIR/AAG_CONFIG_DIR so the backup path
        // lands under the test-owned root, never the real ~/.aag.
        let home = crate::services::provider::test_support::isolated_home("codex-mirror");
        let mirror = home.root.join("codex-mirror");
        let _ = std::fs::remove_dir_all(&mirror);
        std::fs::create_dir_all(&mirror).unwrap();
        let stale_auth = r#"{"tokens":{"id_token":"stale-login","access_token":"stale"},"OPENAI_API_KEY":"sk-stale"}"#;
        std::fs::write(mirror.join("auth.json"), stale_auth).unwrap();
        std::fs::write(
            mirror.join(".codex-global-state.json"),
            r#"{"cachedAccount":"stale"}"#,
        )
        .unwrap();
        std::fs::write(mirror.join(".codex-global-state.json.bak"), "{}").unwrap();
        let cached_tool_state = r#"{"tool":"preserved"}"#;
        let session_history = "{\"conversation\":\"preserved\"}\n";
        std::fs::create_dir_all(mirror.join("cache").join("codex_apps_tools")).unwrap();
        std::fs::write(
            mirror
                .join("cache")
                .join("codex_apps_tools")
                .join("app.json"),
            cached_tool_state,
        )
        .unwrap();
        std::fs::create_dir_all(mirror.join("sessions").join("2026")).unwrap();
        std::fs::write(
            mirror.join("sessions").join("2026").join("rollout.jsonl"),
            session_history,
        )
        .unwrap();

        // Switch semantics: config written, auth.json untouched.
        let result = write_mirror_proxy_files(
            mirror.to_str(),
            "http://127.0.0.1:8989/v1",
            "aag-proxy-token",
            "gpt-5",
            Codex_mirror_mode::Exclusive,
            false,
        )
        .expect("write mirror contract");

        let written_names: Vec<String> = result
            .written
            .iter()
            .map(|p| {
                std::path::Path::new(p)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert!(written_names.contains(&"config.toml".to_string()));
        assert!(
            !written_names.contains(&"auth.json".to_string()),
            "a switch must not touch auth.json"
        );
        let auth = std::fs::read_to_string(mirror.join("auth.json")).unwrap();
        assert_eq!(
            auth, stale_auth,
            "switch must preserve the mirror's own auth state"
        );
        assert!(
            mirror.join(".codex-global-state.json").exists(),
            "switch must not touch the app's global state either"
        );

        let config = std::fs::read_to_string(mirror.join("config.toml")).unwrap();
        assert!(
            config.contains("base_url = \"http://127.0.0.1:8989/v1\""),
            "{config}"
        );
        assert!(config.contains("wire_api = \"responses\""), "{config}");
        assert!(config.contains("requires_openai_auth = false"), "{config}");
        assert!(
            config.contains("experimental_bearer_token = \"aag-proxy-token\""),
            "{config}"
        );
        assert!(config.contains("model = \"gpt-5\""), "{config}");
        assert!(
            !config.contains("sk-"),
            "no provider key may reach the mirror: {config}"
        );

        // Explicit reset semantics: clear authentication and known global
        // login state while preserving the mirror's cache and conversation data.
        let result = write_mirror_proxy_files(
            mirror.to_str(),
            "http://127.0.0.1:8989/v1",
            "aag-proxy-token",
            "gpt-5",
            Codex_mirror_mode::Exclusive,
            true,
        )
        .expect("write mirror contract");
        let written_names: Vec<String> = result
            .written
            .iter()
            .map(|p| {
                std::path::Path::new(p)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert!(written_names.contains(&"auth.json".to_string()));
        let auth = std::fs::read_to_string(mirror.join("auth.json")).unwrap();
        assert_eq!(
            auth.trim(),
            "{}",
            "explicit reset must clear mirror login state"
        );
        assert!(
            !mirror.join(".codex-global-state.json").exists()
                && !mirror.join(".codex-global-state.json.bak").exists(),
            "explicit reset must remove known global login-state files"
        );
        assert_eq!(
            std::fs::read_to_string(mirror.join("cache/codex_apps_tools/app.json")).unwrap(),
            cached_tool_state,
            "reset must preserve unrelated mirror cache data"
        );
        assert_eq!(
            std::fs::read_to_string(mirror.join("sessions/2026/rollout.jsonl")).unwrap(),
            session_history,
            "reset must preserve mirror conversation history"
        );

        // TOML escaping: quotes and backslashes in a token must not break
        // the generated config. The writer may spell the value as a basic
        // string (`"tok\"ens\\x"`) or a literal string (`'tok"ens\x'`) — both
        // are valid TOML — so assert what the value *reads back as*, which
        // is the property Codex actually depends on, rather than one
        // particular escape style.
        write_mirror_proxy_files(
            mirror.to_str(),
            "http://127.0.0.1:8989/v1",
            "tok\"ens\\x",
            "m",
            Codex_mirror_mode::Exclusive,
            false,
        )
        .unwrap();
        let config = std::fs::read_to_string(mirror.join("config.toml")).unwrap();
        let doc = config
            .parse::<toml_edit::DocumentMut>()
            .unwrap_or_else(|e| panic!("generated config must be valid TOML: {e}\n{config}"));
        let read_back = doc
            .get("model_providers")
            .and_then(|p| p.get("aag"))
            .and_then(|a| a.get("experimental_bearer_token"))
            .and_then(|v| v.as_str())
            .expect("experimental_bearer_token must be present and a string");
        assert_eq!(
            read_back, "tok\"ens\\x",
            "the token must survive TOML escaping byte-for-byte: {config}"
        );
    }

    /// auth.json classification is shape-only: {} / null / missing are
    /// "empty" (logged out); anything else counts as occupied.
    #[test]
    fn mirror_auth_json_is_classified_by_shape_only() {
        let home = crate::services::provider::test_support::isolated_home("codex-classify");
        let mirror = home.root.join("codex-mirror");
        let _ = std::fs::remove_dir_all(&mirror);
        std::fs::create_dir_all(&mirror).unwrap();
        assert_eq!(classify_mirror_auth_json(&mirror), "missing");
        std::fs::write(mirror.join("auth.json"), "{}").unwrap();
        assert_eq!(classify_mirror_auth_json(&mirror), "empty");
        std::fs::write(mirror.join("auth.json"), "null").unwrap();
        assert_eq!(classify_mirror_auth_json(&mirror), "empty");
        std::fs::write(
            mirror.join("auth.json"),
            r#"{"tokens":{"access_token":"x"}}"#,
        )
        .unwrap();
        assert_eq!(classify_mirror_auth_json(&mirror), "credentials_present");
    }

    /// Coexist mode keeps the ChatGPT login usable as the default: no
    /// top-level provider/model is written, AAG stays declared, and a
    /// `[profiles.aag]` profile points at it for manual switching.
    #[test]
    fn coexist_template_omits_top_level_provider_and_declares_profile() {
        let home = crate::services::provider::test_support::isolated_home("codex-coexist");
        let mirror = home.root.join("codex-mirror");
        let _ = std::fs::remove_dir_all(&mirror);
        std::fs::create_dir_all(&mirror).unwrap();

        write_mirror_proxy_files(
            mirror.to_str(),
            "http://127.0.0.1:8989/v1",
            "aag-proxy-token",
            "claude-via-aag",
            Codex_mirror_mode::Coexist,
            false,
        )
        .expect("write coexist mirror contract");
        let config = std::fs::read_to_string(mirror.join("config.toml")).unwrap();

        // No top-level keys before the first table header: Codex defaults
        // to its builtin openai provider, so an in-mirror ChatGPT login
        // serves official models without AAG in the path.
        let first_table = config.find('[').unwrap();
        assert!(
            !config[..first_table].contains("model_provider"),
            "coexist must not force a top-level model_provider: {config}"
        );
        assert!(
            !config[..first_table].contains("model ="),
            "coexist must not force a top-level model: {config}"
        );
        // AAG stays declared and reachable via the profile.
        assert!(config.contains("[model_providers.aag]"), "{config}");
        assert!(config.contains("[profiles.aag]"), "{config}");
        assert!(config.contains("model_provider = \"aag\""), "{config}");
        assert!(config.contains("model = \"claude-via-aag\""), "{config}");
        assert!(
            config.contains("experimental_bearer_token = \"aag-proxy-token\""),
            "{config}"
        );
    }

    /// The defect this port fixes: the mirror writer used to render
    /// `config.toml` from a template, so switching providers destroyed every
    /// comment and unrelated key the user had in it. The merge must leave
    /// their file alone outside the managed keys — at the real call site, not
    /// only in `toml_merge`'s unit tests.
    #[test]
    fn switching_preserves_the_users_comments_and_unrelated_keys() {
        let home = crate::services::provider::test_support::isolated_home("codex-preserve");
        let mirror = home.root.join("codex-mirror");
        let _ = std::fs::remove_dir_all(&mirror);
        std::fs::create_dir_all(&mirror).unwrap();
        let hand_written = "# I tuned this by hand\n\
                            \n\
                            approval_policy = \"on-request\"\n\
                            \n\
                            [mcp_servers.my_tools]\n\
                            command = \"npx\"\n\
                            args = [\"-y\", \"my-tools\"]  # keep this too\n";
        std::fs::write(mirror.join("config.toml"), hand_written).unwrap();

        write_mirror_proxy_files(
            mirror.to_str(),
            "http://127.0.0.1:8989/v1",
            "aag-proxy-token",
            "gpt-5",
            Codex_mirror_mode::Exclusive,
            false,
        )
        .expect("switch must succeed against a hand-written config");

        let config = std::fs::read_to_string(mirror.join("config.toml")).unwrap();
        assert!(
            config.contains("# I tuned this by hand"),
            "the user's leading comment must survive a switch: {config}"
        );
        assert!(
            config.contains("# keep this too"),
            "a trailing comment on an unrelated key must survive: {config}"
        );
        assert!(
            config.contains("approval_policy = \"on-request\""),
            "an unrelated top-level key must survive: {config}"
        );
        assert!(
            config.contains("[mcp_servers.my_tools]"),
            "an unrelated table must survive: {config}"
        );
        assert!(
            config.contains("args = [\"-y\", \"my-tools\"]"),
            "an unrelated array value must survive intact: {config}"
        );
        // The managed contract is still written.
        assert!(config.contains("[model_providers.aag]"), "{config}");
        assert!(
            config.contains("experimental_bearer_token = \"aag-proxy-token\""),
            "{config}"
        );
        assert!(config.contains("model_provider = \"aag\""), "{config}");

        // A second switch onto an already-merged file must stay idempotent:
        // the managed block is updated in place, not duplicated beside itself.
        write_mirror_proxy_files(
            mirror.to_str(),
            "http://127.0.0.1:8989/v1",
            "aag-proxy-token",
            "gpt-5-codex",
            Codex_mirror_mode::Exclusive,
            false,
        )
        .expect("second switch must succeed");
        let again = std::fs::read_to_string(mirror.join("config.toml")).unwrap();
        assert_eq!(
            again.matches("[model_providers.aag]").count(),
            1,
            "the managed table must not be defined twice: {again}"
        );
        assert!(again.contains("model = \"gpt-5-codex\""), "{again}");
        assert!(again.contains("# I tuned this by hand"), "{again}");
        assert!(again.parse::<toml_edit::DocumentMut>().is_ok());
    }

    /// Mode parsing fails safe to exclusive for unknown/absent values.
    #[test]
    fn mirror_mode_parsing_fails_safe_to_exclusive() {
        assert_eq!(
            Codex_mirror_mode::from_setting(Some("coexist")),
            Codex_mirror_mode::Coexist
        );
        assert_eq!(
            Codex_mirror_mode::from_setting(Some("exclusive")),
            Codex_mirror_mode::Exclusive
        );
        assert_eq!(
            Codex_mirror_mode::from_setting(Some("nonsense")),
            Codex_mirror_mode::Exclusive
        );
        assert_eq!(
            Codex_mirror_mode::from_setting(None),
            Codex_mirror_mode::Exclusive
        );
    }

    /// The native-`~/.codex` guard must recognize the native directory in
    /// every spelling — trailing separators and (on Windows) case — because
    /// a live write into the native directory would clobber the user's
    /// ChatGPT login.
    #[test]
    fn native_codex_dir_is_recognized_in_any_spelling() {
        let native = crate::config::get_codex_config_dir();
        assert!(crate::config::same_dir(&native, &native));
        let trailing = native.as_os_str().to_string_lossy().to_string() + "\\";
        assert!(
            crate::config::same_dir(std::path::Path::new(&trailing), &native),
            "trailing separator must not evade the guard"
        );
        #[cfg(windows)]
        {
            let upper = native.as_os_str().to_string_lossy().to_uppercase();
            assert!(
                crate::config::same_dir(std::path::Path::new(&upper), &native),
                "case differences must not evade the guard"
            );
        }
    }
}
