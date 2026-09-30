//! Curated metadata, not a trust certification or an installation claim.
use super::{
    config::{failure, read_json, safe_path, MANAGED_PROFILE},
    types::Dsh_plugin,
};
use crate::error::AppError;
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
};

pub(super) fn entries() -> Vec<Dsh_plugin> {
    [
        ("base", "@deepseek-ai/dsh-base", "DSH base", "core", "Official core composition, including tools and filesystem Skills.", "https://github.com/deepseek-ai/deepseek-harness", true),
        ("web_app", "@deepseek-ai/dsh-web-app", "DSH WebUI", "desktop", "Official web application opened in a restricted desktop window.", "https://github.com/deepseek-ai/deepseek-harness", true),
        ("skins", "@dsh-external/dsh-web-skins", "Appearance skins", "personalization", "Community appearance module. The inspected version has a legacy client declaration and is not an enableable bundle.", "https://github.com/Moeblack/dsh-skins", false),
        ("cost_meter", "@steven-wu/dsh-cost-meter", "Cost meter", "practical", "Community estimated usage display. Estimates are not billing evidence.", "https://github.com/Sttrevens/dsh-cost-meter", false),
        ("mcp_client", "@deepseek-ai/dsh-mcp-client", "MCP client", "practical", "Official MCP tool adapter. Installing a plain module does not configure a server or load a bundle.", "https://github.com/deepseek-ai/deepseek-harness", false),
    ].into_iter().map(|(plugin_id, package_name, display_name, category, description, source_url, essential)| Dsh_plugin {
        plugin_id: plugin_id.into(), package_name: package_name.into(), display_name: display_name.into(), category: category.into(), description: description.into(), source_url: source_url.into(),
        installed_version: None, enabled: false, essential, runtime_phase: "unknown".into(),
    }).collect()
}

pub(super) fn validate_package(package_name: &str) -> Result<Dsh_plugin, AppError> {
    entries()
        .into_iter()
        .find(|entry| entry.package_name == package_name)
        .ok_or_else(|| failure("package_not_allowlisted"))
}

pub(super) fn exact_version(version: &str) -> bool {
    if version.is_empty()
        || version.len() > 100
        || !version
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b".-+".contains(&c))
    {
        return false;
    }
    let core = version.split(['-', '+']).next().unwrap_or_default();
    let parts: Vec<_> = core.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.bytes().all(|c| c.is_ascii_digit())
                && (part.len() == 1 || !part.starts_with('0'))
        })
}

pub(super) fn installed_manifest(
    profile: &Path,
    package_name: &str,
) -> Result<Option<Value>, AppError> {
    // pnpm symlinks are expected here, but every resolved package must remain
    // inside this profile's node_modules. No arbitrary external link is adopted.
    let modules = profile.join("node_modules");
    // Links below node_modules are pnpm-owned; the managed root itself is not.
    safe_path(&modules)?;
    let target = modules.join(package_name).join("package.json");
    if !target.exists() {
        return Ok(None);
    }
    let canonical_modules =
        fs::canonicalize(&modules).map_err(|_| failure("package_unreadable"))?;
    let canonical_package =
        fs::canonicalize(modules.join(package_name)).map_err(|_| failure("package_unreadable"))?;
    let canonical_target = fs::canonicalize(&target).map_err(|_| failure("package_unreadable"))?;
    if !canonical_package.starts_with(&canonical_modules)
        || !canonical_target.starts_with(&canonical_package)
    {
        return Err(failure("package_link_escape"));
    }
    let manifest = read_json(&canonical_target)?;
    if manifest
        .as_ref()
        .is_some_and(|manifest| manifest.get("name").and_then(Value::as_str) != Some(package_name))
    {
        return Err(failure("installed_identity_mismatch"));
    }
    Ok(manifest)
}

/// The same containment contract applies to installation and later enablement.
/// A plain module has no bundle patch and remains installable but not enableable.
pub(super) fn bundle_patch(
    profile: &Path,
    package_name: &str,
    manifest: &Value,
) -> Result<Option<PathBuf>, AppError> {
    let Some(patch) = manifest.pointer("/dsh/bundle/patch") else {
        return Ok(None);
    };
    let patch = patch
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| failure("invalid_bundle_patch"))?;
    let package = profile.join("node_modules").join(package_name);
    let canonical_package = fs::canonicalize(&package).map_err(|_| failure("bundle_unreadable"))?;
    let canonical_patch =
        fs::canonicalize(package.join(patch)).map_err(|_| failure("bundle_patch_missing"))?;
    if !canonical_patch.starts_with(&canonical_package) {
        return Err(failure("bundle_patch_escape"));
    }
    if !canonical_patch.is_file() {
        return Err(failure("bundle_patch_missing"));
    }
    Ok(Some(canonical_patch))
}

fn bundled_version(entry_path: &str, package_name: &str) -> Result<Option<String>, AppError> {
    if entry_path.is_empty() {
        return Ok(None);
    }
    for directory in Path::new(entry_path).ancestors().skip(1).take(8) {
        let candidate = directory
            .join("node_modules")
            .join(package_name)
            .join("package.json");
        if !candidate.is_file() {
            continue;
        }
        let resolved =
            fs::canonicalize(candidate).map_err(|_| failure("runtime_bundle_unreadable"))?;
        if let Some(manifest) = read_json(&resolved)? {
            if manifest.get("name").and_then(Value::as_str) == Some(package_name) {
                return Ok(manifest
                    .get("version")
                    .and_then(Value::as_str)
                    .map(str::to_string));
            }
        }
    }
    Ok(None)
}

pub(super) fn inventory(root: &Path) -> Result<Vec<Dsh_plugin>, AppError> {
    let profile = root.join(MANAGED_PROFILE);
    let manifest = read_json(&profile.join("package.json"))?;
    let bundles = manifest
        .as_ref()
        .and_then(|value| value.pointer("/dsh/profile/bundles"))
        .and_then(Value::as_array);
    let settings = super::config::read_settings(root)?.0;
    let mut plugins = entries();
    for plugin in &mut plugins {
        if let Some(installed) = installed_manifest(&profile, &plugin.package_name)? {
            plugin.installed_version = installed
                .get("version")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        if plugin.essential && plugin.installed_version.is_none() {
            plugin.installed_version =
                bundled_version(&settings.dsh_entry_path, &plugin.package_name)?;
        }
        plugin.enabled = bundles.is_some_and(|values| {
            values
                .iter()
                .any(|value| value.as_str() == Some(plugin.package_name.as_str()))
        });
    }
    Ok(plugins)
}
