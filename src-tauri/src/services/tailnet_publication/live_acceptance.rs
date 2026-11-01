//! Prepared personal acceptance only; never part of an ordinary test run.
//! The manager publishes its normal strict API. This exercise sends only GET
//! metadata requests and never sends an inference, conversation, or model call.
//! No saved settings, existing gateway, service, firewall, or Funnel are changed.

use super::{
    metadata, read_serve_status, OwnedRoute, PublicationConfig, PublicationManager,
    PublicationState, ServeStatus,
};
use crate::{
    database::Database,
    proxy::server::ProxyAppState,
    services::owned_process::{
        encode_environment, run_capture, NativeChild, NativeProcessError, SpawnSpec,
    },
};
use rusqlite::{Connection, OpenFlags};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    ffi::{c_void, OsString},
    fs::File,
    io::Write,
    net::{Ipv4Addr, SocketAddr},
    os::windows::{ffi::OsStringExt, fs::MetadataExt, io::AsRawHandle},
    panic::{catch_unwind, AssertUnwindSafe},
    path::{Path, PathBuf},
    sync::{atomic::AtomicBool, Arc, Mutex},
    time::{Duration, Instant},
};

const OPT_IN: &str = "personal_peer_roundtrip";
const LIVE_PORT: u16 = 8989;
const HTTP_TIMEOUT: Duration = Duration::from_secs(5);
const START_TIMEOUT: Duration = Duration::from_secs(25);
const STOP_TIMEOUT: Duration = Duration::from_secs(20);
const PEER_TIMEOUT: Duration = Duration::from_secs(20);
const PEER_STOP_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(20);
const INPUT_LIMIT: usize = 4096;
const OUTPUT_LIMIT: usize = 4096;
const RECEIPT_LIMIT: usize = 1024;
const SSH_CONFIG_LIMIT: usize = 64 * 1024;
const WRONG_KEY: &str = "aag_live_acceptance_wrong_key";

#[derive(Clone, Copy)]
struct AcceptanceError(u16);
type AcceptanceResult<T> = Result<T, AcceptanceError>;

struct SecretBytes(Vec<u8>);
impl Drop for SecretBytes {
    fn drop(&mut self) {
        for byte in &mut self.0 {
            unsafe {
                std::ptr::write_volatile(byte, 0);
            }
        }
    }
}
struct SecretString(String);
impl Drop for SecretString {
    fn drop(&mut self) {
        // Writing zero preserves UTF-8 validity. Clear the owned primary copy;
        // HTTP/runtime string copies are released with their owning scopes.
        unsafe {
            for byte in self.0.as_mut_vec() {
                std::ptr::write_volatile(byte, 0);
            }
        }
    }
}

