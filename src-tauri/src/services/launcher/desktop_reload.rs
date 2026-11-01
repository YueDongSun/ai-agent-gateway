//! Windows reload step for one managed desktop mirror: find the running
//! Electron main process bound to the exact executable and `--user-data-dir`
//! profile, close it gracefully when managed files newer than its start
//! exist, and never force-kill or touch native and other-profile windows.

use std::path::{Path, PathBuf};

#[cfg(windows)]
use super::{discovery, process};
#[cfg(any(windows, test))]
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Capture budget for one `Win32_Process` listing.
#[cfg(windows)]
const QUERY_TIMEOUT: Duration = Duration::from_secs(10);
/// Grace period for `CloseMainWindow` before a manual restart is requested.
#[cfg(windows)]
const CLOSE_DEADLINE: Duration = Duration::from_secs(15);
/// `WaitForExit` poll granularity inside the close script, in milliseconds.
#[cfg(windows)]
const CLOSE_POLL_INTERVAL_MS: u64 = 250;
#[cfg(windows)]
const CONFIGURATION_RECEIPT: &str = "aag_launch_state.json";
#[cfg(windows)]
const MAX_RECEIPT_BYTES: u64 = 4096;
#[cfg(windows)]
const RECEIPT_PROBE_ATTEMPTS: usize = 3;
/// Stable error for any process query whose output cannot be trusted.
#[cfg(any(windows, test))]
const ERROR_QUERY_FAILED: &str = "desktop_reload_process_query_failed";
#[cfg(windows)]
const ERROR_PATH_NOT_ABSOLUTE: &str = "desktop_reload_path_not_absolute";
#[cfg(windows)]
const ERROR_PATH_INVALID: &str = "desktop_reload_path_invalid";
#[cfg(windows)]
const ERROR_RESTART_REQUIRED: &str = "desktop_mirror_restart_required";
/// 100-nanosecond ticks per second in a Windows FILETIME.
#[cfg(any(windows, test))]
const FILETIME_TICKS_PER_SECOND: u64 = 10_000_000;
/// Seconds between the FILETIME epoch (1601-01-01) and the Unix epoch.
#[cfg(any(windows, test))]
const FILETIME_EPOCH_OFFSET_SECONDS: u64 = 11_644_473_600;

/// Only the validated executable file name is interpolated into the script,
/// and no path travels on any command line. The PowerShell filter stays a
/// superset (same file name, non-null executable path) so every exact
/// decision is made by the tested Rust selectors below.
#[cfg(windows)]
const QUERY_SCRIPT_TEMPLATE: &str = r#"
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$hits = Get-CimInstance Win32_Process -Filter "Name = '{{EXECUTABLE_NAME}}'" -ErrorAction Stop | Where-Object { $_.ExecutablePath } | ForEach-Object {
  [pscustomobject]@{ pid = $_.ProcessId; created = [string]$_.CreationDate.ToFileTimeUtc(); exe = [string]$_.ExecutablePath; cmd = [string]$_.CommandLine }
}
if ($null -eq $hits) { '[]'; exit 0 }
ConvertTo-Json -Compress -InputObject @($hits)
exit 0
"#;

/// Revalidate the exact PID (creation time and executable) once more at close
/// time, then request a graceful close and wait at most `DEADLINE_SECONDS`.
#[cfg(windows)]
const CLOSE_SCRIPT_TEMPLATE: &str = r#"
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$row = Get-CimInstance Win32_Process -Filter "ProcessId = $env:AAG_RELOAD_PID" -ErrorAction SilentlyContinue
if ($null -eq $row) { 'gone'; exit 0 }
if ([string]$row.CreationDate.ToFileTimeUtc() -ne $env:AAG_RELOAD_CREATED) { 'gone'; exit 0 }
if ([string]$row.ExecutablePath -ine $env:AAG_RELOAD_EXE) { 'gone'; exit 0 }
$proc = Get-Process -Id $row.ProcessId -ErrorAction SilentlyContinue
if ($null -eq $proc -or $proc.HasExited) { 'gone'; exit 0 }
try { $null = $proc.CloseMainWindow() } catch { }
$deadline = [DateTime]::UtcNow.AddSeconds({{DEADLINE_SECONDS}})
while (-not $proc.HasExited -and [DateTime]::UtcNow -lt $deadline) {
  $null = $proc.WaitForExit({{POLL_MILLISECONDS}})
}
if ($proc.HasExited) { 'exited'; exit 0 }
'running'
exit 0
"#;

