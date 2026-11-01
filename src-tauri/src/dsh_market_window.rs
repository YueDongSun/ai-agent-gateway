//! Bundled marketplace with public metadata authority only.
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};
pub(crate) fn allowed_command(label: &str, command: &str) -> bool {
    crate::dsh_window::trusted_manager(label)
        || (label == "dsh_market"
            && matches!(
                command,
                "dsh_market_get_catalog" | "dsh_market_refresh" | "dsh_market_open_source"
            ))
}
fn presentation(locale: &str, theme: &str) -> Result<String, String> {
    if !matches!(locale, "en" | "zh") || !matches!(theme, "light" | "dark") {
        return Err("dsh_market:invalid_presentation".into());
    }
    Ok(format!(
        "index.html?view=dsh_market&locale={locale}&theme={theme}"
    ))
}
fn app_origin(url: &url::Url) -> bool {
    if !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    matches!(
        (url.scheme(), url.host_str()),
        ("tauri", Some("localhost"))
            | (
                "http" | "https",
                Some("tauri.localhost" | "localhost" | "127.0.0.1" | "[::1]" | "::1")
            )
    )
}
pub(crate) fn allowed_navigation(target: &url::Url, app_url: &url::Url) -> bool {
    // tauri:// has an opaque URL origin; compare its actual components.
    app_origin(target)
        && app_origin(app_url)
        && target.scheme() == app_url.scheme()
        && target.host_str() == app_url.host_str()
        && target.port_or_known_default() == app_url.port_or_known_default()
        && matches!(target.path(), "/" | "/index.html")
        && target
            .query_pairs()
            .filter(|(key, _)| key == "view")
            .collect::<Vec<_>>()
            == vec![("view".into(), "dsh_market".into())]
}
pub(crate) fn open(app: &tauri::AppHandle, locale: &str, theme: &str) -> Result<(), String> {
    let path = presentation(locale, theme)?;
    if let Some(window) = app.get_webview_window("dsh_market") {
        window
            .unminimize()
            .map_err(|_| "dsh_market:window_restore_failed")?;
        window.show().map_err(|_| "dsh_market:window_show_failed")?;
        window
            .eval("window.dispatchEvent(new Event('dsh_market_opened'))")
            .map_err(|_| "dsh_market:window_refresh_signal_failed")?;
        return window
            .set_focus()
            .map_err(|_| "dsh_market:window_focus_failed".into());
    }
    let main = app
        .get_webview_window("main")
        .ok_or("dsh_market:manager_unavailable")?;
    let origin = main
        .url()
        .map_err(|_| "dsh_market:manager_url_unavailable")?;
    if !app_origin(&origin) {
        return Err("dsh_market:invalid_manager_origin".into());
    }
    WebviewWindowBuilder::new(app, "dsh_market", WebviewUrl::App(path.into()))
        .title(if locale == "zh" {
            "DSH 插件市场 — AAG"
        } else {
            "DSH Plugin Marketplace — AAG"
        })
        .inner_size(1320.0, 900.0)
        .min_inner_size(480.0, 560.0)
        .on_navigation(move |target| allowed_navigation(target, &origin))
        .on_new_window(|_, _| tauri::webview::NewWindowResponse::Deny)
        .on_download(|_, _| false)
        .build()
        .map_err(|_| "dsh_market:window_open_failed")?;
    Ok(())
}
pub(crate) fn open_source(url: &str) -> Result<(), String> {
    let parsed = url::Url::parse(url).map_err(|_| "dsh_market:invalid_source_url")?;
    let is_site_feed = url == "https://dsh.market/plugins.json";
    if parsed.scheme() != "https"
        || (!is_site_feed
            && !matches!(
                parsed.host_str(),
                Some("github.com" | "raw.githubusercontent.com")
            ))
        || (parsed.host_str() == Some("dsh.market") && !is_site_feed)
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.port().is_some()
        || parsed.query().is_some()
        || url.chars().any(char::is_control)
    {
        return Err("dsh_market:invalid_source_url".into());
    }
    open_validated_url(parsed.as_str())
}
#[cfg(windows)]
fn open_validated_url(url: &str) -> Result<(), String> {
    use std::ffi::c_void;
    #[link(name = "shell32")]
    extern "system" {
        fn ShellExecuteW(
            window: *mut c_void,
            operation: *const u16,
            file: *const u16,
            parameters: *const u16,
            directory: *const u16,
            show: i32,
        ) -> *mut c_void;
    }
    let operation: Vec<u16> = "open\0".encode_utf16().collect();
    let target: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();
    // User-requested handoff to the system browser; no command shell.
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            target.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
        )
    } as isize;
    if result > 32 {
        Ok(())
    } else {
        Err("dsh_market:source_open_failed".into())
    }
}
#[cfg(not(windows))]
fn open_validated_url(url: &str) -> Result<(), String> {
    let program = if cfg!(target_os = "macos") {
        "/usr/bin/open"
    } else {
        "xdg-open"
    };
    let mut child = std::process::Command::new(program)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|_| "dsh_market:source_open_failed")?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn market_authority_is_an_exact_subset() {
        for command in [
            "dsh_market_get_catalog",
            "dsh_market_refresh",
            "dsh_market_open_source",
        ] {
            assert!(allowed_command("main", command));
            assert!(allowed_command("dsh_market", command));
            for label in ["dsh_web_3141", "dsh_market_child", "", "DSH_MARKET"] {
                assert!(!allowed_command(label, command));
            }
        }
        for command in [
            "provider_get",
            "provider_list",
            "proxy_status",
            "proxy_config_get",
            "settings_get",
            "settings_set",
            "diagnostics_get",
            "diagnostics_request_timeline",
            "diagnostics_clear_request_timeline",
            "dsh_get_snapshot",
            "dsh_list_upstreams",
            "dsh_start",
            "dsh_save_settings",
            "dsh_execute_operation",
            "dsh_open_marketplace",
            "unknown",
        ] {
            assert!(!allowed_command("dsh_market", command));
        }
    }
    #[test]
    fn market_navigation_cannot_enter_manager_or_remote_content() {
        for base in [
            "http://localhost:1421/",
            "http://tauri.localhost/",
            "tauri://localhost/",
        ] {
            let origin = url::Url::parse(base).unwrap();
            assert!(allowed_navigation(
                &origin.join(&presentation("zh", "dark").unwrap()).unwrap(),
                &origin
            ));
            assert!(!allowed_navigation(&origin, &origin));
            for target in [
                "http://evil.invalid/index.html?view=dsh_market",
                "http://localhost:9999/?view=dsh_market",
                "file:///index.html?view=dsh_market",
                "javascript:alert(1)",
                "http://localhost:1421/?view=dsh_market&view=dsh",
            ] {
                assert!(!allowed_navigation(
                    &url::Url::parse(target).unwrap(),
                    &origin
                ));
            }
        }
        assert!(presentation("cn", "dark").is_err());
        assert!(presentation("en&view=dsh", "light").is_err());
        assert!(presentation("en", "system").is_err());
    }
    #[test]
    fn reject_unsafe_source_before_os_handoff() {
        for value in [
            "file:///etc/passwd",
            "https://github.com.evil.invalid/r",
            "https://x@github.com/r",
            "https://github.com/r?token=secret",
            "javascript:alert(1)",
        ] {
            assert!(open_source(value).is_err());
        }
    }
}
