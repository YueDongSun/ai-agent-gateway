//! Deep-link URL routing (Phase 9 Step 2).
//!
//! Minimal v1: any `aag://` URL the OS hands us is forwarded to the frontend
//! via the `"aag://deeplink"` Tauri event as `{ urls: string[] }`. The frontend
//! decides what to do with the URL. Real URL parsing (`aag://switch/<id>`,
//! `aag://mcp/<id>`, etc.) lands in Step 3.
//!
//! On macOS / iOS / Android, URLs come in via `app.deep_link().on_open_url()`.
//! On Windows / Linux, the OS spawns a fresh instance with the URL on argv —
//! `tauri-plugin-single-instance` forwards that argv to us. We unify by
//! forwarding every `aag://...` argv token through the same emit channel.
//!
//! Reference: cc-switch's `src-tauri/src/deeplink/mod.rs` (deep-link URL
//! parsing + handler dispatch).

use tauri::{AppHandle, Emitter, Runtime};
use tauri_plugin_deep_link::DeepLinkExt;

/// Event name the frontend listens on.
pub const EVENT_NAME: &str = "aag://deeplink";

/// Payload shape: an array of URL strings (mirrors what the JS
/// `@tauri-apps/plugin-deep-link` `onOpenUrl` callback receives).
#[derive(serde::Serialize, Clone)]
pub struct DeeplinkPayload {
    pub urls: Vec<String>,
}

/// Scan `argv` for `aag://...` tokens. Returns them in order. Strips common
/// Windows quirks (a stray leading `--` or empty tokens).
pub fn extract_aag_urls(argv: &[String]) -> Vec<String> {
    argv.iter()
        .filter(|a| a.starts_with("aag://"))
        .cloned()
        .collect()
}

/// Emit the deep-link event to the frontend. No-op if `urls` is empty.
pub fn emit_urls<R: Runtime>(app: &AppHandle<R>, urls: Vec<String>) {
    if urls.is_empty() {
        return;
    }
    let payload = DeeplinkPayload { urls };
    if app.emit(EVENT_NAME, payload).is_err() {
        log::warn!("deeplink: event emit failed");
    }
}

/// Install the deep-link URL listener. Called from `lib.rs::run` setup hook.
pub fn install<R: Runtime>(app: &AppHandle<R>) {
    let handle = app.clone();
    app.deep_link().on_open_url(move |event| {
        let urls: Vec<String> = event.urls().into_iter().map(|u| u.to_string()).collect();
        log::info!("deeplink: received {} url(s) via on_open_url", urls.len());
        emit_urls(&handle, urls);
    });
    log::info!("deeplink: on_open_url listener registered");
}

/// Forward CLI argv URLs through the same channel. Called from the
/// single-instance callback so Linux/Windows see deep-link events.
pub fn forward_argv<R: Runtime>(app: &AppHandle<R>, argv: &[String]) {
    let urls = extract_aag_urls(argv);
    if urls.is_empty() {
        log::debug!("deeplink: no aag:// urls in forwarded argv");
        return;
    }
    log::info!("deeplink: forwarded {} url(s) from argv", urls.len());
    emit_urls(app, urls);
}

// ─── Tests ───────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_aag_urls_finds_only_aag_scheme() {
        let argv = vec![
            "aag.exe".into(),
            "--flag".into(),
            "aag://switch/provider-1".into(),
            "https://example.com".into(),
            "aag://mcp/server-2".into(),
            "".into(),
        ];
        let urls = extract_aag_urls(&argv);
        assert_eq!(urls.len(), 2);
        assert_eq!(urls[0], "aag://switch/provider-1");
        assert_eq!(urls[1], "aag://mcp/server-2");
    }

    #[test]
    fn extract_aag_urls_returns_empty_when_no_match() {
        let argv = vec!["aag.exe".into(), "--something".into(), "https://x".into()];
        assert!(extract_aag_urls(&argv).is_empty());
    }

    #[test]
    fn extract_aag_urls_handles_empty_argv() {
        assert!(extract_aag_urls(&[]).is_empty());
    }

    #[test]
    fn extract_aag_urls_is_case_sensitive() {
        let argv = vec!["AAG://upper".into(), "aag://lower".into()];
        let urls = extract_aag_urls(&argv);
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0], "aag://lower");
    }
}
