//! Public marketplace metadata IPC, independent of DSH/provider state.
use crate::services::dsh::market_catalog::{self, Market_catalog};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
#[allow(non_camel_case_types)]
pub(crate) struct Market_state {
    pub root: PathBuf,
    // A parsed feed is tens of MB, so the app-lifetime retention is by
    // reference (Arc) and an open-source lookup is a refcount bump instead
    // of a full deep copy of the catalog.
    current_catalog: Mutex<Option<Arc<Market_catalog>>>,
}
impl Market_state {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self {
            root,
            current_catalog: Mutex::new(None),
        }
    }
    fn remember_catalog(&self, catalog: Arc<Market_catalog>) -> Result<(), String> {
        let mut current = self
            .current_catalog
            .lock()
            .map_err(|_| "dsh_market:catalog_state_failed".to_string())?;
        *current = Some(catalog);
        Ok(())
    }
    fn source_catalog(&self) -> Result<Arc<Market_catalog>, String> {
        let current = self
            .current_catalog
            .lock()
            .map_err(|_| "dsh_market:catalog_state_failed".to_string())?
            .clone();
        current
            .map(Ok)
            .unwrap_or_else(|| {
                market_catalog::get_catalog(&self.root)
                    .map(Arc::new)
                    .map_err(String::from)
            })
    }
}
#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn dsh_market_get_catalog(
    state: tauri::State<'_, Market_state>,
) -> Result<Arc<Market_catalog>, String> {
    let catalog = Arc::new(market_catalog::refresh_catalog(&state.root)
        .await
        .map_err(String::from)?);
    state.remember_catalog(Arc::clone(&catalog))?;
    Ok(catalog)
}
#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn dsh_market_refresh(
    state: tauri::State<'_, Market_state>,
) -> Result<Arc<Market_catalog>, String> {
    let catalog = Arc::new(market_catalog::refresh_catalog(&state.root)
        .await
        .map_err(String::from)?);
    state.remember_catalog(Arc::clone(&catalog))?;
    Ok(catalog)
}
#[tauri::command(rename_all = "snake_case")]
pub(crate) fn dsh_market_open_source(
    state: tauri::State<'_, Market_state>,
    entry_id: String,
    source_kind: String,
) -> Result<(), String> {
    let catalog = state.source_catalog()?;
    let source =
        market_catalog::source_url(&catalog, &entry_id, &source_kind).map_err(String::from)?;
    crate::dsh_market_window::open_source(&source)
}
#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn dsh_open_marketplace(
    app: tauri::AppHandle,
    locale: String,
    theme: String,
) -> Result<(), String> {
    crate::dsh_market_window::open(&app, &locale, &theme)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_catalog_shares_allocation_and_preserves_ipc_shape() {
        let wire = serde_json::json!({
            "schema_version": 1, "fetched_at": "fixture", "registry_generated_at": "fixture",
            "source_registry": "fixture", "registry_commit": "fixture", "registry_url": "",
            "collector_url": "", "entry_count": 0, "entries": [], "cache_status": "fixture",
            "warning": null,
            "privacy_scrub": {
                "schema_version": 1, "fields": [], "replacement_tokens": {},
                "source_entry_count": 0, "modified_entries": 0, "replacements": {},
                "total_replacements": 0
            }
        });
        let catalog = Arc::new(serde_json::from_value::<Market_catalog>(wire.clone()).unwrap());
        let state = Market_state::new(PathBuf::new());
        state.remember_catalog(Arc::clone(&catalog)).unwrap();
        let retained = state.source_catalog().unwrap();
        assert!(Arc::ptr_eq(&catalog, &retained));
        assert_eq!(Arc::strong_count(&catalog), 3);
        assert_eq!(serde_json::to_value(&catalog).unwrap(), wire);
        assert_eq!(serde_json::to_value(&retained).unwrap(), wire);
    }
}