struct Admission {
    state: ProxyAppState,
    key: SecretString,
    client: reqwest::Client,
    local_models: String,
    own_ip: String,
    executable: PathBuf,
    before: ServeStatus,
    local_statuses: [u16; 3],
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadyReceipt {
    phase: u8,
    ready: bool,
    major: u16,
    minor: u16,
    #[serde(rename = "micro")]
    _micro: u16,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthReceipt {
    phase: u8,
    keyless: u16,
    wrong: u16,
    correct: u16,
    keepalive: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClosedReceipt {
    phase: u8,
    existing_closed: bool,
    fresh_failed: bool,
}
#[derive(Serialize)]
struct PeerInput<'a> {
    host: &'a str,
    port: u16,
    key: &'a str,
}
#[derive(Serialize)]
struct StopInput {
    stop: bool,
}
#[derive(Default)]
struct RoundtripReceipt {
    remote_statuses: [u16; 3],
    existing_closed: bool,
    fresh_failed: bool,
    same_owner: bool,
    unrelated_preserved: bool,
}

struct PeerSelection {
    alias: String,
    address: Ipv4Addr,
}
impl PeerSelection {
    fn parse(alias: &str, address: &str) -> AcceptanceResult<Self> {
        if alias.is_empty()
            || alias.len() > 64
            || !alias.as_bytes()[0].is_ascii_alphanumeric()
            || !alias
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        {
            return Err(AcceptanceError(1));
        }
        let parsed: Ipv4Addr = address.parse().map_err(|_| AcceptanceError(1))?;
        if u32::from(parsed) & 0xffc0_0000 != 0x6440_0000 || parsed.to_string() != address {
            return Err(AcceptanceError(1));
        }
        Ok(Self {
            alias: alias.into(),
            address: parsed,
        })
    }
}

fn exact_opt_in(marker: &str, alias: &str, address: &str, port: &str) -> bool {
    marker == OPT_IN && port == "8989" && PeerSelection::parse(alias, address).is_ok()
}

/// No native action is reachable before the exact marker/port and explicit
/// validated peer alias plus expected overlay address are supplied.
#[test]
#[ignore = "personal live Tailnet roundtrip; requires exact marker/port and explicit peer alias plus expected overlay IP"]
fn personal_peer_roundtrip() {
    let peer_alias = std::env::var("AAG_TAILNET_PEER_ALIAS").unwrap_or_default();
    let peer_address = std::env::var("AAG_TAILNET_EXPECTED_PEER_IP").unwrap_or_default();
    if !exact_opt_in(
        &std::env::var("AAG_TAILNET_LIVE_ACCEPTANCE").unwrap_or_default(),
        &peer_alias,
        &peer_address,
        &std::env::var("AAG_TAILNET_LIVE_PORT").unwrap_or_default(),
    ) {
        panic!("tailnet_acceptance_failed code=1");
    }
    let selection = PeerSelection::parse(&peer_alias, &peer_address)
        .unwrap_or_else(|_| panic!("tailnet_acceptance_failed code=1"));

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap_or_else(|_| panic!("tailnet_acceptance_failed code=2"));
    let mut admission = runtime
        .block_on(prepare())
        .unwrap_or_else(|error| panic!("tailnet_acceptance_failed code={}", error.0));
    let manager = PublicationManager::new(admission.executable.clone());
    let mut peer = None;
    let exercised = catch_unwind(AssertUnwindSafe(|| {
        runtime.block_on(exercise(&manager, &admission, &selection, &mut peer))
    }));

    // This synchronous outer owner remains alive across async failure/unwind.
    // Stop is always awaited before peer cleanup and before reporting failure.
    let stopped = runtime
        .block_on(async { tokio::time::timeout(STOP_TIMEOUT, manager.stop()).await })
        .is_ok_and(|result| {
            result.is_ok_and(|snapshot| snapshot.state == PublicationState::Stopped)
        });
    let peer_stopped = peer
        .as_mut()
        .is_none_or(|peer: &mut PeerProcess| peer.close().is_ok());
    let preserved = read_serve_status(&admission.executable, &AtomicBool::new(false))
        .is_ok_and(|after| admission.before.equivalent_to(&after));
    let local_after = runtime
        .block_on(local_status(
            &admission.client,
            &admission.local_models,
            Some(&admission.key.0),
        ))
        .unwrap_or(0);
    // Release the admitted router's credential copy after the manager is stopped.
    unsafe {
        for byte in admission.state.auth_token.as_mut_vec() {
            std::ptr::write_volatile(byte, 0);
        }
    }
    let (receipt, error) = match exercised {
        Ok(Ok(receipt)) => (receipt, 0),
        Ok(Err(error)) => (RoundtripReceipt::default(), error.0),
        Err(_) => (RoundtripReceipt::default(), 90),
    };
    println!(
        "{}",
        serde_json::json!({
            "error": error,
            "local_keyless": admission.local_statuses[0], "local_wrong": admission.local_statuses[1],
            "local_correct": admission.local_statuses[2], "local_after": local_after,
            "remote_keyless": receipt.remote_statuses[0], "remote_wrong": receipt.remote_statuses[1],
            "remote_correct": receipt.remote_statuses[2], "existing_closed": receipt.existing_closed,
            "fresh_failed": receipt.fresh_failed, "same_owner": receipt.same_owner,
            "unrelated_preserved": preserved && receipt.unrelated_preserved,
            "manager_stopped": stopped, "peer_stopped": peer_stopped,
        })
    );
    assert!(
        error == 0 && stopped && peer_stopped && preserved && local_after == 200,
        "tailnet_acceptance_failed code=91"
    );
}

async fn prepare() -> AcceptanceResult<Admission> {
    let path = crate::config::get_db_path();
    if !path.is_absolute() || !path.is_file() {
        return Err(AcceptanceError(3));
    }
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| AcceptanceError(4))?;
    let db = Database {
        conn: Arc::new(Mutex::new(connection)),
    };
    let mut config = db.get_proxy_config().map_err(|_| AcceptanceError(5))?;
    if !config.is_loopback_bind()
        || config.port == 0
        || config.auth_token.trim().is_empty()
        || config.auth_token.len() > 1024
        || !config
            .auth_token
            .bytes()
            .all(|byte| (33..=126).contains(&byte))
    {
        return Err(AcceptanceError(6));
    }
    let saved_publication = super::settings::read_config(&db).map_err(|_| AcceptanceError(7))?;
    if saved_publication.port != LIVE_PORT {
        return Err(AcceptanceError(7));
    }
    let local_models = format!(
        "http://{}:{}/v1/models",
        config.client_endpoint_host(),
        config.port
    );
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(HTTP_TIMEOUT)
        .build()
        .map_err(|_| AcceptanceError(8))?;
    let wrong = if config.auth_token == WRONG_KEY {
        "aag_live_acceptance_wrong_key_alternate"
    } else {
        WRONG_KEY
    };
    let local_statuses = [
        local_status(&client, &local_models, None).await?,
        local_status(&client, &local_models, Some(wrong)).await?,
        local_status(&client, &local_models, Some(&config.auth_token)).await?,
    ];
    if local_statuses != [401, 401, 200] {
        return Err(AcceptanceError(9));
    }
    let snapshot = tokio::time::timeout(
        HTTP_TIMEOUT,
        crate::services::proxy_runtime::snapshot::build_runtime_snapshot(&db, &config),
    )
    .await
    .map_err(|_| AcceptanceError(10))?
    .map_err(|_| AcceptanceError(10))?;
    let key = SecretString(std::mem::take(&mut config.auth_token));
    let executable = metadata::trusted_executable().map_err(|_| AcceptanceError(11))?;
    let before =
        read_serve_status(&executable, &AtomicBool::new(false)).map_err(|_| AcceptanceError(12))?;
    let plan = PublicationConfig {
        enabled: true,
        port: LIVE_PORT,
    }
    .plan(&key.0, "127.0.0.1:1")
    .map_err(|_| AcceptanceError(13))?;
    before.preflight(&plan).map_err(|_| AcceptanceError(13))?;
    let own_ip = metadata::read_self_status()
        .map_err(|_| AcceptanceError(14))?
        .self_ip
        .ok_or(AcceptanceError(14))?
        .to_string();
    let state = ProxyAppState {
        router: snapshot.router,
        auth_token: key.0.clone(),
        pinned_provider: Arc::new(Mutex::new(snapshot.pinned_provider)),
        responses_cot_enabled: Arc::new(AtomicBool::new(false)),
        passthrough_auth_allowed: false,
        db: None,
        live: None,
        allow_client_config_repair: false,
    };
    Ok(Admission {
        state,
        key,
        client,
        local_models,
        own_ip,
        executable,
        before,
        local_statuses,
    })
}

async fn local_status(
    client: &reqwest::Client,
    endpoint: &str,
    key: Option<&str>,
) -> AcceptanceResult<u16> {
    let mut request = client.get(endpoint);
    if let Some(key) = key {
        request = request.bearer_auth(key);
    }
    request
        .send()
        .await
        .map(|response| response.status().as_u16())
        .map_err(|_| AcceptanceError(15))
}

async fn exercise(
    manager: &PublicationManager,
    admission: &Admission,
    selection: &PeerSelection,
    peer_slot: &mut Option<PeerProcess>,
) -> AcceptanceResult<RoundtripReceipt> {
    *peer_slot = Some(PeerProcess::start(selection)?);
    let peer = peer_slot.as_mut().ok_or(AcceptanceError(16))?;
    let ready: ReadyReceipt = peer.receipt(Instant::now() + PEER_TIMEOUT).await?;
    if ready.phase != 0 || !ready.ready || ready.major != 3 || ready.minor < 10 {
        return Err(AcceptanceError(17));
    }
    let started = tokio::time::timeout(
        START_TIMEOUT,
        manager.start(
            PublicationConfig {
                enabled: true,
                port: LIVE_PORT,
            },
            admission.state.clone(),
        ),
    )
    .await
    .map_err(|_| AcceptanceError(18))?
    .map_err(|_| AcceptanceError(18))?;
    if started.state != PublicationState::Running
        || started.port != Some(LIVE_PORT)
        || started.error.is_some()
    {
        return Err(AcceptanceError(18));
    }
    let active = read_serve_status(&admission.executable, &AtomicBool::new(false))
        .map_err(|_| AcceptanceError(19))?;
    let owned = capture_receipt(&admission.before, &active, &admission.key.0)?;
    peer.send(&PeerInput {
        host: &admission.own_ip,
        port: LIVE_PORT,
        key: &admission.key.0,
    })?;
    let auth: AuthReceipt = peer.receipt(Instant::now() + PEER_TIMEOUT).await?;
    if auth.phase != 1
        || [auth.keyless, auth.wrong, auth.correct] != [401, 401, 200]
        || !auth.keepalive
    {
        return Err(AcceptanceError(20));
    }
    let before_stop = read_serve_status(&admission.executable, &AtomicBool::new(false))
        .map_err(|_| AcceptanceError(21))?;
    if !owned.matches(&before_stop) {
        return Err(AcceptanceError(21));
    }
    let stopped = tokio::time::timeout(STOP_TIMEOUT, manager.stop())
        .await
        .map_err(|_| AcceptanceError(22))?
        .map_err(|_| AcceptanceError(22))?;
    if stopped.state != PublicationState::Stopped {
        return Err(AcceptanceError(22));
    }
    let after = read_serve_status(&admission.executable, &AtomicBool::new(false))
        .map_err(|_| AcceptanceError(23))?;
    owned
        .verify_removed(&before_stop, &after)
        .map_err(|_| AcceptanceError(23))?;
    if !admission.before.equivalent_to(&after) {
        return Err(AcceptanceError(23));
    }
    peer.send(&StopInput { stop: true })?;
    let closed: ClosedReceipt = peer.receipt(Instant::now() + PEER_TIMEOUT).await?;
    if closed.phase != 2 || !closed.existing_closed || !closed.fresh_failed {
        return Err(AcceptanceError(24));
    }
    peer.wait_success(Instant::now() + PEER_STOP_TIMEOUT)
        .await?;
    Ok(RoundtripReceipt {
        remote_statuses: [auth.keyless, auth.wrong, auth.correct],
        existing_closed: true,
        fresh_failed: true,
        same_owner: true,
        unrelated_preserved: true,
    })
}

fn capture_receipt(
    before: &ServeStatus,
    after: &ServeStatus,
    key: &str,
) -> AcceptanceResult<OwnedRoute> {
    let foreground = after
        .value
        .get("Foreground")
        .and_then(serde_json::Value::as_object)
        .ok_or(AcceptanceError(25))?;
    let targets: Vec<_> = foreground
        .values()
        .filter_map(|value| value.get("TCP")?.get("8989")?.get("TCPForward")?.as_str())
        .collect();
    let [target] = targets.as_slice() else {
        return Err(AcceptanceError(25));
    };
    let backend: SocketAddr = target.parse().map_err(|_| AcceptanceError(25))?;
    if !backend.ip().is_loopback() || backend.port() == 0 {
        return Err(AcceptanceError(25));
    }
    let plan = PublicationConfig {
        enabled: true,
        port: LIVE_PORT,
    }
    .plan(key, &backend.to_string())
    .map_err(|_| AcceptanceError(25))?;
    before
        .preflight(&plan)
        .and_then(|preflight| preflight.bind(&plan, after))
        .map_err(|_| AcceptanceError(25))
}

#[link(name = "kernel32")]
extern "system" {
    fn GetSystemDirectoryW(buffer: *mut u16, size: u32) -> u32;
    fn GetNamedPipeInfo(
        pipe: *mut c_void,
        flags: *mut u32,
        output: *mut u32,
        input: *mut u32,
        maximum: *mut u32,
    ) -> i32;
}

fn trusted_program_data() -> AcceptanceResult<PathBuf> {
    #[repr(C)]
    struct KnownFolderId {
        first: u32,
        second: u16,
        third: u16,
        last: [u8; 8],
    }
    #[link(name = "shell32")]
    extern "system" {
        fn SHGetKnownFolderPath(
            folder: *const KnownFolderId,
            flags: u32,
            token: *mut c_void,
            path: *mut *mut u16,
        ) -> i32;
    }
    #[link(name = "ole32")]
    extern "system" {
        fn CoTaskMemFree(pointer: *mut c_void);
    }
    // FOLDERID_ProgramData: Windows OpenSSH requires this even for local -G.
    // Resolve it through the OS instead of inheriting caller-controlled values.
    let folder = KnownFolderId {
        first: 0x62ab5d82,
        second: 0xfdc1,
        third: 0x4dc3,
        last: [0xa9, 0xdd, 0x07, 0x0d, 0x1d, 0x49, 0x5d, 0x97],
    };
    let mut pointer = std::ptr::null_mut();
    let result = unsafe { SHGetKnownFolderPath(&folder, 0, std::ptr::null_mut(), &mut pointer) };
    if result < 0 || pointer.is_null() {
        if !pointer.is_null() {
            unsafe { CoTaskMemFree(pointer.cast()) };
        }
        return Err(AcceptanceError(30));
    }
    let mut length = 0;
    while length < 32768 && unsafe { *pointer.add(length) } != 0 {
        length += 1;
    }
    let path = (length > 0 && length < 32768).then(|| {
        PathBuf::from(OsString::from_wide(unsafe {
            std::slice::from_raw_parts(pointer, length)
        }))
    });
    unsafe { CoTaskMemFree(pointer.cast()) };
    path.filter(|path| path.is_absolute() && path.is_dir())
        .ok_or(AcceptanceError(30))
}

fn ssh_environment(
    system: &Path,
    profile: &Path,
    program_data: &Path,
) -> AcceptanceResult<Vec<u16>> {
    let root = system.parent().ok_or(AcceptanceError(30))?;
    // Fixed allowlist: never inherit proxy settings, credentials, or SSH agents.
    encode_environment(&[
        ("SystemRoot".into(), root.as_os_str().to_owned()),
        ("WINDIR".into(), root.as_os_str().to_owned()),
        ("USERPROFILE".into(), profile.as_os_str().to_owned()),
        ("HOME".into(), profile.as_os_str().to_owned()),
        ("PATH".into(), system.as_os_str().to_owned()),
        ("PROGRAMDATA".into(), program_data.as_os_str().to_owned()),
    ])
    .map_err(|_| AcceptanceError(30))
}

fn trusted_ssh() -> AcceptanceResult<(PathBuf, Vec<u16>)> {
    let mut buffer = vec![0u16; 32768];
    let length = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
    if length == 0 || length >= buffer.len() {
        return Err(AcceptanceError(30));
    }
    let system = PathBuf::from(OsString::from_wide(&buffer[..length]));
    let executable = system.join("OpenSSH").join("ssh.exe");
    for path in executable.ancestors() {
        let metadata = std::fs::symlink_metadata(path).map_err(|_| AcceptanceError(30))?;
        if metadata.file_attributes() & 0x400 != 0 || metadata.file_type().is_symlink() {
            return Err(AcceptanceError(30));
        }
    }
    if !executable.is_file() {
        return Err(AcceptanceError(30));
    }
    let profile = crate::config::get_home_dir();
    let program_data = trusted_program_data()?;
    let environment = ssh_environment(&system, &profile, &program_data)?;
    Ok((executable, environment))
}

#[test]
fn ssh_environment_has_only_required_noncredential_windows_variables() {
    let system = Path::new(r"C:\fixture_windows\System32");
    let profile = Path::new(r"C:\fixture_profile");
    let program_data = Path::new(r"C:\fixture_program_data");
    let environment = ssh_environment(system, profile, program_data)
        .unwrap_or_else(|_| panic!("fixture SSH environment must encode"));
    let decoded = String::from_utf16(&environment).unwrap();
    let entries: std::collections::BTreeMap<_, _> = decoded
        .split('\0')
        .filter(|entry| !entry.is_empty())
        .map(|entry| entry.split_once('=').unwrap())
        .collect();
    assert_eq!(
        entries.keys().copied().collect::<Vec<_>>(),
        [
            "HOME",
            "PATH",
            "PROGRAMDATA",
            "SystemRoot",
            "USERPROFILE",
            "WINDIR"
        ]
    );
    assert_eq!(entries["PROGRAMDATA"], program_data.to_str().unwrap());
    for forbidden in [
        "OPENAI_API_KEY",
        "AAG_AUTH_TOKEN",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "SSH_AUTH_SOCK",
    ] {
        assert!(!entries
            .keys()
            .any(|name| name.eq_ignore_ascii_case(forbidden)));
    }
}

fn ssh_options() -> Vec<String> {
    [
        "-T",
        "-o",
        "BatchMode=yes",
        "-o",
        "PreferredAuthentications=publickey",
        "-o",
        "PasswordAuthentication=no",
        "-o",
        "KbdInteractiveAuthentication=no",
        "-o",
        "StrictHostKeyChecking=yes",
        "-o",
        "UpdateHostKeys=no",
        "-o",
        "ConnectTimeout=8",
        "-o",
        "ConnectionAttempts=1",
        "-o",
        "ClearAllForwardings=yes",
        "-o",
        "ForwardAgent=no",
        "-o",
        "PermitLocalCommand=no",
        "-o",
        "RemoteCommand=none",
        "-o",
        "ControlMaster=no",
        "-o",
        "ControlPersist=no",
        "-o",
        "ControlPath=none",
        "-o",
        "SendEnv=-*",
        "-o",
        "LogLevel=ERROR",
        "-o",
        "CanonicalizeHostname=no",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

fn qualify_peer(
    executable: &PathBuf,
    environment: &[u16],
    selection: &PeerSelection,
) -> AcceptanceResult<String> {
    let mut args = ssh_options();
    args.insert(0, "-G".into());
    args.push(selection.alias.clone());
    let mut spec = SpawnSpec::new(executable, &args);
    spec.environment = Some(environment);
    spec.cwd = executable.parent();
    let output = run_capture(
        &spec,
        &AtomicBool::new(false),
        Instant::now() + PEER_TIMEOUT,
        SSH_CONFIG_LIMIT,
    )
    .map_err(|_| AcceptanceError(38))?;
    if output.exit_code != 0 {
        return Err(AcceptanceError(38));
    }
    let bytes = SecretBytes(output.stdout);
    let text = std::str::from_utf8(&bytes.0).map_err(|_| AcceptanceError(38))?;
    let field = |name: &str| -> AcceptanceResult<&str> {
        let mut matches = text.lines().filter_map(|line| {
            line.split_once(' ')
                .filter(|(key, _)| *key == name)
                .map(|(_, value)| value.trim())
        });
        let value = matches.next().ok_or(AcceptanceError(38))?;
        if matches.next().is_some() {
            return Err(AcceptanceError(38));
        }
        Ok(value)
    };
    if field("hostname")? != selection.address.to_string()
        || !matches!(field("stricthostkeychecking")?, "yes" | "true")
        || !matches!(field("updatehostkeys")?, "no" | "false")
        || !matches!(field("batchmode")?, "yes" | "true")
        || field("preferredauthentications")? != "publickey"
        || !matches!(field("passwordauthentication")?, "no" | "false")
        || !matches!(field("kbdinteractiveauthentication")?, "no" | "false")
    {
        return Err(AcceptanceError(38));
    }
    let user = field("user")?;
    if user.is_empty()
        || user.len() > 128
        || !user
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(AcceptanceError(38));
    }
    Ok(user.to_owned())
}

#[derive(Default, Serialize)]
struct ConfigFieldProbe {
    present: bool,
    duplicate: bool,
    expected: bool,
}
#[derive(Default, Serialize)]
struct PeerConfigProbe {
    preparation_error: u16,
    trusted_ssh: bool,
    native_error: u8,
    exit_observed: bool,
    exit_code: u32,
    output_bytes: usize,
    utf8: bool,
    hostname: ConfigFieldProbe,
    strict_host_key: ConfigFieldProbe,
    batch_mode: ConfigFieldProbe,
    public_key_only: ConfigFieldProbe,
    password_disabled: ConfigFieldProbe,
    keyboard_interactive_disabled: ConfigFieldProbe,
    host_key_updates_disabled: ConfigFieldProbe,
    user_syntax: ConfigFieldProbe,
    qualifies: bool,
}

fn config_field_probe(
    text: &str,
    name: &str,
    expected: impl FnOnce(&str) -> bool,
) -> ConfigFieldProbe {
    // Match the primary qualifier's parser exactly; report no parsed value.
    let mut values = text.lines().filter_map(|line| {
        line.split_once(' ')
            .filter(|(key, _)| *key == name)
            .map(|(_, value)| value.trim())
    });
    let first = values.next();
    let duplicate = values.next().is_some();
    ConfigFieldProbe {
        present: first.is_some(),
        duplicate,
        expected: !duplicate && first.is_some_and(expected),
    }
}

fn native_probe_error(error: NativeProcessError) -> u8 {
    // Stable diagnostic codes; zero means no NativeProcessError was returned.
    match error {
        NativeProcessError::ExecutableUnavailable => 1,
        NativeProcessError::SpecificationInvalid => 2,
        NativeProcessError::StartFailed => 3,
        NativeProcessError::OwnershipUnavailable => 4,
        NativeProcessError::ResumeFailed => 5,
        NativeProcessError::StatusFailed => 6,
        NativeProcessError::StopTimeout => 7,
        NativeProcessError::OutputLimitExceeded => 8,
        NativeProcessError::OperationCancelled => 9,
        NativeProcessError::DeadlineExceeded => 10,
        NativeProcessError::PlatformUnsupported => 11,
    }
}

fn probe_peer_config(selection: &PeerSelection) -> PeerConfigProbe {
    let mut report = PeerConfigProbe::default();
    let (executable, environment) = match trusted_ssh() {
        Ok(value) => value,
        Err(error) => {
            report.preparation_error = error.0;
            return report;
        }
    };
    report.trusted_ssh = true;
    let mut args = ssh_options();
    args.insert(0, "-G".into());
    args.push(selection.alias.clone());
    let mut spec = SpawnSpec::new(&executable, &args);
    spec.environment = Some(&environment);
    spec.cwd = executable.parent();
    let output = match run_capture(
        &spec,
        &AtomicBool::new(false),
        Instant::now() + PEER_TIMEOUT,
        SSH_CONFIG_LIMIT,
    ) {
        Ok(value) => value,
        Err(error) => {
            report.native_error = native_probe_error(error);
            return report;
        }
    };
    report.exit_observed = true;
    report.exit_code = output.exit_code;
    let bytes = SecretBytes(output.stdout);
    report.output_bytes = bytes.0.len();
    if output.exit_code != 0 {
        return report;
    }
    let text = match std::str::from_utf8(&bytes.0) {
        Ok(value) => value,
        Err(_) => return report,
    };
    report.utf8 = true;
    report.hostname = config_field_probe(text, "hostname", |value| {
        value == selection.address.to_string()
    });
    report.strict_host_key = config_field_probe(text, "stricthostkeychecking", |value| {
        matches!(value, "yes" | "true")
    });
    report.batch_mode =
        config_field_probe(text, "batchmode", |value| matches!(value, "yes" | "true"));
    report.public_key_only = config_field_probe(text, "preferredauthentications", |value| {
        value == "publickey"
    });
    report.password_disabled = config_field_probe(text, "passwordauthentication", |value| {
        matches!(value, "no" | "false")
    });
    report.keyboard_interactive_disabled =
        config_field_probe(text, "kbdinteractiveauthentication", |value| {
            matches!(value, "no" | "false")
        });
    report.host_key_updates_disabled = config_field_probe(text, "updatehostkeys", |value| {
        matches!(value, "no" | "false")
    });
    report.user_syntax = config_field_probe(text, "user", |value| {
        !value.is_empty()
            && value.len() <= 128
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    });
    report.qualifies = [
        &report.hostname,
        &report.strict_host_key,
        &report.batch_mode,
        &report.public_key_only,
        &report.password_disabled,
        &report.keyboard_interactive_disabled,
        &report.host_key_updates_disabled,
        &report.user_syntax,
    ]
    .into_iter()
    .all(|field| field.present && !field.duplicate && field.expected);
    report
}

/// Isolated read-only diagnosis: no database, AAG key, Serve, or remote command.
#[test]
#[ignore = "read-only SSH configuration probe; requires the same complete live-acceptance opt-in tuple"]
fn peer_config_probe() {
    let alias = std::env::var("AAG_TAILNET_PEER_ALIAS").unwrap_or_default();
    let address = std::env::var("AAG_TAILNET_EXPECTED_PEER_IP").unwrap_or_default();
    if !exact_opt_in(
        &std::env::var("AAG_TAILNET_LIVE_ACCEPTANCE").unwrap_or_default(),
        &alias,
        &address,
        &std::env::var("AAG_TAILNET_LIVE_PORT").unwrap_or_default(),
    ) {
        panic!("peer_config_probe_failed code=1");
    }
    let selection = PeerSelection::parse(&alias, &address)
        .unwrap_or_else(|_| panic!("peer_config_probe_failed code=1"));
    let report = probe_peer_config(&selection);
    println!(
        "{}",
        serde_json::to_string(&report).unwrap_or_else(|_| "{\"serialization_error\":true}".into())
    );
    assert!(report.qualifies, "peer_config_probe_failed code=38");
}

#[test]
fn peer_config_probe_reports_structure_without_emitting_values() {
    let text = "hostname fixture_value_not_for_output\nhostname repeated_value\nuser fixture_user_not_for_output\nsetenv PRIVATE=fixture_secret_not_for_output\n";
    let duplicate = config_field_probe(text, "hostname", |_| true);
    assert!(duplicate.present && duplicate.duplicate && !duplicate.expected);
    let absent = config_field_probe(text, "batchmode", |_| true);
    assert!(!absent.present && !absent.duplicate && !absent.expected);
    let user = config_field_probe(text, "user", |_| true);
    assert!(user.present && !user.duplicate && user.expected);
    let encoded = serde_json::to_string(&user).unwrap();
    assert!(!encoded.contains("fixture_") && !encoded.contains("PRIVATE"));
}

fn peer_arguments(selection: &PeerSelection, user: &str) -> Vec<String> {
    let mut args = ssh_options();
    args.extend(["-o".into(), format!("HostName={}", selection.address)]);
    args.extend([
        "-l".into(),
        user.into(),
        selection.alias.clone(),
        "/usr/bin/python3".into(),
        "-I".into(),
        "-u".into(),
        "-c".into(),
        posix_quote(REMOTE_PYTHON),
    ]);
    args
}
fn posix_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

struct PeerProcess {
    child: NativeChild,
    input: Option<File>,
    input_remaining: usize,
    output: Vec<u8>,
    consumed: usize,
}
impl PeerProcess {
    fn start(selection: &PeerSelection) -> AcceptanceResult<Self> {
        let (executable, environment) = trusted_ssh()?;
        let user = qualify_peer(&executable, &environment, selection)?;
        let args = peer_arguments(selection, &user);
        let mut spec = SpawnSpec::new(&executable, &args);
        spec.pipe_stdin = true;
        spec.capture_stdout = true;
        spec.environment = Some(&environment);
        spec.cwd = executable.parent();
        let mut child = NativeChild::create_suspended(&spec).map_err(|_| AcceptanceError(31))?;
        let input = child.take_stdin().ok_or(AcceptanceError(31))?;
        let (mut flags, mut output_size, mut input_size) = (0, 0, 0);
        if unsafe {
            GetNamedPipeInfo(
                input.as_raw_handle(),
                &mut flags,
                &mut output_size,
                &mut input_size,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(AcceptanceError(32));
        }
        let capacity = if flags & 1 == 0 {
            input_size
        } else {
            output_size
        } as usize;
        if capacity == 0 || flags & 4 != 0 {
            return Err(AcceptanceError(32));
        }
        child.admit_and_resume().map_err(|_| AcceptanceError(31))?;
        Ok(Self {
            child,
            input: Some(input),
            input_remaining: capacity.min(INPUT_LIMIT),
            output: Vec::new(),
            consumed: 0,
        })
    }

    fn send<T: Serialize>(&mut self, value: &T) -> AcceptanceResult<()> {
        let mut bytes = SecretBytes(serde_json::to_vec(value).map_err(|_| AcceptanceError(33))?);
        bytes.0.push(b'\n');
        if bytes.0.len() > self.input_remaining {
            return Err(AcceptanceError(33));
        }
        // This is the sole writer to a fresh pipe. The cumulative bytes across
        // both frames never exceed its observed kernel buffer capacity, even
        // if the peer stops reading. No reader-dependent blocking write occurs.
        self.input_remaining -= bytes.0.len();
        self.input
            .as_mut()
            .ok_or(AcceptanceError(33))?
            .write_all(&bytes.0)
            .map_err(|_| AcceptanceError(33))
    }

    async fn receipt<T: DeserializeOwned>(&mut self, deadline: Instant) -> AcceptanceResult<T> {
        loop {
            self.child
                .read_available(&mut self.output, OUTPUT_LIMIT)
                .map_err(|_| AcceptanceError(34))?;
            if let Some(length) = self.output[self.consumed..]
                .iter()
                .position(|byte| *byte == b'\n')
            {
                if length > RECEIPT_LIMIT {
                    return Err(AcceptanceError(34));
                }
                let value =
                    serde_json::from_slice(&self.output[self.consumed..self.consumed + length])
                        .map_err(|_| AcceptanceError(34))?;
                self.consumed += length + 1;
                return Ok(value);
            }
            if self
                .child
                .try_exit()
                .map_err(|_| AcceptanceError(34))?
                .is_some()
            {
                return Err(AcceptanceError(34));
            }
            if Instant::now() >= deadline {
                return Err(AcceptanceError(35));
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
    async fn wait_success(&mut self, deadline: Instant) -> AcceptanceResult<()> {
        self.input.take();
        loop {
            self.child
                .read_available(&mut self.output, OUTPUT_LIMIT)
                .map_err(|_| AcceptanceError(36))?;
            if let Some(code) = self.child.try_exit().map_err(|_| AcceptanceError(36))? {
                return if code == 0
                    && self.output[self.consumed..]
                        .iter()
                        .all(u8::is_ascii_whitespace)
                {
                    Ok(())
                } else {
                    Err(AcceptanceError(36))
                };
            }
            if Instant::now() >= deadline {
                return Err(AcceptanceError(36));
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
    fn close(&mut self) -> AcceptanceResult<()> {
        self.input.take();
        self.child
            .terminate(Instant::now() + PEER_STOP_TIMEOUT)
            .map_err(|_| AcceptanceError(37))
    }
}

// Static code is the only remote command argument. Host, port, and credential
// arrive through SSH stdin; stdout contains only bounded numeric/boolean JSON.
const REMOTE_PYTHON: &str = r#"
import http.client, ipaddress, json, signal, socket, sys
signal.alarm(90)
key = ""
payload = {}
held = None
request_bytes = bytearray()
def emit(value):
    print(json.dumps(value, separators=(",", ":")), flush=True)
def line():
    value = sys.stdin.buffer.readline(4097)
    if not value.endswith(b"\n") or len(value) > 4096:
        raise ValueError()
    return json.loads(value)
def get_health(host, port, credential, keep=False):
    conn = http.client.HTTPConnection(host, port, timeout=5)
    headers = {} if credential is None else {"Authorization": "Bearer " + credential}
    try:
        conn.request("GET", "/health", headers=headers)
        response = conn.getresponse()
        status = int(response.status)
        body = response.read(524289)
        if len(body) > 524288:
            raise ValueError()
        body = None
        if keep and not response.will_close and conn.sock is not None:
            return status, conn
        conn.close()
        return status, None
    except BaseException:
        conn.close()
        raise
try:
    version = tuple(sys.version_info[:3])
    compatible = version[0] == 3 and version[1] >= 10
    emit({"phase":0,"ready":compatible,"major":version[0],"minor":version[1],"micro":version[2]})
    if not compatible:
        raise ValueError()
    payload = line()
    if set(payload) != {"host","port","key"} or payload["port"] != 8989:
        raise ValueError()
    host, port = payload["host"], payload["port"]
    address = ipaddress.ip_address(host)
    if not ((address.version == 4 and address in ipaddress.ip_network("100.64.0.0/10")) or (address.version == 6 and address in ipaddress.ip_network("fd7a:115c:a1e0::/48"))):
        raise ValueError()
    key = payload.pop("key")
    if not isinstance(key, str) or not 1 <= len(key) <= 1024 or not all(33 <= ord(char) <= 126 for char in key):
        raise ValueError()
    wrong = "aag_live_acceptance_wrong_key"
    if wrong == key:
        wrong += "_alternate"
    missing_status, _ = get_health(host, port, None)
    wrong_status, _ = get_health(host, port, wrong)
    correct_status, held = get_health(host, port, key, True)
    emit({"phase":1,"keyless":missing_status,"wrong":wrong_status,"correct":correct_status,"keepalive":held is not None})
    if [missing_status,wrong_status,correct_status] != [401,401,200] or held is None:
        raise ValueError()
    if line() != {"stop":True}:
        raise ValueError()
    existing_closed = False
    request_bytes.extend(b"GET /health HTTP/1.1\r\nHost: acceptance\r\nAuthorization: Bearer ")
    request_bytes.extend(key.encode("ascii"))
    request_bytes.extend(b"\r\nConnection: keep-alive\r\n\r\n")
    try:
        # Use the exact retained socket, never HTTPConnection's auto-reconnect.
        held.sock.sendall(request_bytes)
        existing_closed = held.sock.recv(1) == b""
    except (socket.timeout, TimeoutError):
        existing_closed = False
    except (OSError, EOFError):
        existing_closed = True
    held.close()
    held = None
    fresh_failed = False
    try:
        fresh = socket.create_connection((host, port), timeout=5)
        fresh.close()
    except OSError:
        fresh_failed = True
    emit({"phase":2,"existing_closed":existing_closed,"fresh_failed":fresh_failed})
    if not existing_closed or not fresh_failed:
        raise ValueError()
except BaseException:
    emit({"phase":9,"error":1})
    sys.exit(1)
finally:
    if held is not None:
        held.close()
    for index in range(len(request_bytes)):
        request_bytes[index] = 0
    key = ""
    payload.clear()
"#;

#[test]
fn live_acceptance_requires_the_complete_exact_opt_in_tuple() {
    let address = Ipv4Addr::new(100, 64, 0, 7).to_string();
    assert!(exact_opt_in(OPT_IN, "fixture_peer", &address, "8989"));
    for (marker, alias, port) in [
        ("", "fixture_peer", "8989"),
        (OPT_IN, "", "8989"),
        (OPT_IN, "fixture_peer", ""),
        (OPT_IN, "fixture_peer", "443"),
        (OPT_IN, "-option", "8989"),
        ("personal_peer_roundtrip ", "fixture_peer", "8989"),
    ] {
        assert!(!exact_opt_in(marker, alias, &address, port));
    }
    assert!(!exact_opt_in(
        OPT_IN,
        "fixture_peer",
        &Ipv4Addr::new(192, 0, 2, 1).to_string(),
        "8989"
    ));
}
