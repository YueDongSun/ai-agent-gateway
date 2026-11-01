use super::Client;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 128 * 1024 * 1024;
pub(super) const MAX_ENTRIES: usize = 4096;
const MAX_DEPTH: usize = 16;

pub(super) struct Inventory {
    pub files: BTreeMap<PathBuf, Vec<u8>>,
    pub executable: BTreeSet<PathBuf>,
    pub budget: Budget,
}

impl Inventory {
    pub fn empty() -> Self {
        Self {
            files: BTreeMap::new(),
            executable: BTreeSet::new(),
            budget: Budget::default(),
        }
    }
}

#[derive(Default)]
pub(super) struct Budget {
    entries: usize,
    bytes: u64,
}

impl Budget {
    pub fn entry(&mut self) -> Result<(), String> {
        self.entries += 1;
        if self.entries > MAX_ENTRIES {
            Err("mirror_sync_entry_limit".into())
        } else {
            Ok(())
        }
    }
}

pub(super) fn name_eq(left: &str, right: &str) -> bool {
    #[cfg(windows)]
    {
        left.eq_ignore_ascii_case(right)
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

pub(super) fn path_key(path: &Path) -> String {
    #[cfg(windows)]
    {
        let native: PathBuf = path.components().collect();
        native.to_string_lossy().to_lowercase()
    }
    #[cfg(not(windows))]
    {
        path.to_string_lossy().into_owned()
    }
}

pub(super) fn plain_absolute_path(path: &Path) -> bool {
    path.is_absolute()
        && !path.components().any(|part| {
            matches!(
                part,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
        && !path.to_string_lossy().chars().any(char::is_control)
}

pub(super) fn path_within(path: &Path, root: &Path) -> bool {
    let parts: Vec<_> = path.components().collect();
    let base: Vec<_> = root.components().collect();
    parts.len() >= base.len()
        && parts.iter().zip(base.iter()).all(|(left, right)| {
            name_eq(
                &left.as_os_str().to_string_lossy(),
                &right.as_os_str().to_string_lossy(),
            )
        })
}

pub(super) fn linked(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    metadata.file_type().is_symlink()
}

pub(super) fn metadata(path: &Path) -> Result<Option<fs::Metadata>, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err("mirror_sync_source_unavailable".into()),
    }
}

pub(super) fn check_cancelled(stopped: &AtomicBool) -> Result<(), String> {
    if stopped.load(Ordering::SeqCst) {
        Err("mirror_sync_stopped".into())
    } else {
        Ok(())
    }
}

pub(super) fn plain_ancestors(path: &Path) -> Result<(), String> {
    for ancestor in path.ancestors() {
        if metadata(ancestor)?.is_some_and(|metadata| linked(&metadata)) {
            return Err("mirror_sync_source_link_forbidden".into());
        }
    }
    Ok(())
}

pub(super) fn read_metadata_file(
    path: &Path,
    budget: &mut Budget,
    limit: u64,
) -> Result<Option<Vec<u8>>, String> {
    let Some(meta) = metadata(path)? else {
        return Ok(None);
    };
    if meta.len() > limit {
        return Err("mirror_sync_size_limit".into());
    }
    budget.entry()?;
    let relative = Path::new("metadata");
    let mut temporary = Inventory {
        files: BTreeMap::new(),
        executable: BTreeSet::new(),
        budget: Budget::default(),
    };
    read_file(path, relative, &mut temporary, budget)?;
    let bytes = temporary
        .files
        .remove(relative)
        .ok_or("mirror_sync_source_unavailable")?;
    if bytes.len() as u64 > limit {
        return Err("mirror_sync_size_limit".into());
    }
    Ok(Some(bytes))
}

pub(super) fn sorted_directories(
    path: &Path,
    budget: &mut Budget,
    stopped: &AtomicBool,
) -> Result<Vec<PathBuf>, String> {
    plain_ancestors(path)?;
    if metadata(path)?.is_none_or(|meta| linked(&meta) || !meta.is_dir()) {
        return Err("mirror_sync_source_unavailable".into());
    }
    let mut directories = Vec::new();
    for entry in fs::read_dir(path).map_err(|_| "mirror_sync_source_unavailable")? {
        check_cancelled(stopped)?;
        budget.entry()?;
        let entry = entry.map_err(|_| "mirror_sync_source_unavailable")?;
        let meta = metadata(&entry.path())?.ok_or("mirror_sync_source_changed")?;
        if linked(&meta) {
            return Err("mirror_sync_source_link_forbidden".into());
        }
        if meta.is_dir() {
            directories.push(entry.path());
        }
    }
    directories.sort();
    Ok(directories)
}

/// Plugin installers may keep `latest` as a junction/symlink beside the real
/// version directory. Admit that one alias only as a duplicate reference to
/// an already enumerated ordinary direct child; all reads use concrete paths.
pub(super) fn plugin_version_directories(
    cache: &Path,
    budget: &mut Budget,
    stopped: &AtomicBool,
) -> Result<Vec<PathBuf>, String> {
    plain_ancestors(cache)?;
    if metadata(cache)?.is_none_or(|meta| linked(&meta) || !meta.is_dir()) {
        return Err("mirror_sync_source_unavailable".into());
    }
    let canonical_cache = fs::canonicalize(cache).map_err(|_| "mirror_sync_source_unavailable")?;
    let mut ordinary = BTreeMap::new();
    let mut aliases = Vec::new();
    for entry in fs::read_dir(cache).map_err(|_| "mirror_sync_source_unavailable")? {
        check_cancelled(stopped)?;
        budget.entry()?;
        let entry = entry.map_err(|_| "mirror_sync_source_unavailable")?;
        let path = entry.path();
        let meta = metadata(&path)?.ok_or("mirror_sync_source_changed")?;
        if linked(&meta) {
            if !name_eq(&entry.file_name().to_string_lossy(), "latest") {
                return Err("mirror_sync_source_link_forbidden".into());
            }
            aliases.push(path);
        } else if meta.is_dir() {
            plain_ancestors(&path)?;
            let concrete = fs::canonicalize(path).map_err(|_| "mirror_sync_source_unavailable")?;
            if concrete
                .parent()
                .is_none_or(|parent| path_key(parent) != path_key(&canonical_cache))
            {
                return Err("mirror_sync_source_link_forbidden".into());
            }
            ordinary.insert(path_key(&concrete), concrete);
        }
    }
    for alias in aliases {
        check_cancelled(stopped)?;
        let raw = fs::read_link(&alias).map_err(|_| "mirror_sync_source_link_forbidden")?;
        let target = if raw.is_absolute() {
            raw
        } else {
            cache.join(raw)
        };
        // Check the immediate destination before canonicalization. A nested
        // alias that eventually lands in this package is still not admitted.
        if !plain_absolute_path(&target) {
            return Err("mirror_sync_source_link_forbidden".into());
        }
        plain_ancestors(&target)?;
        if metadata(&target)?.is_none_or(|meta| linked(&meta) || !meta.is_dir()) {
            return Err("mirror_sync_source_link_forbidden".into());
        }
        let parent = target.parent().ok_or("mirror_sync_source_link_forbidden")?;
        let parent = fs::canonicalize(parent).map_err(|_| "mirror_sync_source_link_forbidden")?;
        if path_key(&parent) != path_key(&canonical_cache) {
            return Err("mirror_sync_source_link_forbidden".into());
        }
        let concrete = fs::canonicalize(target).map_err(|_| "mirror_sync_source_link_forbidden")?;
        if !ordinary.contains_key(&path_key(&concrete)) {
            return Err("mirror_sync_source_link_forbidden".into());
        }
    }
    Ok(ordinary.into_values().collect())
}

fn read_file(
    source: &Path,
    relative: &Path,
    inventory: &mut Inventory,
    budget: &mut Budget,
) -> Result<(), String> {
    plain_ancestors(source)?;
    let before = metadata(source)?.ok_or_else(|| "mirror_sync_source_changed".to_string())?;
    if !before.is_file() || linked(&before) {
        return Err("mirror_sync_source_type_invalid".into());
    }
    if before.len() > MAX_FILE_BYTES {
        return Err("mirror_sync_size_limit".into());
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Read the opened object itself; a raced final-component reparse point
        // must not redirect an instruction read outside the verified source.
        options.custom_flags(0x0020_0000);
    }
    let file = options
        .open(source)
        .map_err(|_| "mirror_sync_source_unavailable")?;
    let opened = file
        .metadata()
        .map_err(|_| "mirror_sync_source_unavailable")?;
    if linked(&opened) || !opened.is_file() {
        return Err("mirror_sync_source_link_forbidden".into());
    }
    let mut contents = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut contents)
        .map_err(|_| "mirror_sync_source_unavailable")?;
    plain_ancestors(source)?;
    let after = metadata(source)?.ok_or_else(|| "mirror_sync_source_changed".to_string())?;
    if linked(&after)
        || !after.is_file()
        || before.len() != contents.len() as u64
        || before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
    {
        return Err("mirror_sync_source_changed".into());
    }
    budget.bytes += contents.len() as u64;
    if contents.len() as u64 > MAX_FILE_BYTES || budget.bytes > MAX_TOTAL_BYTES {
        return Err("mirror_sync_size_limit".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if opened.permissions().mode() & 0o100 != 0 {
            inventory.executable.insert(relative.to_path_buf());
        }
    }
    inventory.files.insert(relative.to_path_buf(), contents);
    Ok(())
}

fn resolve_skill_link(source: &Path, trusted: &[PathBuf]) -> Result<PathBuf, String> {
    let target = fs::canonicalize(source).map_err(|_| "mirror_sync_source_unavailable")?;
    plain_ancestors(&target)?;
    let mut admitted = false;
    for root in trusted {
        let Some(meta) = metadata(root)? else {
            continue;
        };
        if linked(&meta) || !meta.is_dir() {
            continue;
        }
        plain_ancestors(root)?;
        let root = fs::canonicalize(root).map_err(|_| "mirror_sync_source_unavailable")?;
        if path_within(&target, &root) && target != root {
            admitted = true;
            break;
        }
    }
    if !admitted {
        return Err("mirror_sync_source_link_forbidden".into());
    }
    if metadata(&target)?.is_none_or(|meta| !meta.is_dir() || linked(&meta)) {
        return Err("mirror_sync_source_type_invalid".into());
    }
    Ok(target)
}

pub(super) fn scan_tree(
    source: &Path,
    relative: &Path,
    depth: usize,
    trusted: &[PathBuf],
    inventory: &mut Inventory,
    budget: &mut Budget,
    stopped: &AtomicBool,
) -> Result<(), String> {
    scan_tree_with_policy(
        source, relative, depth, trusted, inventory, budget, stopped, false,
    )
}

pub(super) fn scan_plugin_tree(
    source: &Path,
    relative: &Path,
    trusted: &[PathBuf],
    inventory: &mut Inventory,
    budget: &mut Budget,
    stopped: &AtomicBool,
) -> Result<(), String> {
    let depth = relative.components().count().saturating_sub(1);
    scan_tree_with_policy(
        source, relative, depth, trusted, inventory, budget, stopped, true,
    )
}

fn scan_tree_with_policy(
    source: &Path,
    relative: &Path,
    depth: usize,
    trusted: &[PathBuf],
    inventory: &mut Inventory,
    budget: &mut Budget,
    stopped: &AtomicBool,
    plugin_instructions: bool,
) -> Result<(), String> {
    check_cancelled(stopped)?;
    if depth > MAX_DEPTH {
        return Err("mirror_sync_depth_limit".into());
    }
    plain_ancestors(source)?;
    let mut entries = Vec::new();
    for entry in fs::read_dir(source).map_err(|_| "mirror_sync_source_unavailable")? {
        check_cancelled(stopped)?;
        let entry = entry.map_err(|_| "mirror_sync_source_unavailable")?;
        if plugin_instructions
            && [
                "auth.json",
                "auth.toml",
                "credentials.json",
                ".env",
                "sessions",
                ".cache",
                "node_modules",
                "__pycache__",
                ".git",
                ".venv",
                ".codex-plugin",
                ".mcp.json",
                ".app.json",
            ]
            .iter()
            .any(|name| name_eq(&entry.file_name().to_string_lossy(), name))
        {
            return Err("mirror_sync_plugin_content_forbidden".into());
        }
        if depth == 0
            && name_eq(&relative.to_string_lossy(), "skills")
            && name_eq(&entry.file_name().to_string_lossy(), ".system")
        {
            continue;
        }
        budget.entries += 1;
        if budget.entries > MAX_ENTRIES {
            return Err("mirror_sync_entry_limit".into());
        }
        entries.push(entry);
    }
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        check_cancelled(stopped)?;
        let path = entry.path();
        let relative = relative.join(entry.file_name());
        let meta = metadata(&path)?.ok_or_else(|| "mirror_sync_source_changed".to_string())?;
        if linked(&meta) {
            if depth != 0
                || relative
                    .parent()
                    .is_none_or(|parent| !name_eq(&parent.to_string_lossy(), "skills"))
            {
                return Err("mirror_sync_source_link_forbidden".into());
            }
            let target = resolve_skill_link(&path, trusted)?;
            scan_tree_with_policy(
                &target,
                &relative,
                depth + 1,
                trusted,
                inventory,
                budget,
                stopped,
                plugin_instructions,
            )?;
        } else if meta.is_dir() {
            scan_tree_with_policy(
                &path,
                &relative,
                depth + 1,
                trusted,
                inventory,
                budget,
                stopped,
                plugin_instructions,
            )?;
        } else if meta.is_file() {
            read_file(&path, &relative, inventory, budget)?;
        } else {
            return Err("mirror_sync_source_type_invalid".into());
        }
    }
    Ok(())
}

pub(super) fn source_inventory(
    client: Client,
    native: &Path,
    trusted: &[PathBuf],
    stopped: &AtomicBool,
) -> Result<Inventory, String> {
    check_cancelled(stopped)?;
    plain_ancestors(native)?;
    if metadata(native)?.is_none_or(|meta| !meta.is_dir() || linked(&meta)) {
        return Err("mirror_sync_source_unavailable".into());
    }
    fs::read_dir(native).map_err(|_| "mirror_sync_source_unavailable")?;
    let mut inventory = Inventory {
        files: BTreeMap::new(),
        executable: BTreeSet::new(),
        budget: Budget::default(),
    };
    let mut budget = Budget::default();
    for name in client.files() {
        let path = native.join(name);
        if metadata(&path)?.is_some() {
            budget.entries += 1;
            read_file(&path, Path::new(name), &mut inventory, &mut budget)?;
        }
    }
    for name in client.directories() {
        let path = native.join(name);
        let Some(meta) = metadata(&path)? else {
            continue;
        };
        if linked(&meta) || !meta.is_dir() {
            return Err("mirror_sync_source_type_invalid".into());
        }
        scan_tree(
            &path,
            Path::new(name),
            0,
            trusted,
            &mut inventory,
            &mut budget,
            stopped,
        )?;
    }
    // An unavailable root after scanning must never authorize deletion from a
    // formerly populated mirror based on a partial or vanishing source.
    if metadata(native)?.is_none_or(|meta| !meta.is_dir() || linked(&meta)) {
        return Err("mirror_sync_source_unavailable".into());
    }
    let mut unique = BTreeSet::new();
    if inventory
        .files
        .keys()
        .any(|path| !unique.insert(path_key(path)))
    {
        return Err("mirror_sync_source_path_collision".into());
    }
    inventory.budget = budget;
    Ok(inventory)
}

pub(super) fn destination_files(
    client: Client,
    mirror: &Path,
) -> Result<BTreeSet<PathBuf>, String> {
    let mut files = BTreeSet::new();
    let mut pending: Vec<(PathBuf, PathBuf, usize)> = client
        .directories()
        .iter()
        .map(|name| (mirror.join(name), PathBuf::from(name), 0))
        .collect();
    for name in client.files() {
        match fs::symlink_metadata(mirror.join(name)) {
            Ok(meta) if !linked(&meta) && meta.is_file() => {
                files.insert(PathBuf::from(name));
            }
            Ok(_) => return Err("mirror_sync_destination_type_invalid".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("mirror_sync_destination_unavailable".into()),
        }
    }
    let mut count = 0;
    while let Some((directory, relative, depth)) = pending.pop() {
        let meta = match fs::symlink_metadata(&directory) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err("mirror_sync_destination_unavailable".into()),
        };
        if linked(&meta) || !meta.is_dir() {
            return Err("mirror_sync_destination_type_invalid".into());
        }
        if depth > MAX_DEPTH {
            return Err("mirror_sync_depth_limit".into());
        }
        for entry in fs::read_dir(&directory).map_err(|_| "mirror_sync_destination_unavailable")? {
            let entry = entry.map_err(|_| "mirror_sync_destination_unavailable")?;
            if depth == 0
                && name_eq(&relative.to_string_lossy(), "skills")
                && name_eq(&entry.file_name().to_string_lossy(), ".system")
            {
                continue;
            }
            count += 1;
            if count > MAX_ENTRIES {
                return Err("mirror_sync_entry_limit".into());
            }
            let relative = relative.join(entry.file_name());
            let meta = fs::symlink_metadata(entry.path())
                .map_err(|_| "mirror_sync_destination_unavailable")?;
            if linked(&meta) {
                return Err("mirror_sync_destination_link_forbidden".into());
            }
            if meta.is_dir() {
                pending.push((entry.path(), relative, depth + 1));
            } else if meta.is_file() {
                files.insert(relative);
            } else {
                return Err("mirror_sync_destination_type_invalid".into());
            }
        }
    }
    Ok(files)
}
