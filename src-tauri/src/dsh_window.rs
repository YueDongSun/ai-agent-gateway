//! Native DSH presentation; remote content receives no AAG command authority.
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};
pub(crate) fn trusted_manager(label: &str) -> bool {
    label == "main"
}
pub(crate) fn allowed_navigation(target: &url::Url, origin: &url::Url) -> bool {
    target.scheme() == "http"
        && target.host_str() == Some("127.0.0.1")
        && target.origin() == origin.origin()
        && target.username().is_empty()
        && target.password().is_none()
}
pub(crate) fn open(app: &tauri::AppHandle, web_url: &str) -> Result<(), String> {
    let origin = url::Url::parse(web_url).map_err(|_| "dsh:invalid_web_origin")?;
    if !allowed_navigation(&origin, &origin) {
        return Err("dsh:invalid_web_origin".into());
    }
    let label = format!("dsh_web_{}", origin.port_or_known_default().unwrap_or(0));
    for (existing_label, window) in app.webview_windows() {
        if existing_label.starts_with("dsh_web_") && existing_label != label {
            let _ = window.close();
        }
    }
    if let Some(previous) = app.get_webview_window(&label) {
        previous
            .navigate(origin)
            .map_err(|_| "dsh:window_navigation_failed")?;
        previous.show().map_err(|_| "dsh:window_show_failed")?;
        return previous
            .set_focus()
            .map_err(|_| "dsh:window_focus_failed".into());
    }
    let navigation_origin = origin.clone();
    WebviewWindowBuilder::new(app, &label, WebviewUrl::External(origin))
        .title("DSH — AI Agent Gateway")
        .inner_size(1200.0, 850.0)
        .min_inner_size(700.0, 500.0)
        .incognito(true)
        .on_navigation(move |target| allowed_navigation(target, &navigation_origin))
        .on_new_window(|_, _| tauri::webview::NewWindowResponse::Deny)
        .on_download(|_, _| false)
        .build()
        .map_err(|_| "dsh:window_open_failed")?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remote_plugin_windows_cannot_be_manager_subjects() {
        assert!(trusted_manager("main"));
        for label in ["dsh_web", "main_child", "", "MAIN"] {
            assert!(!trusted_manager(label));
        }
    }
    #[test]
    fn navigation_is_bound_to_one_loopback_origin() {
        let origin = url::Url::parse("http://127.0.0.1:3141/").unwrap();
        assert!(allowed_navigation(
            &origin.join("/settings?tab=plugins").unwrap(),
            &origin
        ));
        for target in [
            "http://127.0.0.1:3142/",
            "http://localhost:3141/",
            "https://127.0.0.1:3141/",
            "http://user@127.0.0.1:3141/",
            "https://example.invalid/",
            "file:///tmp/private",
            "javascript:alert(1)",
        ] {
            assert!(
                !allowed_navigation(&url::Url::parse(target).unwrap(), &origin),
                "{target}"
            );
        }
    }
}
