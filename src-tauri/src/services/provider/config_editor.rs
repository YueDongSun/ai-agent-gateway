//! Direct text editing of a client's live configuration file
//! (cc-switch-style "edit JSON"), scoped to the Kilo family.
//!
//! Read returns the file verbatim so JSONC comments survive round-trip;
//! save validates the edited text (comments stripped for Kilo's JSONC,
//! parsed root must be an object) and then writes the user's exact bytes
//! through the standard hardening pipeline: parent directory is secured
//! to owner-only, the previous file is backed up under `~/.aag/backups/`,
//! and the replacement is atomic with rollback on failure
//! ([`super::config_writes::write_files`]).
//!
//! Only `kilo_cli` and `kilo_vscode` are editable in this surface; both
//! resolve through [`super::config_paths::resolve`] to the same single
//! candidate file (first existing, else the highest-priority `kilo.jsonc`).
//! Other app types return `AAG_CLIENT_CONFIG_EDIT_UNSUPPORTED` instead of a
//! silent no-op, so the UI can hide the affordance rather than show a
//! button that fails.

use crate::error::AppError;
use serde::Serialize;
use std::path::PathBuf;

use super::{config_paths, config_writes, AppType};

/// Result of a direct-edit read: the resolved target plus its current bytes.
#[allow(non_camel_case_types)]
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Config_edit_target {
    app_type: String,
    /// Absolute path the editor will write.
    path: String,
    /// Verbatim file contents, or an empty string when the file does not
    /// exist yet (the editor starts from a `{}` skeleton).
    contents: String,
    /// True when the file already exists on disk.
    existing: bool,
}

/// App types whose live config this surface may edit. Every other app
/// keeps its existing switch/mirror write path only.
const EDITABLE_APP_TYPES: [AppType; 2] = [AppType::KiloCli, AppType::KiloVsCode];

/// Upper bound for editor text crossing IPC, matching the client-config
/// bound on the MCP sync surface (`services/mcp/client_config.rs`). A
/// replaced or runaway client file must never be fully buffered or shipped.
const MAX_EDIT_BYTES: usize = 2 * 1024 * 1024;

fn edit_too_large() -> AppError {
    AppError::Config("AAG_CLIENT_CONFIG_EDIT_TOO_LARGE".into())
}

fn editable_target(app_type: &str) -> Result<(AppType, PathBuf), AppError> {
    let app = AppType::from_db_str(app_type)?;
    if !EDITABLE_APP_TYPES.contains(&app) {
        return Err(AppError::Config(
            "AAG_CLIENT_CONFIG_EDIT_UNSUPPORTED".into(),
        ));
    }
    // Both Kilo variants resolve through the same candidate list; the
    // first existing file is the live one, otherwise the writer creates
    // the highest-priority `kilo.jsonc`.
    let resolved = config_paths::resolve(app.to_db_str())?;
    let [path] = resolved.paths.as_slice() else {
        return Err(AppError::Config("AAG_CLIENT_CONFIG_PATHS_INVALID".into()));
    };
    Ok((app, path.clone()))
}

/// Read the live config file for direct editing. The bytes are returned
/// verbatim — comment round-trip matters for `kilo.jsonc`. Files beyond
/// the editor bound are refused before any unbounded buffering; the
/// handle-bound length check plus the capped read cover a file that
/// grows between stat and read.
pub fn client_config_read(app_type: &str) -> Result<Config_edit_target, AppError> {
    use std::io::Read;
    let (app, path) = editable_target(app_type)?;
    crate::config::private_fs::validate_external_file_path(&path)?;
    let existing = path.is_file();
    let contents = if existing {
        let file = std::fs::File::open(&path).map_err(|error| AppError::io(&path, error))?;
        let metadata = file
            .metadata()
            .map_err(|error| AppError::io(&path, error))?;
        if metadata.len() > MAX_EDIT_BYTES as u64 {
            return Err(edit_too_large());
        }
        let mut contents = String::new();
        file.take(MAX_EDIT_BYTES as u64 + 1)
            .read_to_string(&mut contents)
            .map_err(|error| AppError::io(&path, error))?;
        if contents.len() > MAX_EDIT_BYTES {
            return Err(edit_too_large());
        }
        contents
    } else {
        String::new()
    };
    Ok(Config_edit_target {
        app_type: app.to_db_str().into(),
        path: path.to_string_lossy().into_owned(),
        contents,
        existing,
    })
}

