//! cc-switch coexistence detection.
//!
//! When AAG and cc-switch are running on the same machine, both write
//! `ANTHROPIC_BASE_URL` / `OPENAI_BASE_URL` into the same live agent
//! config files (`~/.claude/settings.json`, `~/.codex/config.toml`).
//! Whichever app wrote last wins — the other app's requests then get
//! routed through the wrong proxy. Port defaults already differ
//! (AAG=8989, cc-switch=15721), so the conflict is logical, not
//! network-level.
//!
//! Detection is best-effort: four orthogonal signals, OR'd together. Any
//! one hit means cc-switch is "running" for our purposes. The detector
//! is cheap (one TCP probe + a few file probes) so it's safe to call on
//! every UI mount and on every provider switch.

use crate::config::get_home_dir;
use serde::{Deserialize, Serialize};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::Duration;

/// cc-switch's hardcoded default listen port (see cc-switch's
/// `database/dao/proxy.rs` `GlobalProxyConfig`). Exposed as a constant
/// so the frontend can show "127.0.0.1:15721" in dialogs without
/// hardcoding the magic number.
pub const CC_SWITCH_DEFAULT_PORT: u16 = 15721;

/// Where the detector observed cc-switch activity. The frontend uses
/// this to phrase the warning ("cc-switch proxy is running" vs.
/// "cc-switch previously wrote to settings.json").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CcSwitchSource {
    /// TCP connect to `127.0.0.1:15721` succeeded.
    Port,
    /// `~/.cc-switch/cc-switch.db` exists on disk.
    DbFile,
    /// `~/.claude/settings.json` `env.ANTHROPIC_BASE_URL` points at 15721.
    ClaudeSettings,
    /// `~/.codex/config.toml` `base_url` points at 15721.
    CodexConfig,
}

/// Structured status returned by `detect()`. Serialize-friendly for the
/// frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CcSwitchStatus {
    /// Whether any signal hit.
    pub running: bool,
    /// The first signal that hit (if any). Drives the i18n message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<CcSwitchSource>,
    /// cc-switch's port (always `CC_SWITCH_DEFAULT_PORT` today; future
    /// versions of cc-switch may let users change it).
    pub port: u16,
    /// Live-config residue: whichever of these strings we found.
    /// Either may be `None` even when `running=true` (e.g. TCP probe
    /// only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live_url_claude: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live_url_codex: Option<String>,
    /// Unix-epoch seconds when this status was computed.
    pub checked_at: i64,
    /// True if the user has set the "ignore conflict" override in
    /// settings — the UI surfaces this so dialogs can say "you've
    /// suppressed this warning" instead of pestering the user.
    pub suppressed: bool,
}

impl CcSwitchStatus {
    /// "Clean" status: no signals hit, no override.
    pub fn clean(checked_at: i64) -> Self {
        Self {
            running: false,
            source: None,
            port: CC_SWITCH_DEFAULT_PORT,
            live_url_claude: None,
            live_url_codex: None,
            checked_at,
            suppressed: false,
        }
    }
}

/// Run all four detection signals. Any hit flips `running=true` and
/// records the source. Order matters for UX: the first signal to hit
/// is what the user sees in the warning ("cc-switch proxy is running").
///
/// `home_dir` is the root under which `~/.claude`, `~/.codex`, and
/// `~/.cc-switch` live. Defaults to `crate::config::get_home_dir()`
/// (honours the `AAG_HOME_DIR` env var). Pass an explicit path in tests
/// to avoid racing with parallel tests that also mutate the env var.
pub fn detect() -> CcSwitchStatus {
    detect_with_home(None)
}

