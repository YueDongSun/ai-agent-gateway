//! Read-only discovery of the user configuration files the adapters write.
//!
//! CLI config roots retain their upstream conventions on Windows, macOS and
//! Linux; they are not Electron application-data directories. Explicit paths
//! must be absolute and safe. A portable/test AAG_HOME_DIR ignores inherited
//! overrides outside that home, while allowing fixture paths inside it.
//!
//! Upstream path contracts:
//! - https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-config-dir-reference
//! - https://github.com/earendil-works/pi/blob/main/packages/coding-agent/src/config.ts
//! - https://geminicli.com/docs/reference/configuration/
//! - https://opencode.ai/docs/config/
//! - https://kilo.ai/docs/contributing/architecture/cli-runtime

use std::{
    ffi::OsString,
    fs,
    path::{Component, Path, PathBuf},
};

use crate::{config::private_fs, error::AppError};

#[cfg(test)]
#[path = "config_paths_tests.rs"]
mod tests;

#[allow(non_camel_case_types)]
#[derive(Debug)]
pub(crate) struct Resolved_config_paths {
    /// Adapter order: Pi models/settings; Gemini env/settings; others one file.
    pub paths: Vec<PathBuf>,
    /// Stable UI category, containing neither environment values nor contents.
    pub source: &'static str,
    /// At least one of the selected files already exists.
    pub existing: bool,
}

pub(crate) fn resolve(app_type: &str) -> Result<Resolved_config_paths, AppError> {
    resolve_with_environment(
        app_type,
        &crate::config::get_home_dir(),
        std::env::var_os("AAG_HOME_DIR")
            .is_some_and(|value| !value.to_string_lossy().trim().is_empty()),
        |name| std::env::var_os(name),
    )
}

fn failure(code: &str) -> AppError {
    AppError::Config(format!(
        "AAG_CLIENT_CONFIG_PATH_{}",
        code.to_ascii_uppercase()
    ))
}

fn validate_spelling(path: &Path) -> Result<(), AppError> {
    if !path.is_absolute() {
        return Err(failure("not_absolute"));
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(failure("parent_traversal"));
    }
    if path.to_string_lossy().chars().any(char::is_control) {
        return Err(failure("control_character"));
    }
    Ok(())
}

fn environment_path(
    name: &str,
    home: &Path,
    isolated: bool,
    environment: &impl Fn(&str) -> Option<OsString>,
) -> Result<Option<PathBuf>, AppError> {
    let Some(value) = environment(name).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let path = PathBuf::from(value);
    validate_spelling(&path)?;
    if isolated && !starts_with_home(&path, home) {
        return Ok(None);
    }
    Ok(Some(path))
}

/// Component-wise containment check. Windows paths are case-insensitive, so
/// the isolation boundary must fold case there (`config::same_dir`
/// precedent); `Path::starts_with` alone would silently drop a portable-run
/// override that only differs by drive-letter or directory case.
fn starts_with_home(path: &Path, home: &Path) -> bool {
    #[cfg(windows)]
    {
        let mut path_components = path
            .components()
            .map(|component| component.as_os_str().to_string_lossy().to_lowercase());
        home.components().all(|home_component| {
            path_components.next()
                == Some(home_component.as_os_str().to_string_lossy().to_lowercase())
        })
    }
    #[cfg(not(windows))]
    {
        path.starts_with(home)
    }
}

fn file_exists(path: &Path) -> Result<bool, AppError> {
    private_fs::validate_external_file_path(path)?;
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(failure("metadata_unavailable")),
    }
}

fn selected_files(
    paths: Vec<PathBuf>,
    from_environment: bool,
) -> Result<Resolved_config_paths, AppError> {
    let mut existing = false;
    for path in &paths {
        existing |= file_exists(path)?;
    }
    Ok(Resolved_config_paths {
        paths,
        source: if from_environment {
            "environment"
        } else if existing {
            "existing"
        } else {
            "default"
        },
        existing,
    })
}

