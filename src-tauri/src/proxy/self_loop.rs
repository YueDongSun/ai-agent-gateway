//! Self-loop detection: is an upstream actually AAG's own listener?
//!
//! A failover chain or route pointing at the proxy itself loops every
//! client call back into this process — the request stalls in a circle
//! until per-hop timeouts fire, surfacing as a client "port" hang.
//!
//! ONE rule, two entry points sharing this module so they cannot drift:
//!
//! - [`base_url_targets_self`] — the strict rule for explicit coordinates
//!   (proxy startup: pool rows and failover-queue endpoints). Compared by
//!   parsed host:port (never substring) with loopback aliasing, so
//!   `http://127.0.0.1:8989`, `http://localhost:8989`, and the
//!   full-endpoint form `http://127.0.0.1:8989/v1/messages` all trip the
//!   guard while a look-alike foreign host does not.
//!
//! - [`settings_target_self`] — the coarse rule for opaque import payloads
//!   (cc-switch `settings_config` trees, where upstream URLs hide at
//!   unknown nesting depths across app_type schemas). It walks the JSON
//!   tree and substring-matches `host:port`, deliberately over-cautious
//!   because a missed self-loop here is re-imported forever; the strict
//!   rule still guards every runtime dispatch path.

/// True when `base_url` resolves to the proxy's own listen address.
/// `listen_host` empty disables the guard.
pub fn base_url_targets_self(base_url: &str, listen_host: &str, listen_port: u16) -> bool {
    if listen_host.is_empty() {
        return false;
    }
    let Ok(url) = url::Url::parse(base_url) else {
        return false;
    };
    let host_matches = match url.host() {
        Some(url::Host::Domain(name)) => {
            name.eq_ignore_ascii_case(listen_host)
                || (name.eq_ignore_ascii_case("localhost") && listen_host == "127.0.0.1")
                || (name.eq_ignore_ascii_case("127.0.0.1") && listen_host == "localhost")
        }
        Some(url::Host::Ipv4(address)) => {
            address.to_string() == listen_host
                || (address.is_loopback() && listen_host == "localhost")
        }
        Some(url::Host::Ipv6(address)) => {
            address.to_string()
                == listen_host
                || (address.is_loopback()
                    && (listen_host == "localhost"
                        || listen_host == "127.0.0.1"
                        || listen_host == "::1"))
        }
        None => false,
    };
    if !host_matches {
        return false;
    }
    // Compare effective ports (URL default for the scheme when unspecified).
    let url_port = url.port_or_known_default().unwrap_or(0);
    url_port == listen_port
}

/// True if the settings tree contains any string value that names the
/// proxy's listen address. Deliberately substring-based (see module doc):
/// cc-switch schemas differ per app_type, and a value like
/// `http://127.0.0.1:8989/v1` or a bare `127.0.0.1:8989` must both catch.
pub fn settings_target_self(
    settings: &serde_json::Value,
    listen_host: &str,
    listen_port: u16,
) -> bool {
    if listen_host.is_empty() {
        return false;
    }
    let needle = format!("{listen_host}:{listen_port}");
    fn visit(v: &serde_json::Value, needle: &str, hit: &mut bool) {
        match v {
            serde_json::Value::String(s) => {
                if s.contains(needle) {
                    *hit = true;
                }
            }
            serde_json::Value::Array(a) => {
                for item in a {
                    if *hit {
                        return;
                    }
                    visit(item, needle, hit);
                }
            }
            serde_json::Value::Object(o) => {
                for (_k, val) in o {
                    if *hit {
                        return;
                    }
                    visit(val, needle, hit);
                }
            }
            _ => {}
        }
    }
    let mut hit = false;
    visit(settings, &needle, &mut hit);
    hit
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_detects_loopback_aliases_and_full_endpoints() {
        // Loopback host, exact port.
        assert!(base_url_targets_self("http://127.0.0.1:8989", "127.0.0.1", 8989));
        // localhost alias of the same listener.
        assert!(base_url_targets_self("http://localhost:8989", "127.0.0.1", 8989));
        // Full-endpoint base targeting the same listener.
        assert!(base_url_targets_self(
            "http://127.0.0.1:8989/v1/messages",
            "127.0.0.1",
            8989
        ));
        // A path only, still the same host:port.
        assert!(base_url_targets_self("http://127.0.0.1:8989/v1", "127.0.0.1", 8989));
        // Different port is a different service.
        assert!(!base_url_targets_self("http://127.0.0.1:9001", "127.0.0.1", 8989));
        // A real upstream.
        assert!(!base_url_targets_self("https://api.minimaxi.com", "127.0.0.1", 8989));
        // Unparseable / empty input never loops.
        assert!(!base_url_targets_self("not a url", "127.0.0.1", 8989));
        assert!(!base_url_targets_self("", "127.0.0.1", 8989));
        // Empty listen host disables the guard.
        assert!(!base_url_targets_self("http://127.0.0.1:8989", "", 8989));
    }

    #[test]
    fn settings_walk_finds_nested_listen_addresses() {
        let settings = serde_json::json!({
            "env": { "ANTHROPIC_BASE_URL": "http://127.0.0.1:8989" }
        });
        assert!(settings_target_self(&settings, "127.0.0.1", 8989));
        assert!(!settings_target_self(&settings, "127.0.0.1", 9001));
        assert!(!settings_target_self(&serde_json::json!({"env": {}}), "127.0.0.1", 8989));
    }
}
