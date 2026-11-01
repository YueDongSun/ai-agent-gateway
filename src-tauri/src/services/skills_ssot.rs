//! Skills single-source-of-truth layer under `~/.aag/skills/`.
//!
//! Trusted shared roots (`~/.cc-switch/skills`, `~/.agents/skills`) are
//! imported into one AAG-owned copy with a content-hash manifest; the DAO
//! catalog (`skills` table) records each skill's hash and per-app enable
//! flags, and mirror instruction sync projects from the SSOT root instead of
//! the shared roots directly. Windows needs no symlinks: projection is a
//! bounded copy plus hash verification, and the manifest decides when a
//! re-copy is necessary. A skill that disappears from its source is pruned
//! from the SSOT and the catalog together.

use crate::config;
use crate::database::dao::skills::InstalledSkill;
use crate::database::Database;
use crate::error::AppError;
use sha2::Digest;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

const MANIFEST_FILENAME: &str = ".aag_skills_manifest.json";
const MANIFEST_SCHEMA_VERSION: u8 = 1;
const SOURCE_KIND: &str = "shared_root";
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
const MAX_ENTRIES: usize = 8192;
const MAX_DEPTH: usize = 16;

/// The AAG-owned skills root every projection reads from.
pub(crate) fn ssot_root() -> PathBuf {
    config::get_app_config_dir().join("skills")
}

/// Trusted shared roots imported into the SSOT, in deterministic order.
fn shared_roots() -> Vec<(String, PathBuf)> {
    let home = config::get_home_dir();
    vec![
        (".cc-switch".into(), home.join(".cc-switch").join("skills")),
        (".agents".into(), home.join(".agents").join("skills")),
    ]
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct ManifestEntry {
    content_hash: String,
    source_root: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Manifest {
    schema_version: u8,
    skills: BTreeMap<String, ManifestEntry>,
}

impl Manifest {
    fn empty() -> Self {
        Self {
            schema_version: MANIFEST_SCHEMA_VERSION,
            skills: BTreeMap::new(),
        }
    }
}

fn load_manifest(root: &Path) -> Manifest {
    fs::read_to_string(root.join(MANIFEST_FILENAME))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(Manifest::empty)
}

fn store_manifest(root: &Path, manifest: &Manifest) -> Result<(), AppError> {
    let raw = serde_json::to_string(manifest).map_err(AppError::Json)?;
    config::private_fs::atomic_write(&root.join(MANIFEST_FILENAME), raw.as_bytes())
        .map_err(String::from)
        .map_err(AppError::Config)
}

/// Deterministic content hash of one skill directory: sorted relative paths
/// plus file bytes; directory rows contribute nothing on their own.
fn directory_hash(path: &Path, depth: usize) -> Result<String, String> {
    if depth > MAX_DEPTH {
        return Err("skills_ssot_depth_limit".into());
    }
    let mut hasher = sha2::Sha256::new();
    let mut entries: Vec<_> = fs::read_dir(path)
        .map_err(|_| "skills_ssot_scan_failed".to_string())?
        .filter_map(std::result::Result::ok)
        .collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name();
        if name == MANIFEST_FILENAME {
            continue;
        }
        let child = entry.path();
        let meta = fs::symlink_metadata(&child).map_err(|_| "skills_ssot_scan_failed".to_string())?;
        hasher.update(name.as_encoded_bytes());
        hasher.update([0u8]);
        if meta.is_dir() {
            let child_hash = directory_hash(&child, depth + 1)?;
            hasher.update(child_hash.as_bytes());
        } else if meta.is_file() {
            if meta.len() > MAX_FILE_BYTES {
                return Err("skills_ssot_size_limit".into());
            }
            let bytes = fs::read(&child).map_err(|_| "skills_ssot_read_failed".to_string())?;
            hasher.update(&(bytes.len() as u64).to_le_bytes());
            hasher.update(&bytes);
        }
    }
    Ok(hex::encode(hasher.finalize()))
}

fn copy_tree(source: &Path, destination: &Path, depth: usize, seen: &mut usize) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err("skills_ssot_depth_limit".into());
    }
    config::private_fs::prepare_private_dir(destination).map_err(String::from)?;
    for entry in fs::read_dir(source).map_err(|_| "skills_ssot_scan_failed".to_string())? {
        *seen += 1;
        if *seen > MAX_ENTRIES {
            return Err("skills_ssot_entry_limit".into());
        }
        let entry = entry.map_err(|_| "skills_ssot_scan_failed".to_string())?;
        let child = entry.path();
        let target = destination.join(entry.file_name());
        let meta =
            fs::symlink_metadata(&child).map_err(|_| "skills_ssot_scan_failed".to_string())?;
        if meta.is_dir() {
            copy_tree(&child, &target, depth + 1, seen)?;
        } else if meta.is_file() {
            if meta.len() > MAX_FILE_BYTES {
                return Err("skills_ssot_size_limit".into());
            }
            if fs::symlink_metadata(&target).is_ok() {
                fs::remove_file(&target).map_err(|_| "skills_ssot_write_failed".to_string())?;
            }
            fs::copy(&child, &target).map_err(|_| "skills_ssot_write_failed".to_string())?;
        }
    }
    Ok(())
}

