//! Metadata-only installation query. Only a kernel-attested A peer can supply
//! a candidate receipt; parsing it never proves protected executable identity.

use crate::config::private_fs::Managed_directory_identity;
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf, Prefix};

pub(super) const METHOD: &str = "aag/installation/read";
pub(super) const FRAME_LIMIT: usize = 16 * 1024;
pub(super) const ACCOUNT: &str = r"NT SERVICE\aag_session_authority";
pub(super) const SHIM_RELATIVE: &str =
    r"AI Agent Gateway\SessionAuthority\aag_session_authority.exe";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OwnerClientError {
    Unavailable,
    InvalidRequest,
    InvalidReceipt,
    InstallationMismatch,
    ServiceMismatch,
    ImagesUnqualified,
}
impl OwnerClientError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Unavailable => "session_client_authority_unavailable",
            Self::InvalidRequest => "session_client_authority_request_invalid",
            Self::InvalidReceipt => "session_client_authority_receipt_invalid",
            Self::InstallationMismatch => "session_client_authority_installation_mismatch",
            Self::ServiceMismatch => "session_client_authority_service_mismatch",
            Self::ImagesUnqualified => "session_client_authority_images_unqualified",
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ImagePin {
    pub path: String,
    pub object: Managed_directory_identity,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InstallationReceipt {
    pub schema_version: u32,
    pub status: String,
    pub challenge: String,
    pub root: String,
    pub protected_shim: ImagePin,
    pub pinned_native: ImagePin,
}

pub(super) struct InstallationPaths {
    pub root: PathBuf,
    pub protected_shim: PathBuf,
    pub pinned_native: PathBuf,
}

pub(super) struct ServiceObservation {
    pub service_type: u32,
    pub current_state: u32,
    pub process_id: u32,
    pub account: String,
    pub executable: PathBuf,
}

pub(super) fn request(root: &Path, challenge: &str) -> Result<Vec<u8>, OwnerClientError> {
    validate_root(root)?;
    validate_challenge(challenge)?;
    serde_json::to_vec(&serde_json::json!({"jsonrpc":"2.0","id":challenge,"method":METHOD,
        "params":{"schema_version":1,"challenge":challenge,"root":root.to_str().ok_or(OwnerClientError::InvalidRequest)?}}))
        .map_err(|_| OwnerClientError::InvalidRequest)
}

pub(super) fn receipt(
    bytes: &[u8],
    root: &Path,
    challenge: &str,
) -> Result<InstallationReceipt, OwnerClientError> {
    validate_root(root)?;
    validate_challenge(challenge)?;
    if bytes.len() > FRAME_LIMIT {
        return Err(OwnerClientError::InvalidReceipt);
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Response {
        jsonrpc: String,
        id: String,
        result: InstallationReceipt,
    }
    let response: Response =
        serde_json::from_slice(bytes).map_err(|_| OwnerClientError::InvalidReceipt)?;
    let result = response.result;
    if response.jsonrpc != "2.0"
        || response.id != challenge
        || result.challenge != challenge
        || result.schema_version != 1
        || result.status != "committed"
        || Some(result.root.as_str()) != root.to_str()
        || result.protected_shim.object.object == 0
        || result.pinned_native.object.object == 0
    {
        return Err(OwnerClientError::InvalidReceipt);
    }
    Ok(result)
}

pub(super) fn paths(
    receipt: &InstallationReceipt,
    program_files: &Path,
) -> Result<InstallationPaths, OwnerClientError> {
    let root = PathBuf::from(&receipt.root);
    let shim = PathBuf::from(&receipt.protected_shim.path);
    let native = PathBuf::from(&receipt.pinned_native.path);
    validate_image_layout(&root, &shim, &native, program_files)?;
    Ok(InstallationPaths {
        root,
        protected_shim: shim,
        pinned_native: native,
    })
}

pub(super) fn validate_image_layout(
    root: &Path,
    shim: &Path,
    native: &Path,
    program_files: &Path,
) -> Result<(), OwnerClientError> {
    validate_disk_path(program_files)?;
    validate_root(root)?;
    validate_disk_path(shim)?;
    validate_disk_path(native)?;
    if shim != program_files.join(SHIM_RELATIVE) {
        return Err(OwnerClientError::InstallationMismatch);
    }
    let relative = native
        .strip_prefix(program_files.join("WindowsApps"))
        .map_err(|_| OwnerClientError::InstallationMismatch)?;
    let parts: Vec<_> = relative.components().collect();
    if parts.len() != 4
        || parts[1].as_os_str() != "app"
        || parts[2].as_os_str() != "resources"
        || parts[3].as_os_str() != "codex.exe"
    {
        return Err(OwnerClientError::InstallationMismatch);
    }
    let package = parts[0]
        .as_os_str()
        .to_str()
        .ok_or(OwnerClientError::InstallationMismatch)?;
    let version = package
        .strip_prefix("OpenAI.Codex_")
        .and_then(|value| value.strip_suffix("_x64__2p2nqsd0c76g0"))
        .ok_or(OwnerClientError::InstallationMismatch)?;
    let version_parts: Vec<_> = version.split('.').collect();
    if version_parts.len() != 4
        || version_parts.iter().any(|part| {
            part.parse::<u16>()
                .ok()
                .is_none_or(|value| value.to_string() != *part)
        })
    {
        return Err(OwnerClientError::InstallationMismatch);
    }
    Ok(())
}

pub(super) fn service(
    observed: &ServiceObservation,
    pipe_process_id: u32,
    expected_executable: &Path,
) -> Result<(), OwnerClientError> {
    if observed.service_type != 0x10
        || observed.current_state != 4
        || observed.process_id == 0
        || observed.process_id != pipe_process_id
        || !observed.account.eq_ignore_ascii_case(ACCOUNT)
        || observed.executable != expected_executable
    {
        return Err(OwnerClientError::ServiceMismatch);
    }
    Ok(())
}

pub(super) fn validate_root(root: &Path) -> Result<(), OwnerClientError> {
    validate_disk_path(root)?;
    if root.file_name().is_none_or(|name| name != ".codex-cli") {
        return Err(OwnerClientError::InvalidRequest);
    }
    Ok(())
}
fn validate_challenge(challenge: &str) -> Result<(), OwnerClientError> {
    if challenge.len() != 64
        || !challenge
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(OwnerClientError::InvalidRequest);
    }
    Ok(())
}
pub(super) fn validate_disk_path(path: &Path) -> Result<(), OwnerClientError> {
    let raw = path.to_str().ok_or(OwnerClientError::InvalidRequest)?;
    if raw.contains('/')
        || raw.chars().any(char::is_control)
        || raw.split('\\').any(|part| part == "." || part == "..")
    {
        return Err(OwnerClientError::InvalidRequest);
    }
    let mut parts = path.components();
    if !matches!(parts.next(), Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(drive) if !drive.eq_ignore_ascii_case(&b'M')))
        || parts.next() != Some(Component::RootDir)
    {
        return Err(OwnerClientError::InvalidRequest);
    }
    let mut count = 0;
    for part in parts {
        let Component::Normal(name) = part else {
            return Err(OwnerClientError::InvalidRequest);
        };
        let name = name.to_str().ok_or(OwnerClientError::InvalidRequest)?;
        if name.ends_with(['.', ' ']) || name.chars().any(|value| "<>:\"/\\|?*".contains(value)) {
            return Err(OwnerClientError::InvalidRequest);
        }
        let base = name
            .split('.')
            .next()
            .unwrap_or("")
            .trim_end_matches(' ')
            .to_uppercase();
        if matches!(
            base.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" | "CLOCK$"
        ) || ["COM", "LPT"].iter().any(|prefix| {
            base.strip_prefix(*prefix).is_some_and(|suffix| {
                matches!(
                    suffix,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            })
        }) {
            return Err(OwnerClientError::InvalidRequest);
        }
        count += 1;
    }
    if count == 0 {
        return Err(OwnerClientError::InvalidRequest);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    const CHALLENGE: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    fn root() -> PathBuf {
        std::env::temp_dir()
            .join("aag_session_owner_client_fixture")
            .join(".codex-cli")
    }
    fn program_files() -> PathBuf {
        std::env::var_os("ProgramFiles")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("Program Files"))
    }
    fn shim() -> PathBuf {
        program_files().join(r"AI Agent Gateway\SessionAuthority\aag_session_authority.exe")
    }
    fn response() -> Value {
        json!({"jsonrpc":"2.0", "id":CHALLENGE, "result": {
            "schema_version":1,"status":"committed","challenge":CHALLENGE,
            "root":root().to_str().unwrap(),
            "protected_shim":{"path":shim().to_str().unwrap(),"object":{"volume":7,"object":11}},
            "pinned_native":{"path":program_files().join(r"WindowsApps\OpenAI.Codex_1.2.3.4_x64__2p2nqsd0c76g0\app\resources\codex.exe").to_str().unwrap(),
                "object":{"volume":7,"object":12}}
        }})
    }
    fn parse(value: &Value) -> Result<InstallationReceipt, OwnerClientError> {
        receipt(&serde_json::to_vec(value).unwrap(), &root(), CHALLENGE)
    }

    #[test]
    fn committed_response_is_strictly_bound_to_challenge_schema_and_root() {
        parse(&response()).unwrap();
        for (field, value) in [
            ("schema_version", json!(2)),
            ("status", json!("prepared")),
            ("challenge", json!("different")),
            (
                "root",
                json!(root().with_file_name("other").join(".codex-cli")),
            ),
            ("gateway_key", json!("not-authority")),
        ] {
            let mut bad = response();
            bad["result"][field] = value;
            assert!(parse(&bad).is_err());
        }
        let mut wrong_id = response();
        wrong_id["id"] = json!("stale");
        assert!(parse(&wrong_id).is_err());
        let mut rpc_error = response();
        rpc_error["error"] = json!({"code":-1});
        assert!(parse(&rpc_error).is_err());
        let duplicate = serde_json::to_string(&response()).unwrap().replace(
            "\"status\":\"committed\"",
            "\"status\":\"committed\",\"status\":\"committed\"",
        );
        assert!(receipt(duplicate.as_bytes(), &root(), CHALLENGE).is_err());
    }

    #[test]
    fn image_layout_accepts_only_fixed_shim_and_exact_vendor_package_shape() {
        let valid = parse(&response()).unwrap();
        let checked = paths(&valid, &program_files()).unwrap();
        assert_eq!(checked.protected_shim, shim());
        for native in [
            root().with_file_name("codex.exe"),
            program_files().join(r"WindowsApps\OpenAI.CodexPrimaryRuntime_1.2.3.4_x64__2p2nqsd0c76g0\app\resources\codex.exe"),
            program_files().join(r"WindowsApps\OpenAI.Codex_1.2.3.4_x64__other\app\resources\codex.exe"),
            program_files().join(r"WindowsApps\OpenAI.Codex_1.2.3.4_arm64__2p2nqsd0c76g0\app\resources\codex.exe"),
            program_files().join(r"WindowsApps\OpenAI.Codex_1.2.3.4_x64__2p2nqsd0c76g0\app\resources\..\codex.exe"),
            PathBuf::from(r"\\fixture_server\fixture_share\codex.exe"),
            PathBuf::from(r"M:\fixture\codex.exe"),
        ] {
            let mut bad = valid.clone();
            bad.pinned_native.path = native.to_str().unwrap().into();
            assert!(paths(&bad, &program_files()).is_err());
        }
        let mut wrong_shim = valid.clone();
        wrong_shim.protected_shim.path = root()
            .with_file_name("aag_session_authority.exe")
            .to_str()
            .unwrap()
            .into();
        assert!(paths(&wrong_shim, &program_files()).is_err());
    }

    #[test]
    fn scm_identity_must_match_attested_peer_pid_normal_account_and_fixed_image() {
        let valid = ServiceObservation {
            service_type: 0x10,
            current_state: 4,
            process_id: 42,
            account: r"NT SERVICE\aag_session_authority".into(),
            executable: shim(),
        };
        service(&valid, 42, &shim()).unwrap();
        assert!(service(&valid, 43, &shim()).is_err());
        for (kind, state, pid, account) in [
            (0x20, 4, 42, r"NT SERVICE\aag_session_authority"),
            (0x10, 2, 42, r"NT SERVICE\aag_session_authority"),
            (0x10, 4, 0, r"NT SERVICE\aag_session_authority"),
            (0x10, 4, 42, "LocalSystem"),
            (0x10, 4, 42, r"NT SERVICE\aag_session_reader"),
        ] {
            let bad = ServiceObservation {
                service_type: kind,
                current_state: state,
                process_id: pid,
                account: account.into(),
                executable: shim(),
            };
            assert!(service(&bad, 42, &shim()).is_err());
        }
        assert!(service(
            &valid,
            42,
            &root().with_file_name("aag_session_authority.exe")
        )
        .is_err());
    }

    #[test]
    fn query_contains_only_metadata_and_rejects_invalid_challenge_shape() {
        let bytes = request(&root(), CHALLENGE).unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            value,
            json!({"jsonrpc":"2.0","id":CHALLENGE,"method":METHOD,
            "params":{"schema_version":1,"challenge":CHALLENGE,"root":root().to_str().unwrap()}})
        );
        for challenge in ["", "gateway-secret", "ABCDEF", "a\nb"] {
            assert!(request(&root(), challenge).is_err());
        }
    }
}
