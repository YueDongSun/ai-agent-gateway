//! Read selected personalization inputs and merge two owned agent scalars.
//! Provider, model, authentication, security and plugin activation stay local.

use super::scan;
use crate::config::private_fs;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item, Table};

const CONFIG_LIMIT: u64 = 1024 * 1024;
const ENABLED: &str = "enabled";
const THREADS: &str = "max_concurrent_threads_per_session";

pub(super) struct Inputs {
    pub source_present: bool,
    pub source_plugins: BTreeSet<String>,
    pub mirror_plugins: BTreeSet<String>,
    pub update: Option<ConfigUpdate>,
}

pub(super) struct ConfigUpdate {
    path: PathBuf,
    previous: private_fs::MirrorFileSnapshot,
    contents: Vec<u8>,
}

impl ConfigUpdate {
    pub fn commit(self) -> Result<(), String> {
        super::require_unmanaged_client(super::Client::Codex)?;
        crate::services::provider::config_writes::write_mirror_files(&[
            crate::services::provider::config_writes::MirrorConfigFileUpdate {
                path: &self.path,
                contents: &self.contents,
                app_type: "codex_instructions",
                previous: &self.previous,
            },
        ])
        .map(|_| ())
        .map_err(|_| "mirror_sync_config_write_failed".into())
    }
}

fn parse(bytes: &[u8], source: bool) -> Result<DocumentMut, String> {
    let code = if source {
        "mirror_sync_source_config_invalid"
    } else {
        "mirror_sync_mirror_config_invalid"
    };
    std::str::from_utf8(bytes)
        .map_err(|_| code)?
        .parse()
        .map_err(|_| code.into())
}

fn enabled_plugins(document: &DocumentMut) -> Result<BTreeSet<String>, String> {
    let Some(plugins) = document.get("plugins") else {
        return Ok(BTreeSet::new());
    };
    let plugins = plugins
        .as_table_like()
        .ok_or("mirror_sync_plugin_metadata_invalid")?;
    let mut enabled = BTreeSet::new();
    for (id, item) in plugins.iter() {
        let item = item
            .as_table_like()
            .ok_or("mirror_sync_plugin_metadata_invalid")?;
        if let Some(value) = item.get(ENABLED) {
            let value = value
                .as_bool()
                .ok_or("mirror_sync_plugin_metadata_invalid")?;
            if value {
                enabled.insert(id.to_string());
            }
        }
    }
    Ok(enabled)
}

pub(super) fn prepare(
    native: &Path,
    mirror: &Path,
    budget: &mut scan::Budget,
) -> Result<Inputs, String> {
    super::require_unmanaged_client(super::Client::Codex)?;
    let Some(bytes) = scan::read_metadata_file(&native.join("config.toml"), budget, CONFIG_LIMIT)?
    else {
        return Ok(Inputs {
            source_present: false,
            source_plugins: BTreeSet::new(),
            mirror_plugins: BTreeSet::new(),
            update: None,
        });
    };
    let source = parse(&bytes, true)?;
    let source_plugins = enabled_plugins(&source)?;
    let agents = source
        .get("agents")
        .map(|item| {
            item.as_table_like()
                .ok_or("mirror_sync_source_config_invalid")
        })
        .transpose()?;
    let enabled = agents
        .and_then(|agents| agents.get(ENABLED))
        .map(|value| value.as_bool().ok_or("mirror_sync_source_config_invalid"))
        .transpose()?;
    let threads = agents
        .and_then(|agents| agents.get(THREADS))
        .map(|value| {
            value
                .as_integer()
                .filter(|value| *value > 0)
                .ok_or("mirror_sync_source_config_invalid")
        })
        .transpose()?;
    let path = mirror.join("config.toml");
    let mirror_exists = scan::metadata(&path)?.is_some();
    if !mirror_exists && enabled.is_none() && threads.is_none() {
        return Ok(Inputs {
            source_present: true,
            source_plugins,
            mirror_plugins: BTreeSet::new(),
            update: None,
        });
    }
    private_fs::prepare_managed_private_dir(mirror)
        .map_err(|_| "mirror_sync_private_storage_refused")?;
    if scan::metadata(&path)?.is_some_and(|meta| meta.len() > CONFIG_LIMIT) {
        return Err("mirror_sync_size_limit".into());
    }
    let previous =
        private_fs::read_mirror_file(&path).map_err(|_| "mirror_sync_private_storage_refused")?;
    let mut document = match previous.contents.as_deref() {
        Some(bytes) => parse(bytes, false)?,
        None => DocumentMut::new(),
    };
    let mirror_plugins = enabled_plugins(&document)?;
    let mut changed = false;
    if document.get("agents").is_none() && (enabled.is_some() || threads.is_some()) {
        document.insert("agents", Item::Table(Table::new()));
    }
    if let Some(agents) = document.get_mut("agents") {
        let agents = agents
            .as_table_like_mut()
            .ok_or("mirror_sync_mirror_config_invalid")?;
        match enabled {
            Some(value) if agents.get(ENABLED).and_then(Item::as_bool) != Some(value) => {
                agents.insert(ENABLED, toml_edit::value(value));
                changed = true;
            }
            None if agents.remove(ENABLED).is_some() => changed = true,
            _ => {}
        }
        match threads {
            Some(value) if agents.get(THREADS).and_then(Item::as_integer) != Some(value) => {
                agents.insert(THREADS, toml_edit::value(value));
                changed = true;
            }
            None if agents.remove(THREADS).is_some() => changed = true,
            _ => {}
        }
    }
    let update = changed.then(|| ConfigUpdate {
        path,
        previous,
        contents: document.to_string().into_bytes(),
    });
    Ok(Inputs {
        source_present: true,
        source_plugins,
        mirror_plugins,
        update,
    })
}