/// Whether a graceful mirror reload was prepared. `Ok(false)`: no running
/// main process matches the exact executable and profile, or no managed file
/// is newer than the process creation time. `Ok(true)`: a stale mirror was
/// asked to close and had exited when rechecked (a vanished process counts
/// as closed). `Err("desktop_mirror_restart_required")`: it still runs after
/// the graceful deadline; the caller must surface a manual restart instead
/// of force-killing active work.
#[cfg(windows)]
pub(crate) fn prepare_reload(
    exe: &Path,
    profile: &Path,
    managed_files: &[PathBuf],
) -> Result<bool, String> {
    prepare_reload_inner(exe, profile, managed_files, None)
}

#[cfg(windows)]
pub(crate) fn prepare_reload_with_configuration(
    exe: &Path,
    profile: &Path,
    managed_files: &[PathBuf],
    configuration: &str,
) -> Result<bool, String> {
    prepare_reload_inner(exe, profile, managed_files, Some(configuration))
}

#[cfg(windows)]
fn prepare_reload_inner(
    exe: &Path,
    profile: &Path,
    managed_files: &[PathBuf],
    configuration: Option<&str>,
) -> Result<bool, String> {
    validate_inputs(exe, profile, managed_files)?;
    let rows = query_process_rows(exe)?;
    let Some(mirror) = select_mirror_process(&rows, exe, profile)? else {
        return Ok(false);
    };
    let creation = mirror
        .creation_filetime()
        .ok_or_else(|| ERROR_QUERY_FAILED.to_string())?;
    let Some(creation_time) = filetime_to_system_time(creation) else {
        return Err(ERROR_QUERY_FAILED.to_string());
    };
    if let Some(configuration) = configuration {
        let receipt_path = profile.join(CONFIGURATION_RECEIPT);
        crate::config::private_fs::validate_external_file_path(&receipt_path)
            .map_err(String::from)?;
        let receipt = if receipt_path.exists() {
            crate::config::private_fs::verify_private_file(&receipt_path).map_err(String::from)?;
            let size = std::fs::metadata(&receipt_path)
                .map_err(|_| ERROR_QUERY_FAILED.to_string())?
                .len();
            if size > MAX_RECEIPT_BYTES {
                return Err(ERROR_QUERY_FAILED.to_string());
            }
            std::fs::read(&receipt_path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        } else {
            None
        };
        if receipt
            .as_ref()
            .is_some_and(|receipt| configuration_matches(receipt, mirror, configuration))
        {
            return Ok(false);
        }
    } else if !is_stale(creation_time, &managed_file_mtimes(managed_files)) {
        return Ok(false);
    }
    // The PID may have been recycled since the listing, so the exact mirror
    // (same PID, same creation time) is revalidated before any window
    // message is sent; the close script checks creation time and executable
    // once more at close time.
    let rows = query_process_rows(exe)?;
    match select_mirror_process(&rows, exe, profile)? {
        Some(revalidated)
            if revalidated.pid == mirror.pid
                && revalidated.creation_filetime() == Some(creation) =>
        {
            close_and_wait(revalidated.pid, creation, exe)?;
            if select_mirror_process(&query_process_rows(exe)?, exe, profile)?.is_some() {
                return Err(ERROR_RESTART_REQUIRED.to_string());
            }
            Ok(true)
        }
        // The stale main process is already gone; nothing is left to close.
        Some(_) => Err(ERROR_RESTART_REQUIRED.to_string()),
        None => Ok(true),
    }
}

#[cfg(any(windows, test))]
fn configuration_matches(
    receipt: &serde_json::Value,
    process: &ProcessRow,
    configuration: &str,
) -> bool {
    receipt["configuration"].as_str() == Some(configuration)
        && receipt["pid"].as_u64() == Some(u64::from(process.pid))
        && receipt["created"].as_str() == Some(process.created.as_str())
}

/// Bind the configuration receipt to the actual new main process. Native
/// preference writes may change config.toml's mtime without changing AAG's
/// connection or catalogue, so mtime alone cannot identify a stale Codex.
#[cfg(windows)]
pub(crate) fn remember_configuration(
    exe: &Path,
    profile: &Path,
    configuration: &str,
) -> Result<(), String> {
    for _ in 0..RECEIPT_PROBE_ATTEMPTS {
        if let Some(process) = select_mirror_process(&query_process_rows(exe)?, exe, profile)? {
            let receipt = serde_json::json!({"configuration":configuration,"pid":process.pid,"created":process.created});
            return crate::config::private_fs::atomic_write(
                &profile.join(CONFIGURATION_RECEIPT),
                &serde_json::to_vec(&receipt).map_err(|_| ERROR_QUERY_FAILED.to_string())?,
            )
            .map_err(String::from);
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    Err("desktop_launch_identity_unavailable".into())
}

#[cfg(not(windows))]
pub(crate) fn prepare_reload(
    _exe: &Path,
    _profile: &Path,
    _managed_files: &[PathBuf],
) -> Result<bool, String> {
    Err("desktop_reload_platform_unsupported".to_string())
}

#[cfg(windows)]
fn validate_inputs(exe: &Path, profile: &Path, managed_files: &[PathBuf]) -> Result<(), String> {
    for path in std::iter::once(exe)
        .chain(std::iter::once(profile))
        .chain(managed_files.iter().map(PathBuf::as_path))
    {
        if !path.is_absolute() {
            return Err(ERROR_PATH_NOT_ABSOLUTE.to_string());
        }
        if path.to_string_lossy().chars().any(char::is_control) {
            return Err(ERROR_PATH_INVALID.to_string());
        }
    }
    executable_leaf(exe).map(|_| ())
}

/// File name of `exe` for the WQL `Name` filter; quote characters would
/// break out of the filter's string literal, so such names are refused.
#[cfg(windows)]
fn executable_leaf(exe: &Path) -> Result<&str, String> {
    exe.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty() && !name.contains(['\'', '"']))
        .ok_or_else(|| ERROR_PATH_INVALID.to_string())
}

/// One `Win32_Process` row as emitted by the query script.
#[cfg(any(windows, test))]
#[derive(Clone, Debug, serde::Deserialize)]
struct ProcessRow {
    pid: u32,
    /// Creation FILETIME as decimal digits (strings keep the 64-bit value
    /// exact regardless of any JSON number handling on the way).
    created: String,
    exe: Option<String>,
    cmd: Option<String>,
}

#[cfg(any(windows, test))]
impl ProcessRow {
    fn creation_filetime(&self) -> Option<u64> {
        self.created.trim().parse().ok()
    }
}

/// Accept both shapes `ConvertTo-Json` emits for zero, one, and many hits;
/// anything else is a corrupt capture and must not influence closing.
#[cfg(any(windows, test))]
fn parse_process_rows(output: &str) -> Result<Vec<ProcessRow>, String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let value: serde_json::Value =
        serde_json::from_str(trimmed).map_err(|_| ERROR_QUERY_FAILED.to_string())?;
    let rows = match value {
        serde_json::Value::Array(rows) => rows,
        row @ serde_json::Value::Object(_) => vec![row],
        _ => return Err(ERROR_QUERY_FAILED.to_string()),
    };
    rows.into_iter()
        .map(serde_json::from_value::<ProcessRow>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ERROR_QUERY_FAILED.to_string())
}

/// The unique Electron main process bound to this exact executable and
/// profile. Exact profile equality (not a contains match) is what keeps
/// native and other-profile windows out of closing scope.
#[cfg(any(windows, test))]
fn select_mirror_process<'rows>(
    rows: &'rows [ProcessRow],
    exe: &Path,
    profile: &Path,
) -> Result<Option<&'rows ProcessRow>, String> {
    let mut targets: Vec<&ProcessRow> = Vec::new();
    for row in rows {
        let Some(command_line) = row.cmd.as_deref() else {
            continue;
        };
        let argv = windows_argv(command_line);
        if !is_main_process(&argv) || !argv_targets_profile(&argv, profile) {
            continue;
        }
        if !row
            .exe
            .as_deref()
            .is_some_and(|path| equivalent_windows_paths(path, &exe.to_string_lossy()))
        {
            continue;
        }
        // A matched row without a usable creation time can never be
        // identity-checked before closing, so the whole query is refused
        // instead of quietly closing an unverifiable process.
        row.creation_filetime()
            .ok_or_else(|| ERROR_QUERY_FAILED.to_string())?;
        targets.push(row);
    }
    if targets.len() > 1 {
        return Err("desktop_mirror_restart_required".to_string());
    }
    Ok(targets.into_iter().next())
}