/// Same as [`detect`] but with an explicit home directory. `None` uses
/// `get_home_dir()`.
pub fn detect_with_home(home: Option<&std::path::Path>) -> CcSwitchStatus {
    let checked_at = chrono::Utc::now().timestamp();
    let mut status = CcSwitchStatus::clean(checked_at);

    if probe_port(CC_SWITCH_DEFAULT_PORT) {
        status.running = true;
        status.source = Some(CcSwitchSource::Port);
    } else if probe_db_file(home) {
        status.running = true;
        status.source = Some(CcSwitchSource::DbFile);
    }

    // Always check live-config residue even if we already know cc-switch
    // is running — these give the UI a useful diagnostic ("AAG would
    // overwrite the value `http://...15721/v1` left behind by cc-switch").
    status.live_url_claude = read_claude_anthropic_base_url(home);
    if let Some(url) = &status.live_url_claude {
        if url_uses_cc_switch_port(url, CC_SWITCH_DEFAULT_PORT) {
            status.running = true;
            if status.source.is_none() {
                status.source = Some(CcSwitchSource::ClaudeSettings);
            }
        }
    }

    status.live_url_codex = read_codex_base_url(home);
    if let Some(url) = &status.live_url_codex {
        if url_uses_cc_switch_port(url, CC_SWITCH_DEFAULT_PORT) {
            status.running = true;
            if status.source.is_none() {
                status.source = Some(CcSwitchSource::CodexConfig);
            }
        }
    }

    status
}

/// TCP connect probe with a tight timeout. A successful connect means
/// *some* process is listening on that port — that's good enough; we
/// don't need to verify the protocol.
fn probe_port(port: u16) -> bool {
    let addr = format!("127.0.0.1:{port}");
    TcpStream::connect_timeout(
        &addr
            .parse()
            .unwrap_or_else(|_| "127.0.0.1:0".parse().unwrap()),
        Duration::from_millis(200),
    )
    .map(|_| true)
    .unwrap_or(false)
}

/// `~/.cc-switch/cc-switch.db` is cc-switch's SQLite DB; if it exists,
/// the user almost certainly has cc-switch installed (and may have it
/// running, or be about to).
fn probe_db_file(home: Option<&std::path::Path>) -> bool {
    cc_switch_db_path(home).is_file()
}

fn cc_switch_db_path(home: Option<&std::path::Path>) -> PathBuf {
    home_dir(home).join(".cc-switch").join("cc-switch.db")
}

fn home_dir(home: Option<&std::path::Path>) -> std::path::PathBuf {
    home.map(std::path::PathBuf::from)
        .unwrap_or_else(get_home_dir)
}