fn first_existing(
    candidates: Vec<PathBuf>,
    default: PathBuf,
    from_environment: bool,
) -> Result<Resolved_config_paths, AppError> {
    for path in candidates {
        if file_exists(&path)? {
            return selected_files(vec![path], from_environment);
        }
    }
    selected_files(vec![default], from_environment)
}

fn resolve_with_environment(
    app_type: &str,
    home: &Path,
    isolated: bool,
    environment: impl Fn(&str) -> Option<OsString>,
) -> Result<Resolved_config_paths, AppError> {
    validate_spelling(home)?;
    let override_path = |name| environment_path(name, home, isolated, &environment);

    match app_type {
        "copilot" | "pi" | "gemini" => {
            let (variable, default, names): (&str, PathBuf, &[&str]) = match app_type {
                "copilot" => ("COPILOT_HOME", home.join(".copilot"), &["settings.json"]),
                "pi" => (
                    "PI_CODING_AGENT_DIR",
                    home.join(".pi").join("agent"),
                    &["models.json", "settings.json"],
                ),
                _ => (
                    "GEMINI_CLI_HOME",
                    home.to_path_buf(),
                    &[".env", "settings.json"],
                ),
            };
            let custom = override_path(variable)?;
            let from_environment = custom.is_some();
            let directory = custom.unwrap_or(default);
            // GEMINI_CLI_HOME replaces the home root, not the .gemini directory.
            let directory = if app_type == "gemini" {
                directory.join(".gemini")
            } else {
                directory
            };
            selected_files(
                names.iter().map(|name| directory.join(name)).collect(),
                from_environment,
            )
        }
        "cline_cli" => {
            // Cline resolves providers.json beneath CLINE_DATA_DIR/settings.
            // AAG gives it a separate data directory so native ~/.cline stays
            // owned by Cline and existing native credentials remain isolated.
            let directory = home.join(".cline_cli").join("data").join("settings");
            selected_files(
                vec![
                    directory.join("providers.json"),
                    directory.join("models.json"),
                ],
                false,
            )
        }
        "opencode" | "kilo_cli" | "kilo_vscode" => {
            let is_kilo = app_type != "opencode";
            let (file_variable, directory_variable, directory_name, names, default_name): (
                &str,
                &str,
                &str,
                &[&str],
                &str,
            ) = if is_kilo {
                (
                    "KILO_CONFIG",
                    "KILO_CONFIG_DIR",
                    "kilo",
                    &["kilo.jsonc", "kilo.json", "opencode.jsonc", "opencode.json"],
                    "kilo.jsonc",
                )
            } else {
                (
                    "OPENCODE_CONFIG",
                    "OPENCODE_CONFIG_DIR",
                    "opencode",
                    &["opencode.jsonc", "opencode.json"],
                    "opencode.json",
                )
            };
            let custom_file = override_path(file_variable)?;
            let custom_directory = override_path(directory_variable)?;
            // Upstream custom-directory configs load after custom-file configs.
            // Writing that explicit layer ensures the applied choice takes effect.
            if let Some(directory) = custom_directory {
                return first_existing(
                    names.iter().map(|name| directory.join(name)).collect(),
                    directory.join(default_name),
                    true,
                );
            }
            if let Some(path) = custom_file {
                return selected_files(vec![path], true);
            }
            let xdg = override_path("XDG_CONFIG_HOME")?;
            let from_environment = xdg.is_some();
            let directory = xdg
                .unwrap_or_else(|| home.join(".config"))
                .join(directory_name);
            let mut candidates: Vec<PathBuf> =
                names.iter().map(|name| directory.join(name)).collect();
            // Preserve the existing Kilo compatibility candidate only when the
            // user has not selected a different global configuration root.
            // Stepwise joins keep the displayed spelling uniform on Windows.
            if is_kilo && !from_environment {
                candidates.push(home.join(".kilo").join("config.json"));
            }
            first_existing(candidates, directory.join(default_name), from_environment)
        }
        _ => Err(failure("unsupported_app")),
    }
}