/// Split a Windows command line the way the C runtime does, so quoted
/// `--user-data-dir` values containing spaces survive as one argument.
/// Backslash rules: 2n backslashes before a quote become n backslashes and
/// toggle quoting; 2n+1 become n backslashes plus a literal quote.
#[cfg(any(windows, test))]
fn windows_argv(command_line: &str) -> Vec<String> {
    let mut argv = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut has_token = false;
    let mut backslashes = 0usize;
    for character in command_line.chars() {
        match character {
            '\\' => backslashes += 1,
            '"' => {
                current.push_str(&"\\".repeat(backslashes / 2));
                if backslashes % 2 == 1 {
                    current.push('"');
                } else {
                    in_quotes = !in_quotes;
                }
                backslashes = 0;
            }
            _ => {
                current.push_str(&"\\".repeat(backslashes));
                backslashes = 0;
                if !in_quotes && character.is_whitespace() {
                    if has_token {
                        argv.push(std::mem::take(&mut current));
                        has_token = false;
                    }
                } else {
                    current.push(character);
                    has_token = true;
                }
            }
        }
    }
    if backslashes > 0 {
        current.push_str(&"\\".repeat(backslashes));
        has_token = true;
    }
    if has_token {
        argv.push(current);
    }
    argv
}

/// The value of the Chromium `--user-data-dir` switch, in both `=`-joined
/// and separate forms, matched case-insensitively like Chromium itself.
#[cfg(any(windows, test))]
fn user_data_dir_value(argv: &[String]) -> Option<&str> {
    let mut arguments = argv.iter();
    while let Some(argument) = arguments.next() {
        let text = argument.as_str();
        let (name, joined) = match text.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (text, None),
        };
        if name.trim_start_matches('-').to_lowercase() != "user-data-dir" {
            continue;
        }
        if let Some(value) = joined {
            return Some(value);
        }
        return arguments.next().map(String::as_str);
    }
    None
}

