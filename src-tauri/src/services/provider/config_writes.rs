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

pub(crate) struct MirrorConfigFileUpdate<'a> {
    pub path: &'a Path,
    pub contents: &'a [u8],
    pub app_type: &'static str,
    /// The same admitted bytes used by the caller to construct `contents`.
    pub previous: &'a config::private_fs::MirrorFileSnapshot,
}

struct PendingUpdate {
    path: PathBuf,
    previous: Option<Vec<u8>>,
    contents: Vec<u8>,
    backup: Option<String>,
}

fn require_legacy_config_write(app_type: &str) -> Result<(), AppError> {
    if matches!(app_type, "codex" | "codex_instructions")
        && crate::services::session_authority::client_config::authority_object_present()
            .map_err(AppError::Config)?
    {
        return Err(AppError::SessionClientAuthorityUnavailable);
    }
    Ok(())
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

/// Windows paths are case-insensitive, and the same target can arrive with
/// different spellings (drive-letter case, redundant current-directory
/// components). Compare normalized components so the duplicate-target guard
/// cannot be bypassed that way.
fn same_target(left: &Path, right: &Path) -> bool {
    let key = |path: &Path| -> Vec<String> {
        path.components()
            .map(|component| {
                #[cfg(windows)]
                {
                    component.as_os_str().to_string_lossy().to_lowercase()
                }
                #[cfg(not(windows))]
                {
                    component.as_os_str().to_string_lossy().into_owned()
                }
            })
            .collect()
    };
    key(left) == key(right)
}

/// Validate every target, preserve every prior version, then replace files.
/// If a later write fails, earlier files from this batch are restored.
pub(crate) fn write_files(
    updates: &[ConfigFileUpdate<'_>],
) -> Result<(Vec<String>, Vec<String>), AppError> {
    for update in updates {
        require_legacy_config_write(update.app_type)?;
    }
    let mut pending = Vec::new();
    for update in updates {
        config::private_fs::validate_external_file_path(update.path)?;
        if pending
            .iter()
            .any(|item: &PendingUpdate| same_target(&item.path, update.path))
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

    for (index, item) in pending.iter().enumerate() {
        if let Err(error) = config::atomic_write(&item.path, &item.contents) {
            let rollback_ok = rollback(&pending[..index]);
            return if rollback_ok {
                Err(error)
            } else {
                Err(AppError::Config("client_config_rollback_failed".into()))
            };
        }
    }

    Ok((
        pending
            .iter()
            .map(|item| item.path.to_string_lossy().into_owned())
            .collect(),
        pending.into_iter().filter_map(|item| item.backup).collect(),
    ))
}

#[cfg(any(windows, test))]
struct PendingMirrorUpdate<'a> {
    path: PathBuf,
    previous: &'a config::private_fs::MirrorFileSnapshot,
    contents: Vec<u8>,
    backup: Option<String>,
    applied: Option<config::private_fs::MirrorFileSnapshot>,
}

/// Write only AAG-managed mirror files whose containing directory has already
/// been created and secured. Native clients may rewrite them with the Windows
/// token's Administrators default owner; the mirror filesystem boundary admits
/// that case only with a strict current-user parent and current-user-only grants.
/// Generic client configuration continues to use the stricter `write_files`.
#[cfg(windows)]
pub(crate) fn write_mirror_files(
    updates: &[MirrorConfigFileUpdate<'_>],
) -> Result<(Vec<String>, Vec<String>), AppError> {
    for update in updates {
        require_legacy_config_write(update.app_type)?;
    }
    let mut pending = Vec::new();
    for (index, update) in updates.iter().enumerate() {
        if updates[..index]
            .iter()
            .any(|previous| same_target(previous.path, update.path))
        {
            return Err(AppError::Config("client_config_duplicate_target".into()));
        }
        let previous = update.previous;
        config::private_fs::verify_mirror_snapshot(update.path, previous)?;
        if previous.contents.as_deref() == Some(update.contents) {
            continue;
        }
        let backup = if previous.contents.is_some() {
            let backup = unique_backup(update.path, update.app_type)?;
            config::private_fs::backup_mirror_file(&previous, Path::new(&backup))?;
            Some(backup)
        } else {
            None
        };
        pending.push(PendingMirrorUpdate {
            path: update.path.to_path_buf(),
            previous,
            contents: update.contents.to_vec(),
            backup,
            applied: None,
        });
    }
    for index in 0..pending.len() {
        let item = &mut pending[index];
        match config::private_fs::replace_mirror_file(&item.path, &item.contents, &item.previous) {
            Ok(written) => item.applied = Some(written),
            Err(error) => {
                return if rollback_mirror_files(&pending[..index]) {
                    Err(error)
                } else {
                    Err(AppError::Config("client_config_rollback_failed".into()))
                };
            }
        }
    }
    Ok((
        pending
            .iter()
            .map(|item| item.path.to_string_lossy().into_owned())
            .collect(),
        pending
            .into_iter()
            .flat_map(|item| {
                let recovery = item
                    .applied
                    .and_then(|written| written.recovery_path)
                    .map(|path| path.to_string_lossy().into_owned());
                item.backup.into_iter().chain(recovery)
            })
            .collect(),
    ))
}

/// Unix keeps ordinary write/backup permission handling after the mirror's
/// already-created private parent has been validated.
#[cfg(unix)]
pub(crate) fn write_mirror_files(
    updates: &[MirrorConfigFileUpdate<'_>],
) -> Result<(Vec<String>, Vec<String>), AppError> {
    for update in updates {
        require_legacy_config_write(update.app_type)?;
        config::private_fs::validate_mirror_parent(update.path)?;
        config::private_fs::verify_mirror_snapshot(update.path, update.previous)?;
    }
    let ordinary: Vec<_> = updates
        .iter()
        .map(|update| ConfigFileUpdate {
            path: update.path,
            contents: update.contents,
            app_type: update.app_type,
        })
        .collect();
    write_files(&ordinary)
}

#[cfg(any(windows, test))]
fn rollback_mirror_files(applied: &[PendingMirrorUpdate<'_>]) -> bool {
    let mut restored_all = true;
    for item in applied.iter().rev() {
        let Some(written) = &item.applied else {
            restored_all = false;
            continue;
        };
        let restored = if let Some(previous) = &item.previous.contents {
            config::private_fs::replace_mirror_file(&item.path, previous, written).is_ok()
        } else {
            config::private_fs::remove_mirror_file(&item.path, written).is_ok()
        };
        restored_all &= restored;
    }
    restored_all
}

/// Restore every applied file of the batch, attempting each independently:
/// one unrestorable file must not leave the earlier files half-applied.
/// A file that changed after AAG wrote it is deliberately left alone —
/// restoring would clobber a third-party write, and its private backup
/// remains for manual recovery. Returns true only when every applied file
/// was restored (or removed for files this batch created).
fn rollback(applied: &[PendingUpdate]) -> bool {
    let mut restored_all = true;
    for item in applied.iter().rev() {
        if config::private_fs::validate_external_file_path(&item.path).is_err() {
            restored_all = false;
            continue;
        }
        let still_ours = matches!(fs::read(&item.path), Ok(contents) if contents == item.contents);
        let restored = if !still_ours {
            false
        } else if let Some(previous) = &item.previous {
            config::atomic_write(&item.path, previous).is_ok()
        } else {
            fs::remove_file(&item.path).is_ok()
        };
        if !restored {
            restored_all = false;
        }
    }
    restored_all
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn managed_authority_blocks_generic_codex_writers_before_backups() {
        let home = super::super::test_support::isolated_home("managed-legacy-generic-write");
        let target = home.root.join("legacy_config.toml");
        config::private_fs::atomic_write(&target, b"original").unwrap();
        let previous = config::private_fs::read_mirror_file(&target).unwrap();
        fs::create_dir_all(home.root.join(".codex-cli/.authority")).unwrap();
        let error = write_files(&[ConfigFileUpdate {
            path: &target,
            contents: b"replacement",
            app_type: "codex",
        }])
        .unwrap_err();
        assert_eq!(String::from(error), "session_client_authority_unavailable");
        for app_type in ["codex", "codex_instructions"] {
            let error = write_mirror_files(&[MirrorConfigFileUpdate {
                path: &target,
                contents: b"replacement",
                app_type,
                previous: &previous,
            }])
            .unwrap_err();
            assert_eq!(String::from(error), "session_client_authority_unavailable");
        }
        assert_eq!(fs::read(target).unwrap(), b"original");
        assert!(!home.root.join("aag_state/backups").exists());
    }

    #[cfg(unix)]
    #[test]
    fn unix_mirror_write_keeps_ordinary_private_file_hardening() {
        use std::os::unix::fs::PermissionsExt;
        let home = super::super::test_support::isolated_home("unix_mirror");
        let target = home.root.join("config.json");
        fs::write(&target, b"native rewrite").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
        let previous = config::private_fs::read_mirror_file(&target).unwrap();
        let (_, backups) = write_mirror_files(&[MirrorConfigFileUpdate {
            path: &target,
            contents: b"managed replacement",
            app_type: "test",
            previous: &previous,
        }])
        .unwrap();
        assert_eq!(backups.len(), 1);
        assert_eq!(fs::read(&backups[0]).unwrap(), b"native rewrite");
        config::private_fs::verify_private_file(&target).unwrap();
        config::private_fs::verify_private_file(Path::new(&backups[0])).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"managed replacement");
    }

    #[cfg(windows)]
    #[test]
    fn mirror_writes_back_up_native_default_owner_rewrites_and_remain_repeatable() {
        let home = super::super::test_support::isolated_home("mirror_writes");
        let target = home.root.join("config.json");
        let previous = config::private_fs::read_mirror_file(&target).unwrap();
        write_mirror_files(&[MirrorConfigFileUpdate {
            path: &target,
            contents: b"managed first",
            app_type: "test",
            previous: &previous,
        }])
        .unwrap();
        // Emulate a desktop client's atomic rewrite with the process default owner.
        fs::remove_file(&target).unwrap();
        fs::write(&target, b"native rewrite").unwrap();
        let previous = config::private_fs::read_mirror_file(&target).unwrap();
        let mut updates = [MirrorConfigFileUpdate {
            path: &target,
            contents: b"managed second",
            app_type: "test",
            previous: &previous,
        }];
        let (written, backups) = write_mirror_files(&updates).unwrap();
        assert_eq!(written.len(), 1);
        assert_eq!(backups.len(), 1);
        assert_eq!(fs::read(&backups[0]).unwrap(), b"native rewrite");
        config::private_fs::verify_private_file(Path::new(&backups[0])).unwrap();
        config::private_fs::verify_private_file(&target).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"managed second");
        let latest = config::private_fs::read_mirror_file(&target).unwrap();
        updates[0].previous = &latest;
        assert_eq!(
            write_mirror_files(&updates).unwrap(),
            (Vec::new(), Vec::new())
        );
    }

    #[test]
    fn mirror_rollback_preserves_replaced_files_and_restores_other_applied_files() {
        let home = super::super::test_support::isolated_home("mirror_rollback");
        let paths = [home.root.join("a.json"), home.root.join("b.json")];
        for path in &paths {
            config::atomic_write(path, b"original").unwrap();
        }
        let snapshots: Vec<_> = paths
            .iter()
            .map(|path| config::private_fs::read_mirror_file(path).unwrap())
            .collect();
        let mut pending = Vec::new();
        for (path, previous) in paths.into_iter().zip(&snapshots) {
            let applied =
                config::private_fs::replace_mirror_file(&path, b"updated", previous).unwrap();
            pending.push(PendingMirrorUpdate {
                path,
                previous,
                contents: b"updated".to_vec(),
                backup: None,
                applied: Some(applied),
            });
        }
        // Even identical bytes in a different object are a third-party version.
        config::atomic_write(&pending[0].path, b"updated").unwrap();
        assert!(!rollback_mirror_files(&pending));
        assert_eq!(fs::read(&pending[0].path).unwrap(), b"updated");
        assert_eq!(fs::read(&pending[1].path).unwrap(), b"original");
    }

    #[test]
    fn mirror_writes_reject_an_unprepared_parent_without_creating_it() {
        let home = super::super::test_support::isolated_home("mirror_parent");
        let parent = home.root.join("missing");
        let target = parent.join("config.json");
        let previous = config::private_fs::read_mirror_file(&home.root.join("other.json")).unwrap();
        assert!(write_mirror_files(&[MirrorConfigFileUpdate {
            path: &target,
            contents: b"never written",
            app_type: "test",
            previous: &previous,
        }])
        .is_err());
        assert!(!parent.exists());
    }

    #[test]
    fn mirror_writer_rejects_a_transform_derived_from_an_older_snapshot() {
        let home = super::super::test_support::isolated_home("mirror_stale_transform");
        let target = home.root.join("config.json");
        config::atomic_write(&target, b"before transform").unwrap();
        let previous = config::private_fs::read_mirror_file(&target).unwrap();
        config::atomic_write(&target, b"new client content").unwrap();
        assert!(write_mirror_files(&[MirrorConfigFileUpdate {
            path: &target,
            contents: b"derived from before transform",
            app_type: "test",
            previous: &previous,
        }])
        .is_err());
        assert_eq!(fs::read(&target).unwrap(), b"new client content");
    }

    struct EnvRestore(Option<std::ffi::OsString>, Option<std::ffi::OsString>);

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            match &self.0 {
                Some(value) => std::env::set_var("AAG_HOME_DIR", value),
                None => std::env::remove_var("AAG_HOME_DIR"),
            }
            match &self.1 {
                Some(value) => std::env::set_var("AAG_CONFIG_DIR", value),
                None => std::env::remove_var("AAG_CONFIG_DIR"),
            }
        }
    }

    /// Hardened isolated root (owner-only), so the external-parent ACL
    /// checks the write path performs also hold on hosts whose shared
    /// temporary directory carries untrusted writer ACEs.
    fn isolated_root() -> (EnvRestore, PathBuf) {
        let env = EnvRestore(
            std::env::var_os("AAG_HOME_DIR"),
            std::env::var_os("AAG_CONFIG_DIR"),
        );
        let root = std::env::temp_dir().join(format!(
            "aag-config-writes-test-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        config::private_fs::prepare_managed_private_dir(&root).unwrap();
        std::env::set_var("AAG_HOME_DIR", &root);
        std::env::set_var("AAG_CONFIG_DIR", root.join("aag_state"));
        (env, root)
    }

    #[test]
    fn duplicate_targets_are_rejected_across_spellings() {
        let _lock = crate::config::test_env_lock().blocking_lock();
        let (_restore, root) = isolated_root();
        let target = root.join("settings.json");
        let aliased = root.join(".").join("settings.json");
        let mut updates = vec![
            ConfigFileUpdate {
                path: &target,
                contents: b"a",
                app_type: "test",
            },
            ConfigFileUpdate {
                path: &aliased,
                contents: b"b",
                app_type: "test",
            },
        ];
        let error = write_files(&updates).unwrap_err();
        assert!(error.to_string().contains("client_config_duplicate_target"));

        #[cfg(windows)]
        {
            let uppercase = PathBuf::from(target.to_string_lossy().to_uppercase());
            updates[1].path = &uppercase;
            assert!(write_files(&updates).is_err());
        }
        assert!(!target.exists());
    }

    #[test]
    fn identical_contents_are_skipped_without_a_new_backup() {
        let _lock = crate::config::test_env_lock().blocking_lock();
        let (_restore, root) = isolated_root();
        let target = root.join("settings.json");
        let first = vec![ConfigFileUpdate {
            path: &target,
            contents: b"{\"a\":1}",
            app_type: "test",
        }];
        let (written, backups) = write_files(&first).unwrap();
        assert_eq!(written, vec![target.to_string_lossy().into_owned()]);
        assert!(backups.is_empty(), "a created file needs no backup");

        let second = vec![ConfigFileUpdate {
            path: &target,
            contents: b"{\"a\":2}",
            app_type: "test",
        }];
        let (_written, backups) = write_files(&second).unwrap();
        assert_eq!(backups.len(), 1, "an overwritten file is backed up");

        // Repeating the identical contents must not write again and must
        // not create a redundant backup: no new backup is reported and the
        // backups directory still holds exactly the one file.
        let (written, new_backups) = write_files(&second).unwrap();
        assert!(written.is_empty());
        assert!(new_backups.is_empty(), "identical contents need no backup");
        let backups_dir = root.join("aag_state/backups");
        assert_eq!(fs::read_dir(&backups_dir).unwrap().count(), 1);
    }

    #[test]
    fn rollback_restores_every_untouched_file_and_leaves_third_party_writes() {
        let _lock = crate::config::test_env_lock().blocking_lock();
        let (_restore, root) = isolated_root();
        let file_a = root.join("a.json");
        let file_b = root.join("b.json");
        fs::write(&file_a, b"original-a").unwrap();
        fs::write(&file_b, b"original-b").unwrap();
        let pending = vec![
            PendingUpdate {
                path: file_a.clone(),
                previous: Some(b"original-a".to_vec()),
                contents: b"new-a".to_vec(),
                backup: None,
            },
            PendingUpdate {
                path: file_b.clone(),
                previous: Some(b"original-b".to_vec()),
                contents: b"new-b".to_vec(),
                backup: None,
            },
        ];
        config::atomic_write(&file_a, b"new-a").unwrap();
        config::atomic_write(&file_b, b"new-b").unwrap();
        // A third party rewrote file_a after AAG applied it: rollback must
        // still restore file_b instead of aborting on the first mismatch.
        config::atomic_write(&file_a, b"third-party").unwrap();
        assert!(!rollback(&pending));
        assert_eq!(fs::read(&file_a).unwrap(), b"third-party");
        assert_eq!(fs::read(&file_b).unwrap(), b"original-b");
    }

    #[test]
    fn rollback_removes_files_the_batch_created() {
        let _lock = crate::config::test_env_lock().blocking_lock();
        let (_restore, root) = isolated_root();
        let created = root.join("created.json");
        let pending = PendingUpdate {
            path: created.clone(),
            previous: None,
            contents: b"created".to_vec(),
            backup: None,
        };
        config::atomic_write(&created, b"created").unwrap();
        assert!(rollback(&[pending]));
        assert!(!created.exists());
    }
}
