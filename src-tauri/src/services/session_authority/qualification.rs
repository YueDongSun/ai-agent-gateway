use crate::{
    config::private_fs,
    services::{
        owned_process::{encode_environment, run_capture, SpawnSpec},
        session_broker::{
            native_core::{
                NativeBinding, NativeCore, NativeCoreLimits, NativeCoreSpec, NativeShutdown,
                NativeWait, RawJson,
            },
            service_host::{
                dispatch, FatalServiceExit, ServiceContext, ServiceHostConfig, ServiceHostError,
                ServiceRole,
            },
            windows_isolation::{CoreRole, NormalServiceRole, Sid, VerifiedNormalServicePrincipal},
            CanonicalSessionId,
        },
    },
};
use serde::Serialize;
use std::{
    ffi::OsString,
    path::{Component, Path, PathBuf},
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

const CAPTURE_LIMIT: usize = 64 * 1024;
const NATIVE_HELP_TIMEOUT: Duration = Duration::from_secs(10);
const QUALIFICATION_DIRECTORY: &str = "AI Agent Gateway/SessionAuthority/qualification";

struct Arguments {
    role: ServiceRole,
    directory: PathBuf,
    expected_sid: Sid,
    storage_sid: Sid,
    native_executable: Option<PathBuf>,
    canonical_id: Option<CanonicalSessionId>,
}

#[derive(Serialize)]
struct QualificationReceipt {
    schema_version: u8,
    service_name: String,
    service_user_sid: String,
    role: String,
    source_key: Option<String>,
    physical_generation: Option<u64>,
    normal_principal_verified: bool,
    policy_process_protected: bool,
    protected_policy_threads: Option<usize>,
    native_cli_version: Option<String>,
    native_app_server_initialized: bool,
    conversation_mutations_issued: u64,
    result: &'static str,
}

pub(super) fn run(arguments: Vec<OsString>) -> i32 {
    let parsed = match parse(&arguments) {
        Ok(value) => value,
        Err(code) => {
            eprintln!("{code}");
            return 64;
        }
    };
    let role = parsed.role;
    match dispatch(ServiceHostConfig { role, fatal_exit }, business) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}

fn fatal_exit(fatal: FatalServiceExit) -> ! {
    std::process::exit(fatal.exit_code())
}

fn parse(arguments: &[OsString]) -> Result<Arguments, &'static str> {
    let mut values = std::collections::HashMap::<String, OsString>::new();
    let mut iterator = arguments.iter().skip(1);
    if iterator.next().and_then(|value| value.to_str()) != Some("qualify-service") {
        return Err("session_authority_action_unsupported");
    }
    while let Some(key) = iterator.next() {
        let name = key.to_str().ok_or("session_authority_argument_invalid")?;
        if ![
            "--role",
            "--source-key",
            "--generation",
            "--directory",
            "--expected-sid",
            "--storage-sid",
            "--native-executable",
            "--canonical-id",
        ]
        .contains(&name)
        {
            return Err("session_authority_argument_unknown");
        }
        let value = iterator
            .next()
            .ok_or("session_authority_argument_missing")?;
        if values.insert(name.to_owned(), value.clone()).is_some() {
            return Err("session_authority_argument_duplicate");
        }
    }
    let text = |name: &str| {
        values
            .get(name)
            .and_then(|value| value.to_str())
            .ok_or("session_authority_argument_missing")
    };
    let role = match text("--role")? {
        "policy" => ServiceRole::Policy,
        "reader" => ServiceRole::Reader,
        "runtime" => ServiceRole::runtime(
            text("--source-key")?,
            text("--generation")?
                .parse()
                .map_err(|_| "session_authority_generation_invalid")?,
        )
        .map_err(|_| "session_authority_role_invalid")?,
        _ => return Err("session_authority_role_invalid"),
    };
    if !matches!(role, ServiceRole::Runtime { .. })
        && (values.contains_key("--source-key") || values.contains_key("--generation"))
    {
        return Err("session_authority_role_argument_conflict");
    }
    let directory = PathBuf::from(
        values
            .get("--directory")
            .ok_or("session_authority_directory_missing")?,
    );
    validate_directory(&directory)?;
    let native_executable = values.get("--native-executable").map(PathBuf::from);
    if let Some(executable) = &native_executable {
        if !plain(executable)
            || executable
                .file_name()
                .is_none_or(|name| !name.to_string_lossy().eq_ignore_ascii_case("codex.exe"))
        {
            return Err("session_authority_native_executable_invalid");
        }
        let protected_roots = ["ProgramW6432", "ProgramFiles"]
            .iter()
            .filter_map(|name| std::env::var_os(name))
            .map(PathBuf::from)
            .collect::<Vec<_>>();
        if !protected_roots
            .iter()
            .any(|root| executable.starts_with(root))
        {
            return Err("session_authority_native_executable_unprotected");
        }
    }
    if role == ServiceRole::Policy && native_executable.is_some() {
        return Err("session_authority_policy_must_not_execute_native");
    }
    let expected_sid =
        Sid::parse(text("--expected-sid")?).map_err(|_| "session_authority_sid_invalid")?;
    let storage_sid =
        Sid::parse(text("--storage-sid")?).map_err(|_| "session_authority_sid_invalid")?;
    if (role == ServiceRole::Policy) != (expected_sid == storage_sid) {
        return Err("session_authority_principals_not_distinct");
    }
    let canonical_id = values
        .get("--canonical-id")
        .map(|value| {
            CanonicalSessionId::from_protected_catalog(
                value
                    .to_str()
                    .ok_or("session_authority_canonical_id_invalid")?,
            )
            .map_err(|_| "session_authority_canonical_id_invalid")
        })
        .transpose()?;
    if matches!(role, ServiceRole::Runtime { .. }) && canonical_id.is_none() {
        return Err("session_authority_canonical_id_missing");
    }
    Ok(Arguments {
        role,
        directory,
        expected_sid,
        storage_sid,
        native_executable,
        canonical_id,
    })
}