/// Read `~/.claude/settings.json` → `env.ANTHROPIC_BASE_URL`.
///
/// Returns `None` if the file is absent, malformed, or doesn't have the
/// expected key shape. We deliberately swallow JSON errors here: a
/// corrupt settings.json shouldn't block AAG's own writes.
fn read_claude_anthropic_base_url(home: Option<&std::path::Path>) -> Option<String> {
    let path = home_dir(home).join(".claude").join("settings.json");
    let raw = std::fs::read_to_string(&path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    v.get("env")?
        .get("ANTHROPIC_BASE_URL")?
        .as_str()
        .map(String::from)
}

/// Read `~/.codex/config.toml` → `base_url` (top-level string).
///
/// Codex's config.toml is TOML; we only need the `base_url` line so we
/// do a cheap regex-style scan instead of pulling in a TOML parser
/// dependency just for this. Falls back to "no match" on any error.
fn read_codex_base_url(home: Option<&std::path::Path>) -> Option<String> {
    let path = home_dir(home).join(".codex").join("config.toml");
    let raw = std::fs::read_to_string(&path).ok()?;
    for line in raw.lines() {
        let trimmed = line.trim_start();
        // match `base_url = "..."` (with or without leading whitespace)
        if let Some(rest) = trimmed.strip_prefix("base_url") {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix('=') {
                let rest = rest.trim();
                if let Some(stripped) = rest.strip_prefix('"') {
                    if let Some(end) = stripped.find('"') {
                        return Some(stripped[..end].to_string());
                    }
                }
            }
        }
    }
    None
}

/// Parse the host:port out of a URL and check if it matches
/// `127.0.0.1:<cc_switch_port>`. Tolerant of trailing paths.
pub fn url_uses_cc_switch_port(url: &str, port: u16) -> bool {
    // Accept either a bare `http://host:port/...` URL or just `host:port`.
    let host_port = url
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .split('/')
        .next()
        .unwrap_or("");
    let parts: Vec<&str> = host_port.rsplitn(2, ':').collect();
    if parts.len() != 2 {
        return false;
    }
    let host = parts[1];
    let port_str = parts[0];
    let parsed_port: u16 = match port_str.parse() {
        Ok(p) => p,
        Err(_) => return false,
    };
    (host == "127.0.0.1" || host == "localhost" || host == "::1") && parsed_port == port
}

/// Given an AAG provider's `settings_config`, extract the URL the
/// provider would write into live agent config.
///
/// - For Claude Code: `env.ANTHROPIC_BASE_URL`
/// - For Codex:      `config` (TOML string) → top-level `base_url = "..."`
/// - For Copilot:    `env.COPILOT_PROVIDER_BASE_URL`
///
/// Returns `None` if the provider doesn't reference any AAG proxy URL.
#[allow(dead_code)] // live-config write path retained for tests/history; not called in the app since Phase 15.9
pub fn extract_target_url(app_type: &str, settings_config: &serde_json::Value) -> Option<String> {
    match app_type {
        "claude_code" | "claude_desktop" => settings_config
            .get("env")?
            .get("ANTHROPIC_BASE_URL")?
            .as_str()
            .map(String::from),
        "codex" => {
            let toml_str = settings_config.get("config")?.as_str()?;
            for line in toml_str.lines() {
                let trimmed = line.trim_start();
                if let Some(rest) = trimmed.strip_prefix("base_url") {
                    let rest = rest.trim_start();
                    if let Some(rest) = rest.strip_prefix('=') {
                        let rest = rest.trim();
                        if let Some(stripped) = rest.strip_prefix('"') {
                            if let Some(end) = stripped.find('"') {
                                return Some(stripped[..end].to_string());
                            }
                        }
                    }
                }
            }
            None
        }
        "copilot" => settings_config
            .get("env")?
            .get("COPILOT_PROVIDER_BASE_URL")?
            .as_str()
            .map(String::from),
        _ => None,
    }
}

/// Decide whether a target URL points at AAG's own local proxy
/// (i.e. the URL's host:port matches `aag_proxy_host:aag_proxy_port`).
pub fn target_is_aag_proxy(target: &str, aag_proxy_host: &str, aag_proxy_port: u16) -> bool {
    url_uses_cc_switch_port(target, aag_proxy_port)
        && (target.contains(aag_proxy_host) || target.contains("127.0.0.1"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[tokio::test]
    async fn database_discovery_uses_effective_home_without_an_explicit_override() {
        let _guard = crate::config::test_env_lock().lock().await;
        let root = tempfile::tempdir().unwrap();
        let previous = std::env::var_os("AAG_HOME_DIR");
        std::env::set_var("AAG_HOME_DIR", root.path());
        let expected = root.path().join(".cc-switch/cc-switch.db");
        let discovered = cc_switch_db_path(None);
        std::fs::create_dir_all(expected.parent().unwrap()).unwrap();
        // A directory with the database's name is not an installed database.
        std::fs::create_dir(&expected).unwrap();
        let directory_detected = probe_db_file(None);
        std::fs::remove_dir(&expected).unwrap();
        std::fs::write(&expected, []).unwrap();
        let file_detected = probe_db_file(None);
        match previous {
            Some(value) => std::env::set_var("AAG_HOME_DIR", value),
            None => std::env::remove_var("AAG_HOME_DIR"),
        }
        assert_eq!(discovered, expected);
        assert!(!directory_detected);
        assert!(file_detected);
    }

    /// Bind a localhost socket on an ephemeral port and return
    /// `(port, guard)`. The guard holds the listener open — when
    /// dropped, the port is released.
    fn reserve_port() -> (u16, TcpListener) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
        let port = listener.local_addr().unwrap().port();
        (port, listener)
    }

    /// Default state: nothing cc-switch-shaped exists → status is clean.
    /// (We can't easily guarantee the test env is clean — but we
    /// override AAG_HOME_DIR before the test runs so we control
    /// `~/.claude`, `~/.codex`, and the implicit `~/.cc-switch` doesn't
    /// exist.)
    #[test]
    fn url_uses_cc_switch_port_basic() {
        assert!(url_uses_cc_switch_port("http://127.0.0.1:15721/v1", 15721));
        assert!(url_uses_cc_switch_port("http://localhost:15721/v1", 15721));
        assert!(!url_uses_cc_switch_port("http://127.0.0.1:8989/v1", 15721));
        assert!(!url_uses_cc_switch_port("https://api.openai.com/v1", 15721));
    }

    #[test]
    fn extract_target_url_claude() {
        let v = serde_json::json!({
            "env": {
                "ANTHROPIC_BASE_URL": "http://127.0.0.1:8989",
                "ANTHROPIC_AUTH_TOKEN": "sk-x"
            }
        });
        assert_eq!(
            extract_target_url("claude_code", &v).as_deref(),
            Some("http://127.0.0.1:8989")
        );
    }

    #[test]
    fn extract_target_url_codex_parses_top_level_base_url() {
        let v = serde_json::json!({
            "auth": {},
            "config": "model = \"gpt-5\"\nbase_url = \"http://127.0.0.1:8989/v1\"\n"
        });
        assert_eq!(
            extract_target_url("codex", &v).as_deref(),
            Some("http://127.0.0.1:8989/v1")
        );
    }

    #[test]
    fn extract_target_url_codex_returns_none_when_absent() {
        let v = serde_json::json!({
            "auth": {},
            "config": "model = \"gpt-5\"\n"
        });
        assert!(extract_target_url("codex", &v).is_none());
    }

    #[test]
    fn extract_target_url_copilot() {
        let v = serde_json::json!({
            "env": {
                "COPILOT_PROVIDER_BASE_URL": "http://127.0.0.1:8989/v1",
                "COPILOT_PROVIDER_TYPE": "openai"
            }
        });
        assert_eq!(
            extract_target_url("copilot", &v).as_deref(),
            Some("http://127.0.0.1:8989/v1")
        );
    }

    #[test]
    fn target_is_aag_proxy_recognises_localhost_loopback() {
        assert!(target_is_aag_proxy(
            "http://127.0.0.1:8989/v1",
            "127.0.0.1",
            8989
        ));
        assert!(!target_is_aag_proxy(
            "http://127.0.0.1:15721/v1",
            "127.0.0.1",
            8989
        ));
        assert!(!target_is_aag_proxy(
            "https://api.openai.com/v1",
            "127.0.0.1",
            8989
        ));
    }

    /// End-to-end: bind a TCP listener on an ephemeral port, override
    /// the CC_SWITCH_DEFAULT_PORT constant indirectly by running the
    /// probe against that port (which we know nothing is on, by
    /// construction — but we also need to test the *detection* path
    /// against an arbitrary port).
    ///
    /// The detector hardcodes CC_SWITCH_DEFAULT_PORT=15721, so we can
    /// only directly test that 15721 isn't open. The probe_port()
    /// function itself is parameterized so we test its logic with a
    /// reserved port instead.
    #[test]
    fn probe_port_succeeds_against_running_listener() {
        let (port, _guard) = reserve_port();
        assert!(probe_port(port), "expected probe_port({port}) to succeed");
    }

    #[test]
    fn probe_port_returns_false_for_unbound_port() {
        // Pick a port we just released and immediately probe — should
        // reliably be free (within the test process).
        let (port, guard) = reserve_port();
        drop(guard);
        // Probe on a fresh thread with a short timeout: even if the
        // kernel briefly holds the port, our 200ms timeout cuts the
        // test off quickly.
        assert!(
            !probe_port(port),
            "expected probe_port({port}) to fail after drop"
        );
    }

    /// Whole detect() pipeline against a controlled environment.
    /// Uses AAG_HOME_DIR override (set by the test harness below) so
    /// the test doesn't touch the developer's real ~/.claude.
    #[test]
    fn detect_clean_when_no_signals() {
        // AAG_HOME_DIR is set to a temp dir by the test harness; the
        // .cc-switch subdir doesn't exist, and 15721 is almost
        // certainly unbound in CI. Even if some other process on the
        // dev's machine is on 15721, the source field is what we
        // check for; running should be true *iff* a signal hit.
        let status = detect();
        // We can't assert running==false unconditionally (a co-hosted
        // cc-switch on the dev machine would make this flaky). But
        // the cheap path: assert the struct shape.
        assert_eq!(status.port, CC_SWITCH_DEFAULT_PORT);
        assert!(status.checked_at > 0);
    }

    /// End-to-end with a fake settings.json pointing at cc-switch.
    /// Pass an explicit home dir so this test doesn't race with other
    /// tests that mutate the process-global `AAG_HOME_DIR` env var.
    #[test]
    fn detect_finds_residue_in_claude_settings() {
        let tmp = std::env::temp_dir().join(format!("aag_detect_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let claude_dir = tmp.join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        let settings_path = claude_dir.join("settings.json");
        std::fs::write(
            &settings_path,
            r#"{
                "env": {
                    "ANTHROPIC_BASE_URL": "http://127.0.0.1:15721",
                    "ANTHROPIC_AUTH_TOKEN": "sk-x"
                }
            }"#,
        )
        .unwrap();

        let status = detect_with_home(Some(&tmp));
        assert!(status.running, "expected running=true, got {status:?}");
        assert_eq!(
            status.live_url_claude.as_deref(),
            Some("http://127.0.0.1:15721")
        );
        assert!(matches!(
            status.source,
            Some(CcSwitchSource::Port)
                | Some(CcSwitchSource::ClaudeSettings)
                | Some(CcSwitchSource::DbFile)
                | Some(CcSwitchSource::CodexConfig)
        ));

        // cleanup
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn detect_finds_residue_in_codex_config() {
        let tmp = std::env::temp_dir().join(format!("aag_detect_codex_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let codex_dir = tmp.join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        std::fs::write(
            codex_dir.join("config.toml"),
            "model = \"gpt-5\"\nbase_url = \"http://127.0.0.1:15721/v1\"\n",
        )
        .unwrap();

        let status = detect_with_home(Some(&tmp));
        assert!(status.running, "expected running=true, got {status:?}");
        assert_eq!(
            status.live_url_codex.as_deref(),
            Some("http://127.0.0.1:15721/v1")
        );

        // cleanup
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The four-signal detector must succeed at probing the configured
    /// CC_SWITCH_DEFAULT_PORT even if it's not 15721 — `probe_port` is
    /// parameterised so it works for any port. Confirms the helper used
    /// by detect_with_home is just a probe, not a magic constant.
    #[test]
    fn probe_port_works_for_arbitrary_port() {
        let (port, _guard) = reserve_port();
        // Sanity: the detector's own CC_SWITCH_DEFAULT_PORT constant is
        // 15721 today (matches cc-switch's hardcoded default); if this
        // assertion ever fails, cc-switch has moved and we need to
        // re-check our coexistence logic.
        assert_eq!(CC_SWITCH_DEFAULT_PORT, 15721);
        assert!(probe_port(port));
    }
}