/// Electron main process: Chromium child processes carry a `--type=` switch
/// (`renderer`, `gpu-process`, `utility`); the main process does not.
#[cfg(any(windows, test))]
fn is_main_process(argv: &[String]) -> bool {
    argv.iter().all(|argument| {
        let name = argument
            .split_once('=')
            .map_or(argument.as_str(), |(name, _)| name);
        !name.trim_start_matches('-').to_lowercase().eq("type")
    })
}

#[cfg(any(windows, test))]
fn argv_targets_profile(argv: &[String], profile: &Path) -> bool {
    user_data_dir_value(argv)
        .is_some_and(|value| equivalent_windows_paths(value, &profile.to_string_lossy()))
}

/// Windows path spelling equivalence for values that arrive as raw strings
/// (command lines) versus `Path` values: separators unified, the `\\?\`
/// verbatim prefix dropped, trailing separator dropped, case folded. Not a
/// canonicalization: alternate names for the same file compare unequal.
#[cfg(any(windows, test))]
fn equivalent_windows_paths(left: &str, right: &str) -> bool {
    fn path_key(value: &str) -> String {
        let trimmed = value.trim();
        let without_prefix = trimmed
            .strip_prefix("\\\\?\\UNC\\")
            .map(|rest| format!("\\\\{rest}"))
            .unwrap_or_else(|| {
                trimmed
                    .strip_prefix("\\\\?\\")
                    .unwrap_or(trimmed)
                    .to_string()
            });
        without_prefix
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_lowercase()
    }
    path_key(left) == path_key(right)
}