/// Validate edited text and write it back to the live file.
///
/// Validation strips `//` and `/* */` comments before parsing (Kilo
/// configs are JSONC) and requires the root to be a JSON object — the
/// same fail-closed contract as [`super::kilo::write_live_at`]. The
/// user's text is then written byte-for-byte (comments preserved).
pub fn client_config_save(
    app_type: &str,
    contents: &str,
) -> Result<super::LiveWriteResult, AppError> {
    let (app, path) = editable_target(app_type)?;
    if contents.len() > MAX_EDIT_BYTES {
        return Err(edit_too_large());
    }
    // Reject non-UTF8-only concerns early: parse the stripped text.
    let stripped = strip_jsonc_comments(contents);
    // Code-only: details never cross IPC (stable_detail drops mixed-case
    // text) and the UI already validates JSON client-side.
    let parsed: serde_json::Value = serde_json::from_str(&stripped)
        .map_err(|_| AppError::Config("AAG_CLIENT_CONFIG_EDIT_INVALID_JSON".into()))?;
    if !parsed.is_object() {
        return Err(AppError::Config(
            "AAG_CLIENT_CONFIG_EDIT_ROOT_NOT_OBJECT".into(),
        ));
    }
    // Harden the parent (typically `~/.config/kilo` or `~/.kilo`) to
    // owner-only, mirroring `kilo::write_live_at`. Without this, Windows
    // profile dirs carrying untrusted Modify grants fail
    // `external_directory_write_unsafe` at write time.
    if let Some(parent) = path.parent() {
        crate::config::private_fs::prepare_managed_private_dir(parent)?;
    }
    let (written, backups) = config_writes::write_files(&[config_writes::ConfigFileUpdate {
        path: &path,
        contents: contents.as_bytes(),
        app_type: app.to_db_str(),
    }])?;
    Ok(super::LiveWriteResult {
        app_type: app.to_db_str().into(),
        provider_id: String::new(),
        written,
        backup: backups.into_iter().next(),
    })
}

