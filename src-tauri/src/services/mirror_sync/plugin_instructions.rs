//! Instruction-only projection of configured native plugins. Plugin servers,
//! account bindings and runtime caches are neither copied nor activated.

use super::{codex_preferences::Inputs, scan};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::AtomicBool;

pub(super) const PROJECTION_DIRECTORY: &str = "skills/aag_plugin_instructions";
const MANIFEST_LIMIT: u64 = 128 * 1024;

struct Skill {
    name: String,
    root: PathBuf,
}

fn component(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
}

fn identity(id: &str) -> Result<(&str, &str), String> {
    let (package, marketplace) = id
        .rsplit_once('@')
        .ok_or("mirror_sync_plugin_metadata_invalid")?;
    if !component(package) || !component(marketplace) {
        return Err("mirror_sync_plugin_metadata_invalid".into());
    }
    Ok((package, marketplace))
}

fn skill_name(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes)
        .ok()?
        .trim_start_matches('\u{feff}');
    let mut lines = text.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    let mut found = None;
    for line in lines.take(128) {
        if line.trim() == "---" {
            return found;
        }
        if let Some(name) = line.strip_prefix("name:") {
            let name = name.trim();
            let name = name
                .strip_prefix('"')
                .and_then(|name| name.strip_suffix('"'))
                .or_else(|| {
                    name.strip_prefix('\'')
                        .and_then(|name| name.strip_suffix('\''))
                })
                .unwrap_or(name);
            if found.is_some() || !component(name) {
                return None;
            }
            found = Some(name.to_string());
        }
    }
    None
}

fn add_names(inventory: &scan::Inventory, names: &mut BTreeSet<String>) {
    for (path, contents) in &inventory.files {
        if path.file_name().is_some_and(|name| name == "SKILL.md") {
            if let Some(name) = skill_name(contents) {
                names.insert(name.to_lowercase());
            }
        }
    }
}

fn skill(root: &Path, budget: &mut scan::Budget) -> Result<Option<Skill>, String> {
    let Some(bytes) =
        scan::read_metadata_file(&root.join("SKILL.md"), budget, scan::MAX_FILE_BYTES)?
    else {
        return Ok(None);
    };
    let name = skill_name(&bytes).ok_or("mirror_sync_plugin_skill_invalid")?;
    Ok(Some(Skill {
        name,
        root: root.to_path_buf(),
    }))
}

fn declared_roots(manifest: &Value, version: &Path) -> Result<Vec<PathBuf>, String> {
    let paths: Vec<&str> = match manifest.get("skills") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::String(path)) => vec![path],
        Some(Value::Array(paths)) => paths
            .iter()
            .map(|path| path.as_str().ok_or("mirror_sync_plugin_metadata_invalid"))
            .collect::<Result<_, _>>()?,
        _ => return Err("mirror_sync_plugin_metadata_invalid".into()),
    };
    paths
        .into_iter()
        .map(|raw| {
            let path = Path::new(raw);
            let mut relative = PathBuf::new();
            for part in path.components() {
                match part {
                    Component::CurDir => {}
                    Component::Normal(name) => relative.push(name),
                    _ => return Err("mirror_sync_plugin_metadata_invalid".into()),
                }
            }
            // Declaring the entire plugin would also project server/account data.
            if relative.as_os_str().is_empty() {
                return Err("mirror_sync_plugin_metadata_invalid".into());
            }
            let root = version.join(relative);
            scan::plain_ancestors(&root)?;
            if scan::metadata(&root)?.is_none_or(|meta| !meta.is_dir() || scan::linked(&meta)) {
                return Err("mirror_sync_source_unavailable".into());
            }
            Ok(root)
        })
        .collect()
}