/// Convert a Windows FILETIME (100ns ticks since 1601-01-01 UTC) to
/// `SystemTime`; `None` when the value predates the Unix epoch.
#[cfg(any(windows, test))]
fn filetime_to_system_time(filetime: u64) -> Option<SystemTime> {
    let seconds = filetime / FILETIME_TICKS_PER_SECOND;
    let nanoseconds = (filetime % FILETIME_TICKS_PER_SECOND) * 100;
    seconds
        .checked_sub(FILETIME_EPOCH_OFFSET_SECONDS)
        .map(|unix_seconds| UNIX_EPOCH + Duration::new(unix_seconds, nanoseconds as u32))
}

/// A mirror is stale when any managed file was modified strictly after the
/// process started; an equal timestamp is treated as already loaded, and a
/// missing or unreadable file carries no timestamp and cannot trigger a
/// reload.
#[cfg(any(windows, test))]
fn is_stale(process_creation: SystemTime, file_mtimes: &[Option<SystemTime>]) -> bool {
    file_mtimes
        .iter()
        .any(|mtime| matches!(mtime, Some(modified) if *modified > process_creation))
}

#[cfg(windows)]
fn managed_file_mtimes(managed_files: &[PathBuf]) -> Vec<Option<SystemTime>> {
    managed_files
        .iter()
        .map(|path| {
            std::fs::metadata(path)
                .ok()
                .and_then(|meta| meta.modified().ok())
        })
        .collect()
}

#[cfg(windows)]
fn query_process_rows(exe: &Path) -> Result<Vec<ProcessRow>, String> {
    let executable_name = executable_leaf(exe)?;
    let script = QUERY_SCRIPT_TEMPLATE.replace("{{EXECUTABLE_NAME}}", executable_name);
    let mut command = process::background_command(discovery::powershell()?);
    command.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
    let bytes = process::capture_stdout(&mut command, QUERY_TIMEOUT)?;
    let output = String::from_utf8(bytes).map_err(|_| ERROR_QUERY_FAILED.to_string())?;
    parse_process_rows(&output)
}