/// One source skill discovered under a shared root.
struct SourceSkill {
    name: String,
    path: PathBuf,
    source_root: String,
}

fn discover_sources() -> (Vec<SourceSkill>, Vec<String>) {
    let mut sources = Vec::new();
    let mut missing_roots = Vec::new();
    for (label, root) in shared_roots() {
        let Ok(entries) = fs::read_dir(&root) else {
            missing_roots.push(label);
            continue;
        };
        for entry in entries.filter_map(std::result::Result::ok) {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let path = entry.path();
            if fs::symlink_metadata(&path)
                .map(|meta| meta.is_dir() && !meta.file_type().is_symlink())
                .unwrap_or(false)
            {
                sources.push(SourceSkill {
                    name,
                    path,
                    source_root: label.clone(),
                });
            }
        }
    }
    (sources, missing_roots)
}

/// Import every trusted shared-root skill into the SSOT, refresh the content
/// hashes and catalog rows, prune skills whose source disappeared, and return
/// the SSOT root for projection. Errors never delete existing SSOT content:
/// a failed import keeps the previous copy and manifest entry.
pub(crate) fn reconcile(db: &Database) -> Result<PathBuf, AppError> {
    let root = ssot_root();
    config::private_fs::prepare_managed_private_dir(&root)
        .map_err(String::from)
        .map_err(AppError::Config)?;
    let mut manifest = load_manifest(&root);
    let (sources, missing_roots) = discover_sources();

    let mut imported: BTreeMap<String, ManifestEntry> = BTreeMap::new();
    let mut budget_bytes: u64 = 0;
    for source in &sources {
        // First root wins a name collision; later duplicates stay in their
        // shared root untouched.
        if imported.contains_key(&source.name) {
            continue;
        }
        let hash = match directory_hash(&source.path, 0) {
            Ok(hash) => hash,
            Err(code) => {
                log::warn!("skills ssot: hash failed skill={}", source.name);
                // Keep any previous copy catalogued when it still exists.
                if let Some(entry) = manifest.skills.get(&source.name).cloned() {
                    imported.insert(source.name.clone(), entry);
                }
                continue;
            }
        };
        budget_bytes = budget_bytes.saturating_add(
            fs_extra_dir_size(&source.path).unwrap_or(0),
        );
        if budget_bytes > MAX_TOTAL_BYTES {
            log::warn!("skills ssot: total size limit reached");
            break;
        }
        let unchanged = manifest
            .skills
            .get(&source.name)
            .is_some_and(|entry| entry.content_hash == hash);
        if !unchanged {
            let target = root.join(&source.name);
            let mut seen = 0usize;
            match replace_tree(&source.path, &target, &mut seen) {
                Ok(()) => {
                    upsert_catalog_row(db, &source.name, &target, &hash, &source.source_root)
                        .map_err(AppError::from)?;
                }
                Err(code) => {
                    log::warn!("skills ssot: copy failed skill={} code={}", source.name, code);
                    if let Some(entry) = manifest.skills.get(&source.name).cloned() {
                        imported.insert(source.name.clone(), entry);
                    }
                    continue;
                }
            }
        }
        imported.insert(
            source.name.clone(),
            ManifestEntry {
                content_hash: hash,
                source_root: source.source_root.clone(),
            },
        );
    }

    // Prune catalog + copies whose source skill disappeared. A missing whole
    // shared root prunes its imports too: the owner removed the root.
    let mut removed: Vec<String> = Vec::new();
    for (name, entry) in &manifest.skills {
        let source_exists = sources
            .iter()
            .any(|source| &source.name == name && source.source_root == entry.source_root);
        let root_removed =
            missing_roots.iter().any(|label| *label == entry.source_root);
        if !source_exists || root_removed {
            let target = root.join(name);
            let _ = fs::remove_dir_all(&target);
            let _ = db.delete_skill(name);
            removed.push(name.clone());
        }
    }
    if !removed.is_empty() {
        log::info!("skills ssot: pruned count={}", removed.len());
    }
    manifest.skills = imported;
    store_manifest(&root, &manifest)?;
    Ok(root)
}

fn replace_tree(source: &Path, target: &Path, seen: &mut usize) -> Result<(), String> {
    if target.exists() {
        fs::remove_dir_all(target).map_err(|_| "skills_ssot_write_failed".to_string())?;
    }
    copy_tree(source, target, 0, seen)
}

