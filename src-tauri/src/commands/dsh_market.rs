//! Public marketplace metadata IPC, independent of DSH/provider state.
use crate::services::dsh::market_catalog::{self, Market_catalog};
use std::path::PathBuf;
#[allow(non_camel_case_types)]
pub(crate) struct Market_state {
    pub root: PathBuf,
}
#[tauri::command(rename_all = "snake_case")]
pub(crate) fn dsh_market_get_catalog(
    state: tauri::State<'_, Market_state>,
) -> Result<Market_catalog, String> {
    market_catalog::get_catalog(&state.root).map_err(String::from)
}
#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn dsh_market_refresh(
    state: tauri::State<'_, Market_state>,
) -> Result<Market_catalog, String> {
    market_catalog::refresh_catalog(&state.root)
        .await
        .map_err(String::from)
}
#[tauri::command(rename_all = "snake_case")]
pub(crate) fn dsh_market_open_source(
    state: tauri::State<'_, Market_state>,
    entry_id: String,
    source_kind: String,
) -> Result<(), String> {
    let catalog = market_catalog::get_catalog(&state.root).map_err(String::from)?;
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