fn plain(path: &Path) -> bool {
    path.is_absolute()
        && !path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
        && !path.to_string_lossy().chars().any(char::is_control)
}

fn validate_directory(path: &Path) -> Result<(), &'static str> {
    if !plain(path) {
        return Err("session_authority_directory_invalid");
    }
    let program_data = std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .ok_or("session_authority_program_data_unavailable")?;
    let root = program_data.join(QUALIFICATION_DIRECTORY);
    if !plain(&root) || !path.starts_with(&root) || path == root {
        return Err("session_authority_qualification_scope_invalid");
    }
    Ok(())
}

fn business(context: ServiceContext) -> Result<(), ServiceHostError> {
    let arguments = parse(&std::env::args_os().collect::<Vec<_>>())
        .map_err(|_| ServiceHostError::BusinessFailed)?;
    if arguments.role != context.role() {
        return Err(ServiceHostError::RoleInvalid);
    }
    let actual_sid = context.verified_user_sid()?;
    if actual_sid != arguments.expected_sid.value() {
        return Err(ServiceHostError::IdentityMismatch);
    }
    let policy_protection = if arguments.role == ServiceRole::Policy {
        Some(crate::services::session_broker::service_host::protected_process::protect_policy_process(&context)
            .map_err(|_| ServiceHostError::BusinessFailed)?)
    } else {
        None
    };
    // The operator creates this exact fresh directory with its service user as
    // owner before SCM startup. This boundary never adopts a foreign owner.
    private_fs::prepare_managed_private_dir(&arguments.directory)
        .map_err(|_| ServiceHostError::BusinessFailed)?;
    let mut receipt = QualificationReceipt {
        schema_version: 1,
        service_name: arguments.role.service_name(),
        service_user_sid: actual_sid,
        role: match arguments.role {
            ServiceRole::Policy => "policy",
            ServiceRole::Reader => "reader",
            ServiceRole::Runtime { .. } => "runtime",
        }
        .into(),
        source_key: match arguments.role {
            ServiceRole::Runtime { source_key, .. } => Some(source_key.as_str().to_owned()),
            _ => None,
        },
        physical_generation: match arguments.role {
            ServiceRole::Runtime { generation, .. } => Some(generation.get()),
            _ => None,
        },
        normal_principal_verified: true,
        policy_process_protected: policy_protection.is_some(),
        protected_policy_threads: policy_protection
            .as_ref()
            .map(|proof| proof.protected_thread_count()),
        native_cli_version: None,
        native_app_server_initialized: false,
        conversation_mutations_issued: 0,
        result: "identity_verified_no_conversation_admitted",
    };
    let mut native_core = None;
    match arguments.role {
        ServiceRole::Policy => {}
        ServiceRole::Reader | ServiceRole::Runtime { .. } => {
            let role = match arguments.role {
                ServiceRole::Reader => NormalServiceRole::TrustedReader,
                ServiceRole::Runtime { generation, .. } => NormalServiceRole::RuntimeGeneration {
                    source_id: arguments
                        .canonical_id
                        .clone()
                        .ok_or(ServiceHostError::BusinessFailed)?,
                    physical_generation: generation.get(),
                },
                _ => return Err(ServiceHostError::RoleInvalid),
            };
            let principal = VerifiedNormalServicePrincipal::capture_current(
                &arguments.expected_sid,
                &arguments.storage_sid,
                role,
            )
            .map_err(|_| ServiceHostError::IdentityUnavailable)?;
            receipt.normal_principal_verified = true;
            if let Some(executable) = &arguments.native_executable {
                receipt.native_cli_version = Some(native_version(
                    executable,
                    &arguments.directory,
                    &principal,
                )?);
                native_core = Some(initialize_native(executable, &arguments, principal)?);
                receipt.native_app_server_initialized = true;
            }
        }
    }
    let bytes =
        serde_json::to_vec_pretty(&receipt).map_err(|_| ServiceHostError::BusinessFailed)?;
    private_fs::atomic_write(&arguments.directory.join("qualification.json"), &bytes)
        .map_err(|_| ServiceHostError::BusinessFailed)?;
    context.ready()?;
    while !context.is_cancelled() {
        if let Some(core) = &mut native_core {
            core.poll().map_err(|_| ServiceHostError::BusinessFailed)?;
        }
        context.wait_cancelled(Duration::from_millis(100));
    }
    context.request_stop();
    if let Some(core) = &mut native_core {
        if !matches!(
            core.shutdown(Instant::now() + Duration::from_secs(5)),
            NativeShutdown::Stopped { .. }
        ) {
            return Err(ServiceHostError::CleanupTimeout);
        }
    }
    Ok(())
}