fn upsert_catalog_row(
    db: &Database,
    name: &str,
    directory: &Path,
    hash: &str,
    source_root: &str,
) -> Result<(), AppError> {
    let now = chrono::Utc::now().timestamp();
    let existing = db.get_skill(name).unwrap_or(None);
    let row = InstalledSkill {
        id: name.to_string(),
        name: name.to_string(),
        description: None,
        directory: directory.to_string_lossy().into_owned(),
        repo_owner: Some(SOURCE_KIND.into()),
        repo_name: Some(source_root.to_string()),
        repo_branch: None,
        content_hash: Some(hash.to_string()),
        enabled_claude: existing.as_ref().is_none_or(|row| row.enabled_claude),
        enabled_codex: existing.as_ref().is_none_or(|row| row.enabled_codex),
        installed_at: existing.as_ref().map_or(now, |row| row.installed_at),
        updated_at: now,
    };
    db.save_skill(&row)
}

fn fs_extra_dir_size(path: &Path) -> Option<u64> {
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = fs::read_dir(&current).ok()?;
        for entry in entries.filter_map(std::result::Result::ok) {
            let meta = entry.metadata().ok()?;
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                total = total.saturating_add(meta.len());
            }
        }
    }
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(label: &str) -> crate::services::provider::test_support::IsolatedHome {
        crate::services::provider::test_support::isolated_home(label)
    }

    fn shared_root(home: &crate::services::provider::test_support::IsolatedHome, label: &str) -> PathBuf {
        let root = home.root.join(label).join("skills");
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn skill_file(root: &Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn imports_shared_roots_into_the_ssot_catalog_with_hashes() {
        let home = fixture("skills-ssot-import");
        let cc = shared_root(&home, ".cc-switch");
        skill_file(&cc, "alpha/SKILL.md", "# alpha");
        skill_file(&cc, "alpha/helper.md", "helper");
        let agents = shared_root(&home, ".agents");
        skill_file(&agents, "beta/SKILL.md", "# beta");

        let db = Database::memory().unwrap();
        let root = reconcile(&db).unwrap();
        assert!(root.join("alpha/SKILL.md").is_file());
        assert!(root.join("alpha/helper.md").is_file());
        assert!(root.join("beta/SKILL.md").is_file());
        let alpha = db.get_skill("alpha").unwrap().unwrap();
        assert_eq!(alpha.repo_owner.as_deref(), Some("shared_root"));
        assert!(alpha.content_hash.as_deref().is_some_and(|hash| !hash.is_empty()));
        assert!(alpha.enabled_claude && alpha.enabled_codex);
        assert!(root.join(MANIFEST_FILENAME).is_file());
    }

    #[test]
    fn changed_content_reimports_and_removed_source_prunes() {
        let home = fixture("skills-ssot-refresh");
        let cc = shared_root(&home, ".cc-switch");
        skill_file(&cc, "alpha/SKILL.md", "# v1");
        let db = Database::memory().unwrap();
        let root = reconcile(&db).unwrap();
        let first_hash = db.get_skill("alpha").unwrap().unwrap().content_hash.unwrap();
        assert!(first_hash.len() == 64);

        skill_file(&cc, "alpha/SKILL.md", "# v2 with more content");
        let root = reconcile(&db).unwrap();
        let second = db.get_skill("alpha").unwrap().unwrap();
        assert_ne!(second.content_hash.as_deref(), Some(first_hash.as_str()));
        assert_eq!(
            fs::read_to_string(root.join("alpha/SKILL.md")).unwrap(),
            "# v2 with more content"
        );

        fs::remove_dir_all(cc.join("alpha")).unwrap();
        reconcile(&db).unwrap();
        assert!(!root.join("alpha").exists());
        assert!(db.get_skill("alpha").unwrap().is_none());
    }

    #[test]
    fn first_shared_root_wins_name_collisions() {
        let home = fixture("skills-ssot-collision");
        let cc = shared_root(&home, ".cc-switch");
        let agents = shared_root(&home, ".agents");
        skill_file(&cc, "twin/SKILL.md", "# from cc-switch");
        skill_file(&agents, "twin/SKILL.md", "# from agents");
        let db = Database::memory().unwrap();
        let root = reconcile(&db).unwrap();
        assert_eq!(
            fs::read_to_string(root.join("twin/SKILL.md")).unwrap(),
            "# from cc-switch"
        );
        let row = db.get_skill("twin").unwrap().unwrap();
        assert_eq!(row.repo_name.as_deref(), Some(".cc-switch"));
    }

    #[test]
    fn removing_a_whole_shared_root_prunes_its_imports() {
        let home = fixture("skills-ssot-root-removal");
        let agents = shared_root(&home, ".agents");
        skill_file(&agents, "gamma/SKILL.md", "# gamma");
        let db = Database::memory().unwrap();
        let root = reconcile(&db).unwrap();
        assert!(root.join("gamma/SKILL.md").is_file());

        fs::remove_dir_all(agents).unwrap();
        reconcile(&db).unwrap();
        assert!(!root.join("gamma").exists());
        assert!(db.get_skill("gamma").unwrap().is_none());
    }
}
