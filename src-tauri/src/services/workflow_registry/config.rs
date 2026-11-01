//! Private, validated host configuration. No machine paths ship in the catalog.
#![allow(non_camel_case_types)]
use serde::Deserialize;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

const MAX_CONFIG_BYTES: u64 = 16 * 1024;

#[derive(Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workflow_config {
    pub python_path: Option<PathBuf>,
    pub router_path: Option<PathBuf>,
    pub project_dir: Option<PathBuf>,
    #[serde(default)]
    pub probe_paths: HashMap<String, PathBuf>,
    #[serde(default)]
    pub jev_enabled: bool,
    pub jev_model: Option<String>,
    pub jev_confidence_floor: Option<f64>,
    #[serde(default)]
    pub strands_advice_enabled: bool,
    pub strands_advice_path: Option<PathBuf>,
    pub strands_advice_config_path: Option<PathBuf>,
}

impl Workflow_config {
    pub fn load(config_root: &Path) -> Result<Self, String> {
        let path = config_root.join("workflow_config.json");
        crate::config::private_fs::validate_external_file_path(&path)
            .map_err(|_| "workflow:config_path_invalid".to_string())?;
        if !path.exists() {
            return Ok(Self::default());
        }
        crate::config::private_fs::verify_private_file(&path)
            .map_err(|_| "workflow:config_not_private".to_string())?;
        let metadata = fs::metadata(&path).map_err(|_| "workflow:config_unreadable".to_string())?;
        if metadata.len() > MAX_CONFIG_BYTES {
            return Err("workflow:config_too_large".into());
        }
        let bytes = fs::read(path).map_err(|_| "workflow:config_unreadable".to_string())?;
        let config: Self =
            serde_json::from_slice(&bytes).map_err(|_| "workflow:config_invalid".to_string())?;
        Ok(config)
    }

    pub fn validate_router(&self) -> Result<(), String> {
        for path in [&self.python_path, &self.router_path] {
            validate_executable(path.as_deref().ok_or("workflow:router_not_configured")?)?;
        }
        self.validate_project_dir(true)
    }

    pub fn validate_probe(&self, capability_id: &str) -> Result<(), String> {
        if !matches!(
            capability_id,
            "pi_cli" | "cline_cli" | "kimi_cli" | "minimax_cli"
        ) {
            return Err("workflow:unsupported_probe_id".into());
        }
        if let Some(path) = self.probe_paths.get(capability_id) {
            validate_executable(path)?;
        }
        Ok(())
    }

    fn validate_project_dir(&self, required: bool) -> Result<(), String> {
        match &self.project_dir {
            Some(path) if path.is_absolute() && path.is_dir() => Ok(()),
            None if !required => Ok(()),
            _ => Err("workflow:project_dir_invalid".into()),
        }
    }

    pub fn validate_jev(&self) -> Result<(), String> {
        if let Some(jev_model) = &self.jev_model {
            if !jev_model.starts_with("jev-")
                || jev_model.len() > 64
                || !jev_model
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_')
            {
                return Err("workflow:jev_model_invalid".into());
            }
        }
        if self
            .jev_confidence_floor
            .is_some_and(|floor| !floor.is_finite() || !(0.0..=1.0).contains(&floor))
        {
            return Err("workflow:jev_confidence_floor_invalid".into());
        }
        Ok(())
    }

    pub fn validate_advice(&self) -> Result<(), String> {
        for path in [&self.python_path, &self.strands_advice_path] {
            validate_executable(
                path.as_deref()
                    .ok_or("workflow:strands_advice_path_invalid")?,
            )?;
        }
        self.validate_project_dir(false)?;
        let path = self
            .strands_advice_config_path
            .as_deref()
            .ok_or("workflow:strands_advice_config_invalid")?;
        validate_executable(path).map_err(|_| "workflow:strands_advice_config_invalid")?;
        crate::config::private_fs::verify_private_file(path)
            .map_err(|_| "workflow:strands_advice_config_not_private".to_string())
    }
}

fn validate_executable(path: &Path) -> Result<(), String> {
    if !path.is_absolute() || !path.is_file() {
        return Err("workflow:configured_executable_invalid".into());
    }
    crate::config::private_fs::validate_external_file_path(path)
        .map_err(|_| "workflow:configured_executable_invalid".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optional_configuration_is_validated_only_by_its_consumer() {
        let directory = tempfile::tempdir().unwrap();
        crate::config::private_fs::prepare_managed_private_dir(directory.path()).unwrap();
        let executable = directory.path().join("fixture_executable");
        crate::config::private_fs::atomic_write(&executable, b"fixture").unwrap();
        let value = serde_json::json!({
            "python_path": executable, "router_path": executable,
            "project_dir": directory.path(),
            "probe_paths": {"pi_cli": executable, "cline_cli": "missing_probe"},
            "strands_advice_enabled": false, "strands_advice_path": "missing_advice",
            "jev_model": "invalid_model", "jev_confidence_floor": 2.0
        });
        crate::config::private_fs::atomic_write(
            &directory.path().join("workflow_config.json"),
            &serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        let config = Workflow_config::load(directory.path()).unwrap();
        assert!(config.validate_router().is_ok());
        assert!(config.validate_probe("pi_cli").is_ok());
        assert!(config.validate_probe("cline_cli").is_err());
        assert!(config.validate_probe("kimi_cli").is_ok());
        assert!(config.validate_advice().is_err());
        assert!(config.validate_jev().is_err());
        let probe_only = Workflow_config {
            probe_paths: config.probe_paths,
            ..Workflow_config::default()
        };
        assert!(probe_only.validate_probe("pi_cli").is_ok());
        assert!(probe_only.validate_router().is_err());
    }
}