fn initialize_native(
    executable: &Path,
    arguments: &Arguments,
    principal: VerifiedNormalServicePrincipal,
) -> Result<NativeCore, ServiceHostError> {
    let home = arguments.directory.join("native_metadata");
    let index = arguments.directory.join("native_index");
    let workspace = arguments.directory.join("neutral_workspace");
    let temporary = arguments.directory.join("temp");
    for directory in [&home, &index, &workspace, &temporary] {
        private_fs::prepare_managed_private_dir(directory)
            .map_err(|_| ServiceHostError::BusinessFailed)?;
    }
    let windows = std::env::var_os("SystemRoot").ok_or(ServiceHostError::BusinessFailed)?;
    let is_reader = arguments.role == ServiceRole::Reader;
    let generation = match arguments.role {
        ServiceRole::Runtime { generation, .. } => generation.get(),
        _ => 1,
    };
    let canonical = arguments.canonical_id.clone().unwrap_or(
        CanonicalSessionId::from_protected_catalog("qualification_reader")
            .map_err(|_| ServiceHostError::BusinessFailed)?,
    );
    let spec = NativeCoreSpec {
        binding: NativeBinding {
            canonical_session_id: canonical,
            physical_generation: generation,
            role: if is_reader {
                CoreRole::ReadOnly
            } else {
                CoreRole::ReadWrite
            },
        },
        executable: executable.to_path_buf(),
        codex_home: home,
        sqlite_home: index,
        cwd: workspace,
        environment: vec![
            ("SystemRoot".into(), windows.clone()),
            ("WINDIR".into(), windows),
            ("TEMP".into(), temporary.as_os_str().to_owned()),
            ("TMP".into(), temporary.as_os_str().to_owned()),
        ],
        // This empty qualification home admits no plugin, MCP, hook, or model
        // work. The runtime's real source settings are a separate contract.
        config_overrides: vec![
            "features.plugins=false".into(),
            "features.hooks=false".into(),
            "features.remote_plugin=false".into(),
            "features.code_mode_host=false".into(),
            "features.skill_mcp_dependency_install=false".into(),
            "history.persistence=\"none\"".into(),
        ],
        limits: NativeCoreLimits::default(),
    };
    let mut core = NativeCore::spawn_for_service(spec, principal)
        .map_err(|_| ServiceHostError::BusinessFailed)?;
    let params=RawJson::parse("{\"clientInfo\":{\"name\":\"aag_session_qualification\",\"version\":\"0.2.0\"},\"capabilities\":{\"experimentalApi\":true}}").map_err(|_|ServiceHostError::BusinessFailed)?;
    let ticket = core
        .request("initialize", Some(&params))
        .map_err(|_| ServiceHostError::BusinessFailed)?;
    match core
        .wait_response(&ticket, Instant::now() + NATIVE_HELP_TIMEOUT)
        .map_err(|_| ServiceHostError::BusinessFailed)?
    {
        NativeWait::Response(response) => {
            let envelope: serde_json::Value = serde_json::from_str(response.frame.raw_json())
                .map_err(|_| ServiceHostError::BusinessFailed)?;
            if envelope.get("result").is_none() || envelope.get("error").is_some() {
                let _ = core.shutdown(Instant::now() + Duration::from_secs(5));
                return Err(ServiceHostError::BusinessFailed);
            }
        }
        NativeWait::OutcomeUnknown { .. } => {
            let _ = core.shutdown(Instant::now() + Duration::from_secs(5));
            return Err(ServiceHostError::BusinessFailed);
        }
    }
    core.notify_initialized()
        .map_err(|_| ServiceHostError::BusinessFailed)?;
    core.poll().map_err(|_| ServiceHostError::BusinessFailed)?;
    Ok(core)
}

