//! Managed filesystem and child-environment preparation, separate from process
//! ownership. Constructing a command is read-only; callers prepare directories
//! only while holding the instance lock for an explicit execution operation.
use super::config::{self, failure};
use crate::error::AppError;
use std::{
    collections::HashSet,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

const INHERITED_OS_VARIABLES: &[&str] = &[
    "PATH",
    "SystemRoot",
    "WINDIR",
    "COMSPEC",
    "PATHEXT",
    "TEMP",
    "TMP",
    "LANG",
    "LC_ALL",
];
// These are external OS/runtime contracts, not project-controlled names.
const HOME_VARIABLES: &[(&str, &str)] = &[
    ("HOME", ""),
    ("USERPROFILE", ""),
    ("APPDATA", "AppData/Roaming"),
    ("LOCALAPPDATA", "AppData/Local"),
    ("XDG_CONFIG_HOME", ".config"),
    ("XDG_CACHE_HOME", ".cache"),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Directory_state {
    Missing,
    Existing_directory,
    Blocked_by_file,
}

pub(super) struct Runtime_environment {
    root: PathBuf,
    home: PathBuf,
    pub(super) workspace: PathBuf,
}

impl Runtime_environment {
    pub(super) fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            home: root.join("user_home"),
            workspace: root.join("workspace"),
        }
    }

    pub(super) fn prepare(&self) -> Result<(), AppError> {
        let mut directories = vec![self.workspace.clone()];
        directories.extend(
            HOME_VARIABLES
                .iter()
                .map(|(_, relative)| self.home.join(relative)),
        );
        // The Windows folder picker resolves Desktop against USERPROFILE even
        // for an isolated DSH home. It must exist before the child can open it.
        #[cfg(windows)]
        directories.push(self.home.join("Desktop"));

        // Reject an existing linked path before creating any directory.
        // Never replace user content to repair an incompatible managed home.
        // The directories share the root/user_home ancestor chain, so each
        // component is checked once instead of re-walking per directory.
        let mut checked_ancestors: HashSet<PathBuf> = HashSet::new();
        for directory in &directories {
            for ancestor in directory.ancestors() {
                if !checked_ancestors.insert(ancestor.to_path_buf()) {
                    continue;
                }
                config::reject_linked(ancestor)?;
            }
        }
        for directory in &directories {
            match directory_state(directory)? {
                Directory_state::Existing_directory => {}
                Directory_state::Blocked_by_file => {
                    if directory == &self.workspace {
                        return Err(failure("workspace_create_failed"));
                    }
                    // A home/cache path may have been written as a file by an
                    // older runtime. Preserve it and let the child report any
                    // feature-specific inability to use that optional path.
                }
                Directory_state::Missing => {
                    fs::create_dir_all(directory).map_err(|_| {
                        failure(if directory == &self.workspace {
                            "workspace_create_failed"
                        } else {
                            "runtime_home_create_failed"
                        })
                    })?;
                }
            }
        }
        Ok(())
    }

    pub(super) fn node_command(&self, node_path: &str) -> Result<Command, AppError> {
        let mut command = Command::new(node_path);
        self.configure_command(&mut command, |key| std::env::var_os(key))?;
        Ok(command)
    }

    pub(super) fn prepared_command(&self, node_path: &str) -> Result<Command, AppError> {
        let command = self.node_command(node_path)?;
        self.prepare()?;
        Ok(command)
    }

    fn configure_command(
        &self,
        command: &mut Command,
        read_environment: impl Fn(&str) -> Option<OsString>,
    ) -> Result<(), AppError> {
        command.env_clear();
        // Retain OS functionality without inheriting provider credentials,
        // NODE_OPTIONS, npm hooks, proxies or the user's home configuration.
        for key in INHERITED_OS_VARIABLES {
            if let Some(value) = read_environment(key) {
                command.env(key, value);
            }
        }
        #[cfg(windows)]
        command.env("SystemDrive", system_drive(&read_environment)?);
        for (key, relative) in HOME_VARIABLES {
            let path = if relative.is_empty() {
                self.home.clone()
            } else {
                self.home.join(relative)
            };
            command.env(key, path);
        }
        command
            .env("DSH_HOME", &self.root)
            .env("CI", "true")
            .env("NO_COLOR", "1");
        Ok(())
    }
}

fn directory_state(path: &Path) -> Result<Directory_state, AppError> {
    for (index, ancestor) in path.ancestors().enumerate() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) => {
                if metadata.is_dir() {
                    return Ok(if index == 0 {
                        Directory_state::Existing_directory
                    } else {
                        Directory_state::Missing
                    });
                }
                return Ok(Directory_state::Blocked_by_file);
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) => {}
            Err(_) => return Err(failure("managed_path_unreadable")),
        }
    }
    Ok(Directory_state::Missing)
}

#[cfg(windows)]
fn system_drive(
    read_environment: &impl Fn(&str) -> Option<OsString>,
) -> Result<OsString, AppError> {
    if let Some(drive) = read_environment("SystemDrive") {
        if drive.to_str().is_some_and(|value| {
            let bytes = value.as_bytes();
            bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
        }) {
            return Ok(drive);
        }
        return Err(failure("invalid_system_drive"));
    }
    if let Some(system_root) = read_environment("SystemRoot") {
        let path = Path::new(&system_root);
        if path.is_absolute() {
            if let Some(std::path::Component::Prefix(prefix)) = path.components().next() {
                match prefix.kind() {
                    std::path::Prefix::Disk(drive) | std::path::Prefix::VerbatimDisk(drive) => {
                        return Ok(format!("{}:", char::from(drive)).into());
                    }
                    _ => {}
                }
            }
        }
    }
    Err(failure("system_drive_unavailable"))
}

#[cfg(test)]
#[path = "runtime_environment_tests.rs"]
mod tests;
