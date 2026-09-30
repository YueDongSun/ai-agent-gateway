//! Safe, recoverable writes to user-owned client configuration files.
use crate::{config, error::AppError};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub(crate) struct ConfigFileUpdate<'a> {
    pub path: &'a Path,
    pub contents: &'a [u8],
    pub app_type: &'static str,
}

struct PendingUpdate {
    path: PathBuf,
    previous: Option<Vec<u8>>,
    contents: Vec<u8>,
    backup: Option<String>,
}

fn unique_backup(path: &Path, app_type: &str) -> Result<String, AppError> {
    let mut nonce = [0u8; 12];
    getrandom::getrandom(&mut nonce)
        .map_err(|_| AppError::Config("client_config_backup_random_unavailable".into()))?;
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("client-config");
    let filename: String = filename
        .chars()
        .take(64)
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect();
    Ok(config::get_backups_dir()?
        .join(format!(
            "{app_type}.{filename}.{}.{}.bak",
            chrono::Utc::now().timestamp(),
            hex::encode(nonce)
        ))
        .to_string_lossy()
        .into_owned())
}

/// Validate every target, preserve every prior version, then replace files.
/// If a later write fails, earlier files from this batch are restored.
pub(crate) fn write_files(
    updates: &[ConfigFileUpdate<'_>],
) -> Result<(Vec<String>, Vec<String>), AppError> {
    let mut pending = Vec::new();
    for update in updates {
        config::private_fs::validate_external_file_path(update.path)?;
        if pending
            .iter()
            .any(|item: &PendingUpdate| item.path == update.path)
        {
            return Err(AppError::Config("client_config_duplicate_target".into()));
        }
        let previous = match fs::read(update.path) {
            Ok(contents) => Some(contents),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(AppError::io(update.path, error)),
        };
        if previous.as_deref() == Some(update.contents) {
            continue;
        }
        let backup = match previous {
            Some(_) => {
                let backup = unique_backup(update.path, update.app_type)?;
                config::backup_file(update.path, Path::new(&backup))?;
                Some(backup)
            }
            None => None,
        };
        pending.push(PendingUpdate {
            path: update.path.to_path_buf(),
            previous,
            contents: update.contents.to_vec(),
            backup,
        });
    }

    let mut applied = 0usize;
    for item in &pending {
        if let Err(error) = config::atomic_write(&item.path, &item.contents) {
            let rollback_ok = rollback(&pending[..applied]);
            return if rollback_ok {
                Err(error)
            } else {
                Err(AppError::Config("client_config_rollback_failed".into()))
            };
        }
        applied += 1;
    }

    Ok((
        pending
            .iter()
            .map(|item| item.path.to_string_lossy().into_owned())
            .collect(),
        pending.into_iter().filter_map(|item| item.backup).collect(),
    ))
}

fn rollback(applied: &[PendingUpdate]) -> bool {
    for item in applied.iter().rev() {
        if config::private_fs::validate_external_file_path(&item.path).is_err() {
            return false;
        }
        match fs::read(&item.path) {
            Ok(contents) if contents == item.contents => {}
            _ => return false,
        }
        if let Some(previous) = &item.previous {
            if config::atomic_write(&item.path, previous).is_err() {
                return false;
            }
        } else if fs::remove_file(&item.path).is_err() {
            return false;
        }
    }
    true
}