fn native_version(
    executable: &Path,
    directory: &Path,
    principal: &VerifiedNormalServicePrincipal,
) -> Result<String, ServiceHostError> {
    private_fs::validate_external_file_path(executable)
        .map_err(|_| ServiceHostError::BusinessFailed)?;
    let temporary = directory.join("temp");
    private_fs::prepare_managed_private_dir(&temporary)
        .map_err(|_| ServiceHostError::BusinessFailed)?;
    let windows = std::env::var_os("SystemRoot").ok_or(ServiceHostError::BusinessFailed)?;
    let environment = encode_environment(&[
        ("SystemRoot".into(), windows.clone()),
        ("WINDIR".into(), windows),
        ("CODEX_HOME".into(), directory.as_os_str().to_owned()),
        ("TEMP".into(), temporary.as_os_str().to_owned()),
        ("TMP".into(), temporary.as_os_str().to_owned()),
    ])
    .map_err(|_| ServiceHostError::BusinessFailed)?;
    let arguments = vec!["--version".into()];
    let mut spec = SpawnSpec::new(executable, &arguments);
    spec.environment = Some(&environment);
    spec.cwd = Some(directory);
    // None inherits only this newly verified normal B/C process; it is never
    // used in PolicyA or as a fallback from a failed identity proof.
    principal
        .verify_current()
        .map_err(|_| ServiceHostError::IdentityUnavailable)?;
    let output = run_capture(
        &spec,
        &AtomicBool::new(false),
        Instant::now() + NATIVE_HELP_TIMEOUT,
        CAPTURE_LIMIT,
    )
    .map_err(|_| ServiceHostError::BusinessFailed)?;
    if output.exit_code != 0 {
        return Err(ServiceHostError::BusinessFailed);
    }
    let version = String::from_utf8(output.stdout).map_err(|_| ServiceHostError::BusinessFailed)?;
    let version = version.trim();
    if version.len() > 128
        || !version.starts_with("codex-cli ")
        || !version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b" ._+-".contains(&byte))
    {
        return Err(ServiceHostError::BusinessFailed);
    }
    Ok(version.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(role: &str) -> Vec<OsString> {
        vec![
            "aag_session_authority".into(),
            "qualify-service".into(),
            "--role".into(),
            role.into(),
            "--directory".into(),
            std::env::var_os("ProgramData")
                .map(PathBuf::from)
                .unwrap()
                .join(QUALIFICATION_DIRECTORY)
                .join("fixture")
                .into_os_string(),
            "--expected-sid".into(),
            "S-1-5-80-1-2-3-4-5".into(),
            "--storage-sid".into(),
            "S-1-5-80-6-7-8-9-10".into(),
        ]
    }

    #[test]
    fn qualification_cannot_be_redirected_into_a_live_home_or_native_policy_execution() {
        let mut values = arguments("reader");
        let slot = values
            .iter()
            .position(|value| value == "--directory")
            .unwrap()
            + 1;
        values[slot] = r"C:\Users\fixture\.codex".into();
        assert!(matches!(
            parse(&values),
            Err("session_authority_qualification_scope_invalid")
        ));
        let mut values = arguments("policy");
        values.extend([
            "--native-executable".into(),
            r"C:\Program Files\fixture\codex.exe".into(),
        ]);
        assert!(matches!(
            parse(&values),
            Err("session_authority_policy_must_not_execute_native")
        ));
    }

    #[test]
    fn runtime_requires_an_owner_bound_source_and_unambiguous_arguments() {
        let mut values = arguments("runtime");
        values.extend([
            "--source-key".into(),
            "0123456789abcdef0123456789abcdef".into(),
            "--generation".into(),
            "1".into(),
        ]);
        assert!(matches!(
            parse(&values),
            Err("session_authority_canonical_id_missing")
        ));
        values.extend(["--canonical-id".into(), "qualification_source".into()]);
        assert!(parse(&values).is_ok());
        values.extend(["--role".into(), "reader".into()]);
        assert!(matches!(
            parse(&values),
            Err("session_authority_argument_duplicate")
        ));
    }
}