/// Strip JSONC `//` line and `/* */` block comments. Shared by the direct
/// editor and (via re-export) the adapters that parse JSONC, replacing the
/// per-adapter copies (kilo/opencode/copilot each carried one).
pub(crate) fn strip_jsonc_comments(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                chars.next();
                for ch in chars.by_ref() {
                    if ch == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                while let Some(ch) = chars.next() {
                    if ch == '*' && chars.peek() == Some(&'/') {
                        chars.next();
                        break;
                    }
                }
            }
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::provider::test_support::isolated_home;

    fn read_target(app_type: &str) -> Config_edit_target {
        client_config_read(app_type).expect("read config edit target")
    }

    #[test]
    fn read_returns_verbatim_contents_with_comments() {
        let home = isolated_home("kilo-edit-read");
        std::fs::create_dir_all(home.root.join(".config").join("kilo")).unwrap();
        std::fs::write(
            home.root.join(".config").join("kilo").join("kilo.jsonc"),
            "{\n  // provider map\n  \"theme\": \"dark\"\n}\n",
        )
        .unwrap();

        let target = read_target("kilo_cli");
        assert!(target.existing);
        assert!(target.path.ends_with("kilo.jsonc"), "{}", target.path);
        assert!(target.contents.contains("// provider map"));
        assert!(target.contents.contains("\"theme\": \"dark\""));
    }

    #[test]
    fn read_missing_file_returns_empty_not_created() {
        let home = isolated_home("kilo-edit-missing");
        let target = read_target("kilo_cli");
        assert!(!target.existing);
        assert_eq!(target.contents, "");
        assert!(
            !home
                .root
                .join(".config")
                .join("kilo")
                .join("kilo.jsonc")
                .exists(),
            "read must not create the file"
        );
    }

    #[test]
    fn save_writes_jsonc_verbatim_and_creates_backup() {
        let home = isolated_home("kilo-edit-save");
        let kilo_dir = home.root.join(".config").join("kilo");
        std::fs::create_dir_all(&kilo_dir).unwrap();
        std::fs::write(
            kilo_dir.join("kilo.jsonc"),
            "{\n  \"theme\": \"light\"\n}\n",
        )
        .unwrap();

        let edited = "{\n  // dark mode\n  \"theme\": \"dark\"\n}\n";
        let result = client_config_save("kilo_cli", edited).expect("save edited config");
        assert_eq!(result.written.len(), 1);
        assert!(result.backup.is_some(), "existing file must be backed up");

        let on_disk =
            std::fs::read_to_string(kilo_dir.join("kilo.jsonc")).expect("read back config");
        assert_eq!(on_disk, edited, "comments must round-trip verbatim");
    }

    #[test]
    fn editor_failures_surface_specific_stable_ipc_codes() {
        // Each failure mode must cross IPC as its own stable code; the
        // generic configuration_error fallback would hide the cause from
        // the editor UI (which renders the external message verbatim).
        let _home = isolated_home("kilo-edit-codes");
        let unsupported = client_config_read("claude_code").expect_err("scoped read");
        assert_eq!(
            unsupported.external_message(),
            "AAG_CLIENT_CONFIG_EDIT_UNSUPPORTED"
        );

        let invalid_json = client_config_save("kilo_cli", "{ not json").expect_err("invalid JSON");
        assert_eq!(
            invalid_json.external_message(),
            "AAG_CLIENT_CONFIG_EDIT_INVALID_JSON"
        );

        let not_object = client_config_save("kilo_cli", "[1, 2]").expect_err("array root");
        assert_eq!(
            not_object.external_message(),
            "AAG_CLIENT_CONFIG_EDIT_ROOT_NOT_OBJECT"
        );

        let oversized = format!("{{\"padding\":\"{}\"}}", "x".repeat(2 * 1024 * 1024));
        let too_large = client_config_save("kilo_cli", &oversized).expect_err("oversized save");
        assert_eq!(
            too_large.external_message(),
            "AAG_CLIENT_CONFIG_EDIT_TOO_LARGE"
        );
    }

    #[test]
    fn save_rejects_invalid_json_and_leaves_file_untouched() {
        let home = isolated_home("kilo-edit-invalid");
        let kilo_dir = home.root.join(".config").join("kilo");
        std::fs::create_dir_all(&kilo_dir).unwrap();
        std::fs::write(
            kilo_dir.join("kilo.jsonc"),
            "{\n  \"theme\": \"light\"\n}\n",
        )
        .unwrap();

        let err = client_config_save("kilo_cli", "{ not json").expect_err("invalid JSON");
        assert!(
            err.to_string()
                .contains("AAG_CLIENT_CONFIG_EDIT_INVALID_JSON"),
            "{err}"
        );
        assert_eq!(
            std::fs::read_to_string(kilo_dir.join("kilo.jsonc")).unwrap(),
            "{\n  \"theme\": \"light\"\n}\n",
            "file must be unchanged after a rejected save"
        );
    }

    #[test]
    fn read_rejects_an_oversized_existing_config() {
        let home = isolated_home("kilo-edit-read-large");
        let kilo_dir = home.root.join(".config").join("kilo");
        std::fs::create_dir_all(&kilo_dir).unwrap();
        std::fs::write(kilo_dir.join("kilo.jsonc"), "x".repeat(2 * 1024 * 1024 + 1)).unwrap();

        let err = client_config_read("kilo_cli").expect_err("oversized read");
        assert!(
            err.to_string().contains("AAG_CLIENT_CONFIG_EDIT_TOO_LARGE"),
            "{err}"
        );
    }

    #[test]
    fn save_rejects_an_oversized_submission_without_touching_the_target() {
        let home = isolated_home("kilo-edit-save-large");
        let kilo_dir = home.root.join(".config").join("kilo");
        std::fs::create_dir_all(&kilo_dir).unwrap();
        std::fs::write(
            kilo_dir.join("kilo.jsonc"),
            "{\n  \"theme\": \"light\"\n}\n",
        )
        .unwrap();

        let oversized = format!("{{\"padding\":\"{}\"}}", "x".repeat(2 * 1024 * 1024));
        let err = client_config_save("kilo_cli", &oversized).expect_err("oversized save");
        assert!(
            err.to_string().contains("AAG_CLIENT_CONFIG_EDIT_TOO_LARGE"),
            "{err}"
        );
        assert_eq!(
            std::fs::read_to_string(kilo_dir.join("kilo.jsonc")).unwrap(),
            "{\n  \"theme\": \"light\"\n}\n",
            "file must be unchanged after a rejected save"
        );
    }

    #[test]
    fn save_rejects_non_object_root() {
        let _home = isolated_home("kilo-edit-array");
        let err = client_config_save("kilo_cli", "[1, 2]").expect_err("array root");
        assert!(
            err.to_string()
                .contains("AAG_CLIENT_CONFIG_EDIT_ROOT_NOT_OBJECT"),
            "{err}"
        );
    }

    #[test]
    fn save_rejects_empty_document() {
        let _home = isolated_home("kilo-edit-empty");
        let err = client_config_save("kilo_cli", "").expect_err("empty document");
        assert!(
            err.to_string()
                .contains("AAG_CLIENT_CONFIG_EDIT_INVALID_JSON"),
            "{err}"
        );
    }

    #[test]
    fn unsupported_app_type_fails_closed_before_any_io() {
        let _home = isolated_home("kilo-edit-unsupported");
        // Valid app types outside the Kilo family: scoped out with the
        // dedicated error.
        for app_type in ["claude_code", "codex", "opencode", "gemini"] {
            let err = client_config_read(app_type).expect_err("read must be scoped");
            assert!(
                err.to_string()
                    .contains("AAG_CLIENT_CONFIG_EDIT_UNSUPPORTED"),
                "{app_type}: {err}"
            );
            let err = client_config_save(app_type, "{}").expect_err("save must be scoped");
            assert!(
                err.to_string()
                    .contains("AAG_CLIENT_CONFIG_EDIT_UNSUPPORTED"),
                "{app_type}: {err}"
            );
        }
        // Unknown app_type strings are rejected by AppType parsing itself.
        let err = client_config_read("bogus").expect_err("unknown app_type");
        assert!(err.to_string().contains("unknown app_type"), "{err}");
        let err = client_config_save("bogus", "{}").expect_err("unknown app_type");
        assert!(err.to_string().contains("unknown app_type"), "{err}");
    }

    #[test]
    fn kilo_vscode_resolves_the_same_candidate_file() {
        let home = isolated_home("kilo-edit-vscode");
        let kilo_dir = home.root.join(".config").join("kilo");
        std::fs::create_dir_all(&kilo_dir).unwrap();
        std::fs::write(kilo_dir.join("kilo.jsonc"), "{}").unwrap();

        let cli = read_target("kilo_cli");
        let vscode = read_target("kilo_vscode");
        assert_eq!(
            cli.path, vscode.path,
            "both Kilo variants share the live file"
        );
    }

    #[test]
    fn strip_jsonc_comments_handles_strings_and_nested_markers() {
        // String contents that look like comment markers must survive.
        let raw = r#"{"a": "http://x", "b": "//not-a-comment", "c": 1} // tail"#;
        let stripped = strip_jsonc_comments(raw);
        let parsed: serde_json::Value = serde_json::from_str(&stripped).expect("valid after strip");
        assert_eq!(parsed["a"], "http://x");
        assert_eq!(parsed["b"], "//not-a-comment");
        // Block comments too.
        let blocky = strip_jsonc_comments("{ /* c1 */ \"k\": 1 /* c2 */ }");
        let parsed: serde_json::Value = serde_json::from_str(&blocky).expect("valid after strip");
        assert_eq!(parsed["k"], 1);
    }

    /// Shared JSONC-parity fixture: the same cases run against the
    /// TypeScript mirror (`ui/src/lib/jsonc.ts`) in
    /// `tests/jsonc_parity.test.ts`, so the two parsers cannot silently
    /// drift. Add cases to the fixture — never inline here.
    #[test]
    fn shared_fixture_cases() {
        #[derive(serde::Deserialize)]
        struct Case {
            name: String,
            raw: String,
            parses: bool,
            #[serde(default)]
            contains: Vec<String>,
            #[serde(rename = "not_contains", default)]
            not_contains: Vec<String>,
        }
        #[derive(serde::Deserialize)]
        struct Fixture {
            cases: Vec<Case>,
        }
        let fixture: Fixture =
            serde_json::from_str(include_str!("../../../../tests/fixtures/jsonc_cases.json"))
                .expect("fixture parses");
        assert!(!fixture.cases.is_empty(), "fixture must carry cases");
        for case in &fixture.cases {
            let stripped = strip_jsonc_comments(&case.raw);
            for needle in &case.contains {
                assert!(
                    stripped.contains(needle.as_str()),
                    "{}: stripped must contain {needle:?} — got {stripped:?}",
                    case.name
                );
            }
            for needle in &case.not_contains {
                assert!(
                    !stripped.contains(needle.as_str()),
                    "{}: stripped must not contain {needle:?} — got {stripped:?}",
                    case.name
                );
            }
            let parsed = serde_json::from_str::<serde_json::Value>(&stripped).is_ok();
            assert_eq!(
                parsed, case.parses,
                "{}: parse outcome mismatch — stripped {stripped:?}",
                case.name
            );
        }
    }
}
