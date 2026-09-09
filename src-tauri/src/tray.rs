//! System tray icon and menu (Phase 9 Step 1).
//!
//! Minimal v1: a fixed Show / Hide / Quit menu. Dynamic menus (per-provider,
//! per-profile hot keys, usage stats in the label) land in later iterations
//! mirroring cc-switch's `tray.rs`. We deliberately stay small here so the
//! iteration stays bounded and verifiable.
//!
//! Reference: cc-switch `src-tauri/src/tray.rs:595` (`create_tray_menu`)
//! and `src-tauri/src/tray.rs:894` (`handle_tray_menu_event`).

use tauri::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Runtime};

/// Stable IDs so menu-event dispatch doesn't depend on display labels.
pub const MENU_SHOW: &str = "aag_tray_show";
pub const MENU_HIDE: &str = "aag_tray_hide";
pub const MENU_QUIT: &str = "aag_tray_quit";

/// Build the fixed Show / Hide / Quit menu. Errors are logged and surface as
/// AppError::Message upstream — but `build_tray` swallows them at runtime so
/// a missing menu doesn't crash the whole app.
pub fn build_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let show = MenuItem::with_id(app, MENU_SHOW, "Show", true, None::<&str>)?;
    let hide = MenuItem::with_id(app, MENU_HIDE, "Hide", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, MENU_QUIT, "Quit", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;

    Menu::with_items(app, &[&show, &sep, &hide, &sep, &quit])
}

/// Install the tray icon + menu. Idempotent: Tauri replaces any prior tray
/// with the same builder id on rebuild.
pub fn install<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let menu = build_menu(app)?;
    let icon = app
        .default_window_icon()
        .cloned()
        .ok_or_else(|| tauri::Error::AssetNotFound("default window icon".into()))?;

    TrayIconBuilder::with_id("aag_tray")
        .tooltip("AI Agent Gateway")
        .icon(icon)
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event: MenuEvent| {
            handle_menu_event(app, event);
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

/// Dispatch a menu event to the right action. Called from the
/// `on_menu_event` closure above.
fn handle_menu_event<R: Runtime>(app: &AppHandle<R>, event: MenuEvent) {
    match event.id().as_ref() {
        MENU_SHOW => show_main_window(app),
        MENU_HIDE => {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.hide();
            }
        }
        MENU_QUIT => {
            log::info!("tray: quit requested");
            app.exit(0);
        }
        other => log::warn!("tray: unknown menu id {other}"),
    }
}

/// Bring the main window to the foreground (and un-minimize if needed).
fn show_main_window<R: Runtime>(app: &AppHandle<R>) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}