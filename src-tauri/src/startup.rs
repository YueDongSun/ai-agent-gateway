//! Native startup behavior for the desktop application.
//!
//! The autostart entry launches the installed Tauri executable with a private
//! argument. This lets the same binary distinguish a normal user launch from
//! a background login launch without introducing a second executable.

use std::env;

/// Argument passed by the autostart registration.
pub const AUTOSTART_ARGUMENT: &str = "--aag-autostart";

/// Return whether the current process was launched by the OS login entry.
pub fn launched_from_autostart() -> bool {
    has_autostart_argument(env::args())
}

fn has_autostart_argument<I>(arguments: I) -> bool
where
    I: IntoIterator<Item = String>,
{
    arguments
        .into_iter()
        .any(|argument| argument == AUTOSTART_ARGUMENT)
}

#[cfg(desktop)]
pub fn enable_autostart<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    use tauri_plugin_autostart::ManagerExt;

    let manager = app.autolaunch();
    match manager.is_enabled() {
        Ok(true) => log::debug!("autostart: already enabled"),
        Ok(false) => match manager.enable() {
            Ok(()) => log::info!("autostart: enabled"),
            Err(_) => log::warn!("autostart: enable failed"),
        },
        Err(_) => log::warn!("autostart: state inspection failed"),
    }
}

#[cfg(desktop)]
pub fn show_main_window_for_normal_launch<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    use tauri::Manager;

    if launched_from_autostart() {
        log::info!("startup: launched in tray-only mode");
        return;
    }

    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

#[cfg(test)]
mod tests {
    use super::{has_autostart_argument, AUTOSTART_ARGUMENT};

    #[test]
    fn detects_only_the_private_autostart_argument() {
        assert!(has_autostart_argument(vec![
            "ai-agent-gateway.exe".to_string(),
            AUTOSTART_ARGUMENT.to_string(),
        ]));
        assert!(!has_autostart_argument(vec![
            "ai-agent-gateway.exe".to_string(),
            "--show".to_string(),
        ]));
    }
}