fn versions(
    native: &Path,
    id: &str,
    optional: bool,
    budget: &mut scan::Budget,
    stopped: &AtomicBool,
) -> Result<Vec<Vec<Skill>>, String> {
    let (package, marketplace) = identity(id)?;
    let cache = native.join("plugins/cache").join(marketplace).join(package);
    if optional && scan::metadata(&cache)?.is_none() {
        return Ok(Vec::new());
    }
    let mut versions = Vec::new();
    for version in scan::plugin_version_directories(&cache, budget, stopped)? {
        let bytes = scan::read_metadata_file(
            &version.join(".codex-plugin/plugin.json"),
            budget,
            MANIFEST_LIMIT,
        )?
        .ok_or("mirror_sync_source_unavailable")?;
        let manifest: Value =
            serde_json::from_slice(&bytes).map_err(|_| "mirror_sync_plugin_metadata_invalid")?;
        if manifest.get("name").and_then(Value::as_str) != Some(package) {
            return Err("mirror_sync_plugin_metadata_invalid".into());
        }
        let mut skills = Vec::new();
        for root in declared_roots(&manifest, &version)? {
            if let Some(direct) = skill(&root, budget)? {
                skills.push(direct);
            } else {
                for directory in scan::sorted_directories(&root, budget, stopped)? {
                    if let Some(skill) = skill(&directory, budget)? {
                        skills.push(skill);
                    }
                }
            }
        }
        versions.push(skills);
    }
    if !optional && versions.is_empty() {
        return Err("mirror_sync_source_unavailable".into());
    }
    Ok(versions)
}

pub(super) fn extend_inventory(
    native: &Path,
    mirror: &Path,
    trusted: &[PathBuf],
    inputs: &Inputs,
    inventory: &mut scan::Inventory,
    budget: &mut scan::Budget,
    stopped: &AtomicBool,
) -> Result<(), String> {
    if !inputs.source_present {
        if scan::destination_files(super::Client::Codex, mirror)?
            .iter()
            .any(|path| scan::path_within(path, Path::new(PROJECTION_DIRECTORY)))
        {
            return Err("mirror_sync_source_unavailable".into());
        }
        return Ok(());
    }
    let mut names = BTreeSet::new();
    add_names(inventory, &mut names);
    let shared = native
        .parent()
        .ok_or("mirror_sync_directory_invalid")?
        .join(".agents/skills");
    if scan::metadata(&shared)?.is_some() {
        let mut shared_inventory = scan::Inventory::empty();
        scan::scan_tree(
            &shared,
            Path::new("skills"),
            0,
            trusted,
            &mut shared_inventory,
            budget,
            stopped,
        )?;
        add_names(&shared_inventory, &mut names);
    }
    // Existing configured mirror packages remain owned by the plugin loader.
    // Read their skill names only; never alter their cache or activation flags.
    for id in &inputs.mirror_plugins {
        let installed = versions(mirror, id, true, budget, stopped)?;
        if installed.len() > 1 && installed.iter().any(|version| !version.is_empty()) {
            return Err("mirror_sync_plugin_version_ambiguous".into());
        }
        for version in installed {
            names.extend(version.into_iter().map(|skill| skill.name.to_lowercase()));
        }
    }
    let mut projected = BTreeSet::new();
    for id in &inputs.source_plugins {
        scan::check_cancelled(stopped)?;
        let versions = versions(native, id, false, budget, stopped)?;
        if versions
            .iter()
            .flatten()
            .all(|skill| names.contains(&skill.name.to_lowercase()))
        {
            continue;
        }
        // Configuration supplies no selected-version pointer. A sole validated
        // installed tree is unambiguous; multiple candidates are not ranked by
        // directory name, version string or mtime.
        if versions.len() != 1 {
            return Err("mirror_sync_plugin_version_ambiguous".into());
        }
        let (package, marketplace) = identity(id)?;
        for skill in versions.into_iter().next().unwrap() {
            let name = skill.name.to_lowercase();
            if names.contains(&name) {
                continue;
            }
            if !projected.insert(name) {
                return Err("mirror_sync_plugin_skill_collision".into());
            }
            let relative = Path::new(PROJECTION_DIRECTORY)
                .join(marketplace)
                .join(package)
                .join(&skill.name);
            if inventory
                .files
                .keys()
                .any(|path| scan::path_within(path, &relative))
            {
                return Err("mirror_sync_source_path_collision".into());
            }
            scan::scan_plugin_tree(&skill.root, &relative, trusted, inventory, budget, stopped)?;
        }
    }
    let mut unique = BTreeMap::new();
    for path in inventory.files.keys() {
        if unique.insert(scan::path_key(path), path).is_some() {
            return Err("mirror_sync_source_path_collision".into());
        }
    }
    Ok(())
}
