//! Shared test fixture: isolates AAG into a temp root, secures it to owner-only,
//! captures and restores all AAG_* + client-app env overrides, and clears them
//! for the duration of the test. Replaces ad-hoc `isolated_home()` reimplemented
//! in pi.rs, opencode.rs, vscode_extension.rs, kilo.rs, and e2e.rs.

#![cfg(test)]

use std::ffi::OsString;
use std::path::PathBuf;

/// Captured process-environment overrides the test must not leak past `Drop`.
const ENV_OVERRIDES: &[&str] = &[
    "AAG_HOME_DIR",
    "AAG_CONFIG_DIR",
    "COPILOT_HOME",
    "PI_CODING_AGENT_DIR",
    "GEMINI_CLI_HOME",
    "OPENCODE_CONFIG",
    "OPENCODE_CONFIG_DIR",
    "KILO_CONFIG",
    "KILO_CONFIG_DIR",
    "XDG_CONFIG_HOME",
];

/// RAII fixture providing a fresh, owner-only temp root redirected through
/// `AAG_HOME_DIR` / `AAG_CONFIG_DIR`. On `Drop` every captured override is
/// restored (or cleared if absent) and the crate env-mutation lock is released.
///
/// Holds the crate-wide `test_env_lock()` for the entire lifetime of the
/// fixture — tests that call `isolated_home()` therefore run sequentially with
/// every other test that mutates process-wide env vars.
pub(crate) struct IsolatedHome {
    /// The temp root AAG will treat as the user's home directory for the
    /// duration of the test. Always present and secured (owner-only DACL on
    /// Windows, mode 0o700 on Unix) before this struct is returned.
    pub root: PathBuf,
    /// Captured snapshots of every process-wide override listed in
    /// `ENV_OVERRIDES`, taken BEFORE the fixture cleared them.
    environment: Vec<(&'static str, Option<OsString>)>,
    /// Crate-wide env-mutation lock. Declared last so that on Drop, env is
    /// restored before the lock is returned to other tests.
    _env_guard: tokio::sync::MutexGuard<'static, ()>,
}

/// Build an `IsolatedHome` rooted at `<temp_dir>/aag-{label}-{pid}-{nonce}`.
/// Acquires `crate::config::test_env_lock()` blocking and runs env captures +
/// install.
///
/// This variant is for **synchronous** tests: it uses `blocking_lock()`, which
/// panics if called from within a Tokio runtime thread. Async tests must use
/// [`isolated_home_async`].
pub(crate) fn isolated_home(label: &str) -> IsolatedHome {
    let env_guard = crate::config::test_env_lock().blocking_lock();
    build_isolated_home(label, env_guard)
}

/// Async variant of [`isolated_home`] for `#[tokio::test]` contexts, where
/// `blocking_lock()` would panic on a runtime thread. Awaits the env lock.
pub(crate) async fn isolated_home_async(label: &str) -> IsolatedHome {
    let env_guard = crate::config::test_env_lock().lock().await;
    build_isolated_home(label, env_guard)
}

fn build_isolated_home(
    label: &str,
    env_guard: tokio::sync::MutexGuard<'static, ()>,
) -> IsolatedHome {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let root = std::env::temp_dir().join(format!(
        "aag-{label}-test-{}-{nonce}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create isolated AAG test root");
    crate::config::private_fs::prepare_managed_private_dir(&root)
        .expect("secure isolated AAG test root to owner-only");

    // Capture then clear every override, then install the test's own values.
    let environment: Vec<(&'static str, Option<OsString>)> = ENV_OVERRIDES
        .iter()
        .map(|key| (*key, std::env::var_os(key)))
        .collect();
    for (key, _) in &environment {
        std::env::remove_var(key);
    }
    let aag_state = root.join("aag_state");
    std::fs::create_dir_all(&aag_state).expect("create aag_state");
    crate::config::private_fs::prepare_managed_private_dir(&aag_state)
        .expect("secure aag_state to owner-only");
    std::env::set_var("AAG_HOME_DIR", &root);
    std::env::set_var("AAG_CONFIG_DIR", &aag_state);

    IsolatedHome { root, environment, _env_guard: env_guard }
}

impl IsolatedHome {
    /// The `AAG_CONFIG_DIR` location for the test (`<root>/aag_state`).
    pub(crate) fn config_dir(&self) -> PathBuf {
        self.root.join("aag_state")
    }
}

impl Drop for IsolatedHome {
    fn drop(&mut self) {
        // Restore every captured env override. The `_env_guard` is the last
        // declared field, so it's dropped last — the lock is only released
        // after env restoration is complete, which is the correct ordering.
        for (key, value) in &self.environment {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}