#[cfg(windows)]
fn close_and_wait(pid: u32, creation_filetime: u64, exe: &Path) -> Result<bool, String> {
    let script = CLOSE_SCRIPT_TEMPLATE
        .replace(
            "{{DEADLINE_SECONDS}}",
            &CLOSE_DEADLINE.as_secs().to_string(),
        )
        .replace("{{POLL_MILLISECONDS}}", &CLOSE_POLL_INTERVAL_MS.to_string());
    let mut command = process::background_command(discovery::powershell()?);
    command.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
    command
        .env("AAG_RELOAD_PID", pid.to_string())
        .env("AAG_RELOAD_CREATED", creation_filetime.to_string())
        .env("AAG_RELOAD_EXE", exe);
    // The script is bounded by CLOSE_DEADLINE internally; the capture budget
    // adds one query budget of slack for PowerShell startup.
    let bytes = process::capture_stdout(&mut command, QUERY_TIMEOUT + CLOSE_DEADLINE)?;
    let token = String::from_utf8_lossy(&bytes).trim().to_string();
    match token.as_str() {
        "gone" | "exited" => Ok(true),
        "running" => Err(ERROR_RESTART_REQUIRED.to_string()),
        _ => Err(ERROR_QUERY_FAILED.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const EXE: &str = r"C:\fixture\Apps\Codex\Codex.exe";
    const PROFILE: &str = r"C:\fixture\mirror\electron";

    #[test]
    fn configuration_receipt_binds_effective_settings_to_one_process() {
        let process = row(42, "133900000000000000", EXE, "fixture");
        let receipt = serde_json::json!({"configuration":"fixture_revision","pid":42,"created":process.created});
        assert!(configuration_matches(
            &receipt,
            &process,
            "fixture_revision"
        ));
        assert!(!configuration_matches(
            &receipt,
            &process,
            "changed_revision"
        ));
        assert!(!configuration_matches(
            &receipt,
            &row(43, "133900000000000000", EXE, "fixture"),
            "fixture_revision"
        ));
        assert!(!configuration_matches(
            &receipt,
            &row(42, "133900000000000001", EXE, "fixture"),
            "fixture_revision"
        ));
    }

    fn row(pid: u32, created: &str, exe: &str, cmd: &str) -> ProcessRow {
        ProcessRow {
            pid,
            created: created.to_string(),
            exe: Some(exe.to_string()),
            cmd: Some(cmd.to_string()),
        }
    }

    fn main_argv(profile_value: &str) -> Vec<String> {
        windows_argv(&format!(
            r#""{EXE}" --user-data-dir="{profile_value}" --enable-features=x"#
        ))
    }

    #[test]
    fn windows_argv_splits_on_unquoted_whitespace_only() {
        assert_eq!(
            windows_argv(
                r#""C:\fixture\Apps\Codex.exe" --user-data-dir="C:\fixture\mirror\electron dir" --flag"#
            ),
            [
                r"C:\fixture\Apps\Codex.exe",
                r"--user-data-dir=C:\fixture\mirror\electron dir",
                "--flag"
            ]
        );
        // 2n+1 backslashes before a quote: n backslashes plus a literal quote.
        assert_eq!(
            windows_argv(r#""a\"b" c"#),
            ["a\"b".to_string(), "c".to_string()]
        );
        // A trailing run of backslashes belongs to the final token.
        assert_eq!(windows_argv(r#"dir\"#), [r#"dir\"#.to_string()]);
    }

    #[test]
    fn user_data_dir_value_accepts_joined_separate_and_case_forms() {
        assert_eq!(
            user_data_dir_value(&["--user-data-dir=C:\\fixture\\m\\e".into(), "--flag".into()]),
            Some("C:\\fixture\\m\\e")
        );
        assert_eq!(
            user_data_dir_value(&["--USER-DATA-DIR".into(), r"C:\fixture\m\e".into()]),
            Some(r"C:\fixture\m\e")
        );
        assert_eq!(user_data_dir_value(&["-user-data-dir=x".into()]), Some("x"));
        assert_eq!(user_data_dir_value(&["--user-data-dir2=x".into()]), None);
        assert_eq!(user_data_dir_value(&["--other=y".into()]), None);
    }

    #[test]
    fn profile_match_is_exact_and_spelling_insensitive() {
        let profile = Path::new(PROFILE);
        // Unquoted values: a QUOTED value ending in a backslash is genuinely
        // a different value under the MSVCRT rules covered by the test above.
        for spelling in [
            r"C:\fixture\mirror\electron",
            r"C:\fixture\MIRROR\ELECTRON",
            "C:/fixture/mirror/electron",
            r#"C:\fixture\mirror\electron\"#,
            r"\\?\C:\fixture\mirror\electron",
        ] {
            let argv = windows_argv(&format!("prog.exe --user-data-dir={spelling} --flag"));
            assert!(
                argv_targets_profile(&argv, profile),
                "spelling {spelling} must match"
            );
        }
        // A sibling profile sharing this path as a prefix must not match.
        assert!(!argv_targets_profile(
            &main_argv(r"C:\fixture\mirror\electron2"),
            profile
        ));
        // A process without --user-data-dir (native default profile) never matches.
        assert!(!argv_targets_profile(
            &windows_argv(r#""x" --flag"#),
            profile
        ));
    }

    #[test]
    fn child_type_switches_are_not_main_processes() {
        assert!(is_main_process(&main_argv(PROFILE)));
        for child_switch in ["--type=renderer", "--type=gpu-process", "--type=utility"] {
            let argv = main_argv(PROFILE);
            let mut with_type = vec![child_switch.to_string()];
            with_type.extend(argv);
            assert!(!is_main_process(&with_type), "{child_switch} is a child");
        }
    }

    #[test]
    fn select_mirror_process_requires_one_exact_main() {
        let exe = Path::new(EXE);
        let profile = Path::new(PROFILE);
        let rows = vec![
            row(
                101,
                "133900000000000000",
                EXE,
                r#""C:\fixture\Apps\Codex\Codex.exe" --type=renderer --user-data-dir="C:\fixture\mirror\electron""#,
            ),
            row(
                102,
                "133900000000000000",
                EXE,
                r#""C:\fixture\Apps\Codex\Codex.exe" --flag"#,
            ),
            row(
                103,
                "133900000000000000",
                r"C:\fixture\Other\Other.exe",
                r#""C:\fixture\Other\Other.exe" --user-data-dir="C:\fixture\mirror\electron""#,
            ),
            row(
                104,
                "133700000000000000",
                EXE,
                r#""C:\fixture\Apps\Codex\Codex.exe" --user-data-dir="C:\fixture\mirror\electron""#,
            ),
            row(
                105,
                "133800000000000000",
                EXE,
                r#""C:\fixture\Apps\Codex\Codex.exe" --user-data-dir="c:\fixture\MIRROR\electron""#,
            ),
        ];
        assert_eq!(
            select_mirror_process(&rows, exe, profile).unwrap_err(),
            "desktop_mirror_restart_required"
        );
        let selected = select_mirror_process(&rows[..4], exe, profile)
            .unwrap()
            .unwrap();
        assert_eq!(selected.pid, 104);
        assert!(select_mirror_process(&[], exe, profile).unwrap().is_none());
    }

    #[test]
    fn matched_row_with_unusable_creation_time_refuses_the_query() {
        let exe = Path::new(EXE);
        let profile = Path::new(PROFILE);
        let rows = vec![row(
            106,
            "not-a-number",
            EXE,
            r#""C:\fixture\Apps\Codex\Codex.exe" --user-data-dir="C:\fixture\mirror\electron""#,
        )];
        assert_eq!(
            select_mirror_process(&rows, exe, profile).unwrap_err(),
            ERROR_QUERY_FAILED.to_string()
        );
    }

    #[test]
    fn parse_process_rows_wraps_single_object_and_rejects_garbage() {
        assert!(parse_process_rows("").unwrap().is_empty());
        assert!(parse_process_rows("[]").unwrap().is_empty());
        let single = parse_process_rows(
            r#"{"pid":4242,"created":"133800000000000000","exe":"C:\\a\\Codex.exe","cmd":"\"C:\\a\\Codex.exe\" --user-data-dir=\"C:\\fixture\\m\\e\""}"#,
        )
        .unwrap();
        assert_eq!(single.len(), 1);
        assert_eq!(single[0].pid, 4242);
        assert_eq!(single[0].creation_filetime(), Some(133800000000000000));
        let several = parse_process_rows(
            r#"[{"pid":1,"created":"1","exe":null,"cmd":null},{"pid":2,"created":"2","exe":"e","cmd":"c"}]"#,
        )
        .unwrap();
        assert_eq!(several.len(), 2);
        for garbage in ["not json", "5", r#"{"pid":1}"#] {
            assert_eq!(
                parse_process_rows(garbage).unwrap_err(),
                ERROR_QUERY_FAILED.to_string(),
                "no content may leak for {garbage}"
            );
        }
    }

    #[test]
    fn filetime_conversion_covers_epoch_and_underflow() {
        assert_eq!(filetime_to_system_time(0), None);
        let epoch = FILETIME_EPOCH_OFFSET_SECONDS * FILETIME_TICKS_PER_SECOND;
        assert_eq!(filetime_to_system_time(epoch), Some(UNIX_EPOCH));
        // Sub-second ticks survive; a whole second before the epoch does not.
        assert_eq!(
            filetime_to_system_time(epoch + 1),
            Some(UNIX_EPOCH + Duration::new(0, 100))
        );
        assert_eq!(
            filetime_to_system_time(epoch - FILETIME_TICKS_PER_SECOND),
            None
        );
        assert_eq!(
            filetime_to_system_time(116444736010000000),
            Some(UNIX_EPOCH + Duration::from_secs(1))
        );
    }

    #[test]
    fn stale_detection_requires_strictly_newer_files() {
        let start = UNIX_EPOCH + Duration::from_secs(1000);
        assert!(!is_stale(start, &[]));
        assert!(!is_stale(start, &[None]));
        assert!(!is_stale(
            start,
            &[Some(start), Some(start - Duration::from_secs(1))]
        ));
        assert!(is_stale(
            start,
            &[None, Some(start + Duration::from_secs(1))]
        ));
    }
}
