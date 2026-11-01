//! Process-local read facade. Only Windows exposes the restricted pipe.
#![allow(non_camel_case_types)]
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const FRAME_LIMIT: usize = 64 * 1024;
const PAGE_DEFAULT: usize = 8;
const PAGE_MAXIMUM: usize = 16;
const PAGE_RESPONSE_LIMIT: usize = FRAME_LIMIT - 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    schema_version: u8,
    instance_id: String,
    request_id: String,
    method: String,
    params: Request_params,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request_params {
    #[serde(default)]
    task: Option<String>,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Serialize)]
struct Discovery_descriptor<'a> {
    schema_version: u8,
    instance_id: &'a str,
    pipe_name: &'a str,
    process_id: u32,
}

fn parse_request(frame: &[u8], instance_id: &str) -> Result<Request, &'static str> {
    if frame.len() > FRAME_LIMIT || !frame.ends_with(b"\n") {
        return Err("malformed_frame");
    }
    let request: Request =
        serde_json::from_slice(&frame[..frame.len() - 1]).map_err(|_| "malformed_request")?;
    if request.schema_version != 1 {
        return Err("unsupported_version");
    }
    if request.instance_id != instance_id {
        return Err("stale_instance");
    }
    if request.request_id.is_empty()
        || request.request_id.len() > 64
        || !request.request_id.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '-'
        })
    {
        return Err("invalid_request_id");
    }
    match request.method.as_str() {
        "list_capabilities" | "get_health"
            if request.params.task.is_none()
                && request
                    .params
                    .limit
                    .is_none_or(|limit| (1..=PAGE_MAXIMUM).contains(&limit))
                && request.params.cursor.as_deref().is_none_or(|cursor| {
                    !cursor.is_empty()
                        && cursor.len() <= 3
                        && cursor.bytes().all(|byte| byte.is_ascii_digit())
                }) =>
        {
            Ok(request)
        }
        "route_preview"
            if request
                .params
                .task
                .as_deref()
                .is_some_and(|task| !task.trim().is_empty() && task.len() <= 8 * 1024) =>
        {
            if request.params.cursor.is_none() && request.params.limit.is_none() {
                Ok(request)
            } else {
                Err("invalid_params")
            }
        }
        "list_capabilities" | "get_health" | "route_preview" => Err("invalid_params"),
        _ => Err("method_not_allowed"),
    }
}

fn error_response(instance_id: &str, request_id: &str, code: &str) -> Value {
    let code = code.strip_prefix("workflow:").unwrap_or(code);
    let code = if !code.is_empty()
        && code.len() <= 64
        && code
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
    {
        code
    } else {
        "owner_unavailable"
    };
    json!({"schema_version":1,"instance_id":instance_id,"request_id":request_id,
        "ok":false,"error":{"code":code,"message":"Workflow read request could not be completed."}})
}

fn capability_page(
    mut list: super::types::Capability_list,
    cursor: Option<&str>,
    limit: Option<usize>,
) -> Result<Value, String> {
    let offset = cursor
        .unwrap_or("0")
        .parse::<usize>()
        .map_err(|_| "invalid_cursor".to_string())?;
    if offset > list.capabilities.len() {
        return Err("invalid_cursor".into());
    }
    let total = list.capabilities.len();
    let mut end = total.min(offset.saturating_add(limit.unwrap_or(PAGE_DEFAULT).min(PAGE_MAXIMUM)));
    let values = std::mem::take(&mut list.capabilities);
    loop {
        list.capabilities = values[offset..end].to_vec();
        list.next_cursor = (end < total).then(|| end.to_string());
        let value = serde_json::to_value(&list).map_err(|_| "response_invalid".to_string())?;
        let size = serde_json::to_vec(&value)
            .map_err(|_| "response_invalid".to_string())?
            .len();
        if size <= PAGE_RESPONSE_LIMIT {
            return Ok(value);
        }
        if end <= offset.saturating_add(1) {
            return Err("response_too_large".into());
        }
        end -= 1;
    }
}

fn health_page(
    mut snapshot: super::types::Health_snapshot,
    cursor: Option<&str>,
    limit: Option<usize>,
) -> Result<Value, String> {
    let offset = cursor
        .unwrap_or("0")
        .parse::<usize>()
        .map_err(|_| "invalid_cursor".to_string())?;
    if offset > snapshot.observations.len() {
        return Err("invalid_cursor".into());
    }
    let total = snapshot.observations.len();
    let mut end = total.min(offset.saturating_add(limit.unwrap_or(PAGE_DEFAULT).min(PAGE_MAXIMUM)));
    let values = std::mem::take(&mut snapshot.observations);
    loop {
        snapshot.observations = values[offset..end].to_vec();
        snapshot.next_cursor = (end < total).then(|| end.to_string());
        let value = serde_json::to_value(&snapshot).map_err(|_| "response_invalid".to_string())?;
        if serde_json::to_vec(&value)
            .map_err(|_| "response_invalid".to_string())?
            .len()
            <= PAGE_RESPONSE_LIMIT
        {
            return Ok(value);
        }
        if end <= offset.saturating_add(1) {
            return Err("response_too_large".into());
        }
        end -= 1;
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use crate::{
        config::private_fs,
        database::Database,
        services::{mcp::Mcp_service, workflow_registry::Workflow_service},
    };
    use std::{
        ffi::c_void,
        fs,
        path::{Path, PathBuf},
        ptr,
        sync::{Arc, Mutex},
        time::Duration,
    };
    use tauri::Manager;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::windows::named_pipe::ServerOptions,
        sync::{oneshot, OwnedSemaphorePermit, Semaphore},
        task::JoinSet,
        time::timeout,
    };

    const CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);
    const MAX_RESPONSE_BYTES: usize = 64 * 1024;
    const DISPATCH_TIMEOUT: Duration = Duration::from_secs(10);
    const MAX_ACTIVE_CLIENTS: usize = 4;

    #[repr(C)]
    struct Security_attributes {
        length: u32,
        security_descriptor: *mut c_void,
        inherit_handle: i32,
    }

    #[link(name = "advapi32")]
    extern "system" {
        fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
            source: *const u16,
            revision: u32,
            descriptor: *mut *mut c_void,
            size: *mut u32,
        ) -> i32;
        fn OpenProcessToken(process: *mut c_void, access: u32, token: *mut *mut c_void) -> i32;
        fn GetTokenInformation(
            token: *mut c_void,
            class: u32,
            information: *mut c_void,
            length: u32,
            returned: *mut u32,
        ) -> i32;
        fn ConvertSidToStringSidW(sid: *mut c_void, text: *mut *mut u16) -> i32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
        fn GetCurrentProcess() -> *mut c_void;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    fn current_user_sid() -> Result<String, String> {
        let mut token = ptr::null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), 0x0008, &mut token) } == 0 {
            return Err("workflow:pipe_identity_unavailable".into());
        }
        let result = (|| {
            let mut length = 0u32;
            unsafe {
                GetTokenInformation(token, 1, ptr::null_mut(), 0, &mut length);
            }
            if length == 0 || length > 4096 {
                return Err("workflow:pipe_identity_unavailable".into());
            }
            let mut storage =
                vec![0usize; (length as usize).div_ceil(std::mem::size_of::<usize>())];
            if unsafe {
                GetTokenInformation(
                    token,
                    1,
                    storage.as_mut_ptr() as *mut c_void,
                    length,
                    &mut length,
                )
            } == 0
            {
                return Err("workflow:pipe_identity_unavailable".into());
            }
            let sid = unsafe { *(storage.as_ptr() as *const *mut c_void) };
            if sid.is_null() {
                return Err("workflow:pipe_identity_unavailable".into());
            }
            let mut text = ptr::null_mut();
            if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 || text.is_null() {
                return Err("workflow:pipe_identity_unavailable".into());
            }
            let mut count = 0;
            while count < 256 && unsafe { *text.add(count) } != 0 {
                count += 1;
            }
            let value = String::from_utf16(unsafe { std::slice::from_raw_parts(text, count) })
                .map_err(|_| "workflow:pipe_identity_unavailable".to_string());
            unsafe {
                LocalFree(text as *mut c_void);
            }
            value
        })();
        unsafe {
            CloseHandle(token);
        }
        result
    }

    struct Owner_descriptor(*mut c_void);
    impl Owner_descriptor {
        fn new() -> Result<Self, String> {
            // Protected DACL grants generic all only to the process token user.
            let sddl: Vec<u16> = format!("D:P(A;;GA;;;{})", current_user_sid()?)
                .encode_utf16()
                .chain(Some(0))
                .collect();
            let mut descriptor = ptr::null_mut();
            let ok = unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    1,
                    &mut descriptor,
                    ptr::null_mut(),
                )
            };
            if ok == 0 || descriptor.is_null() {
                return Err("workflow:pipe_acl_unavailable".into());
            }
            Ok(Self(descriptor))
        }
    }
    impl Drop for Owner_descriptor {
        fn drop(&mut self) {
            unsafe {
                LocalFree(self.0);
            }
        }
    }

    struct Descriptor_guard {
        path: PathBuf,
        bytes: Vec<u8>,
    }

    impl Drop for Descriptor_guard {
        fn drop(&mut self) {
            if fs::read(&self.path).ok().as_deref() == Some(self.bytes.as_slice()) {
                let _ = fs::remove_file(&self.path);
            }
        }
    }

    fn create_pipe(
        name: &str,
        first: bool,
    ) -> Result<tokio::net::windows::named_pipe::NamedPipeServer, String> {
        let descriptor = Owner_descriptor::new()?;
        let mut attributes = Security_attributes {
            length: std::mem::size_of::<Security_attributes>() as u32,
            security_descriptor: descriptor.0,
            inherit_handle: 0,
        };
        let mut options = ServerOptions::new();
        options
            .first_pipe_instance(first)
            .reject_remote_clients(true);
        let pipe = unsafe {
            options
                .create_with_security_attributes_raw(name, &mut attributes as *mut _ as *mut c_void)
        }
        .map_err(|_| "workflow:pipe_create_failed".to_string())?;
        Ok(pipe)
    }

    pub struct Workflow_ipc {
        shutdown: Mutex<Option<oneshot::Sender<()>>>,
    }

    impl Default for Workflow_ipc {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Workflow_ipc {
        pub fn new() -> Self {
            Self {
                shutdown: Mutex::new(None),
            }
        }

        pub fn start(&self, app: tauri::AppHandle, runtime_root: PathBuf) {
            let (shutdown, receiver) = oneshot::channel();
            if let Ok(mut guard) = self.shutdown.lock() {
                if guard.is_some() {
                    return;
                }
                *guard = Some(shutdown);
            } else {
                return;
            }
            tauri::async_runtime::spawn(async move {
                if run(app, &runtime_root, receiver).await.is_err() {
                    log::warn!("workflow ipc: unavailable");
                }
            });
        }

        pub fn stop(&self) {
            if let Ok(mut guard) = self.shutdown.lock() {
                if let Some(sender) = guard.take() {
                    let _ = sender.send(());
                }
            }
        }
    }

    async fn run(
        app: tauri::AppHandle,
        runtime_root: &Path,
        mut shutdown: oneshot::Receiver<()>,
    ) -> Result<(), String> {
        private_fs::prepare_managed_private_dir(runtime_root)
            .map_err(|_| "workflow:runtime_root_unavailable".to_string())?;
        let mut random = [0u8; 16];
        getrandom::getrandom(&mut random)
            .map_err(|_| "workflow:instance_id_unavailable".to_string())?;
        let instance_id = hex::encode(random);
        let pipe_name = format!(r"\\.\pipe\aag_workflow_{instance_id}");
        let mut pipe = create_pipe(&pipe_name, true)?;
        let descriptor_path = runtime_root.join("workflow_ipc.json");
        let descriptor = Discovery_descriptor {
            schema_version: 1,
            instance_id: &instance_id,
            pipe_name: &pipe_name,
            process_id: std::process::id(),
        };
        let descriptor_bytes = serde_json::to_vec(&descriptor)
            .map_err(|_| "workflow:descriptor_invalid".to_string())?;
        private_fs::atomic_write(&descriptor_path, &descriptor_bytes)
            .map_err(|_| "workflow:descriptor_write_failed".to_string())?;
        let _descriptor_guard = Descriptor_guard {
            path: descriptor_path,
            bytes: descriptor_bytes,
        };
        let permits = Arc::new(Semaphore::new(MAX_ACTIVE_CLIENTS));
        let mut clients = JoinSet::new();
        loop {
            tokio::select! {
                _ = &mut shutdown => break,
                _ = clients.join_next(), if !clients.is_empty() => {},
                result = pipe.connect() => {
                    if result.is_err() {
                        // A client that connected and died before the connect
                        // resolved surfaces as an error here (e.g. Windows
                        // ERROR_NO_DATA). That is ordinary client churn, not a
                        // server fault — recreate the listening instance and
                        // keep serving. Only shutdown (or pipe creation
                        // failing below) ends the loop.
                        drop(pipe);
                        match create_pipe(&pipe_name, false) {
                            Ok(next) => {
                                pipe = next;
                                continue;
                            }
                            Err(code) => {
                                log::warn!("workflow ipc: pipe recreate failed code={code}");
                                break;
                            }
                        }
                    }
                    let next = match create_pipe(&pipe_name, false) {
                        Ok(next) => next,
                        // Transient handle pressure: drop the just-connected
                        // client rather than killing the whole IPC surface.
                        Err(code) => {
                            log::warn!("workflow ipc: pipe create failed code={code}");
                            drop(pipe);
                            match create_pipe(&pipe_name, false) {
                                Ok(next) => {
                                    pipe = next;
                                    continue;
                                }
                                Err(code) => {
                                    log::warn!("workflow ipc: pipe recreate failed code={code}");
                                    break;
                                }
                            }
                        }
                    };
                    let app = app.clone();
                    let current = instance_id.clone();
                    if let Ok(permit) = Arc::clone(&permits).try_acquire_owned() {
                        clients.spawn(async move { let _ = handle_client(pipe, app, &current, permit).await; });
                    } else {
                        drop(pipe);
                    }
                    pipe = next;
                }
            }
        }
        clients.abort_all();
        while clients.join_next().await.is_some() {}
        Ok(())
    }

    async fn handle_client(
        mut pipe: tokio::net::windows::named_pipe::NamedPipeServer,
        app: tauri::AppHandle,
        instance_id: &str,
        permit: OwnedSemaphorePermit,
    ) -> Result<(), String> {
        let mut frame = Vec::new();
        let read = timeout(CONNECTION_TIMEOUT, async {
            loop {
                let mut byte = [0u8; 1];
                let count = pipe.read(&mut byte).await.map_err(|_| "read_failed")?;
                if count == 0 {
                    return Err("truncated_frame");
                }
                frame.push(byte[0]);
                if frame.len() > FRAME_LIMIT {
                    return Err("frame_too_large");
                }
                if byte[0] == b'\n' {
                    return Ok(());
                }
            }
        })
        .await
        .map_err(|_| "read_timeout")?;
        let response = match read {
            Err(code) => error_response(instance_id, "", code),
            Ok(()) => match parse_request(&frame, instance_id) {
                Err(code) => error_response(instance_id, "", code),
                Ok(request) => {
                    let request_id = request.request_id.clone();
                    match timeout(
                        DISPATCH_TIMEOUT,
                        tauri::async_runtime::spawn_blocking(move || {
                            let _permit = permit;
                            dispatch(&app, request)
                        }),
                    )
                    .await
                    {
                        Ok(Ok(Ok(result))) => json!({"schema_version":1,"instance_id":instance_id,
                            "request_id":request_id,"ok":true,"result":result}),
                        Ok(Ok(Err(code))) => error_response(instance_id, &request_id, &code),
                        Ok(Err(_)) => error_response(instance_id, &request_id, "owner_unavailable"),
                        Err(_) => error_response(instance_id, &request_id, "dispatch_timeout"),
                    }
                }
            },
        };
        let mut bytes =
            serde_json::to_vec(&response).map_err(|_| "workflow:response_invalid".to_string())?;
        if bytes.len() > MAX_RESPONSE_BYTES {
            let request_id = response
                .get("request_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            bytes = serde_json::to_vec(&error_response(
                instance_id,
                request_id,
                "response_too_large",
            ))
            .map_err(|_| "workflow:response_invalid".to_string())?;
        }
        bytes.push(b'\n');
        timeout(CONNECTION_TIMEOUT, pipe.write_all(&bytes))
            .await
            .map_err(|_| "workflow:write_timeout".to_string())?
            .map_err(|_| "workflow:write_failed".to_string())?;
        Ok(())
    }

    fn dispatch(app: &tauri::AppHandle, request: Request) -> Result<Value, String> {
        let service = app.state::<Workflow_service>();
        let db = app.state::<Database>();
        let mcp = app.state::<Mcp_service>();
        let result = match request.method.as_str() {
            "list_capabilities" => {
                return capability_page(
                    service.list_capabilities(db.inner(), mcp.inner())?,
                    request.params.cursor.as_deref(),
                    request.params.limit,
                );
            }
            "get_health" => {
                return health_page(
                    service.health(db.inner(), mcp.inner())?,
                    request.params.cursor.as_deref(),
                    request.params.limit,
                );
            }
            "route_preview" => serde_json::to_value(
                service.route_preview(request.params.task.as_deref().unwrap_or_default()),
            ),
            _ => return Err("method_not_allowed".into()),
        };
        result.map_err(|_| "response_invalid".into())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use tokio::net::windows::named_pipe::ClientOptions;

        #[tokio::test]
        async fn current_user_can_connect_to_restricted_local_pipe() {
            let mut nonce = [0u8; 8];
            getrandom::getrandom(&mut nonce).expect("nonce");
            let name = format!(r"\\.\pipe\aag_workflow_test_{}", hex::encode(nonce));
            let server = create_pipe(&name, true).expect("restricted pipe");
            let client = ClientOptions::new()
                .open(&name)
                .expect("current user connection");
            timeout(Duration::from_secs(3), server.connect())
                .await
                .expect("deadline")
                .expect("connect");
            drop(client);
        }
    }
}

#[cfg(windows)]
pub use windows::Workflow_ipc;

#[cfg(not(windows))]
pub struct Workflow_ipc;

#[cfg(not(windows))]
impl Workflow_ipc {
    pub fn new() -> Self {
        Self
    }
    pub fn start(&self, _app: tauri::AppHandle, _runtime_root: std::path::PathBuf) {}
    pub fn stop(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_error_codes_have_one_stable_namespace() {
        assert_eq!(
            error_response("one", "r1", "workflow:mcp_registry_unavailable")["error"]["code"],
            "mcp_registry_unavailable"
        );
        assert_eq!(
            error_response("one", "r1", "unexpected private detail")["error"]["code"],
            "owner_unavailable"
        );
    }

    #[test]
    fn full_registry_pages_stay_bounded_and_preserve_each_identity() {
        let mut capabilities = Vec::new();
        for index in 0..128 {
            capabilities.push(super::super::types::Capability {
                id: format!("mcp_server_{index}"),
                name: "M".repeat(128),
                kind: "mcp_server".into(),
                group: "mcp".into(),
                owner: "mcp_registry".into(),
                access_method: "mcp_registry".into(),
                host_scope: "local".into(),
                source_ref: "mcp_registry".into(),
                mcp_server_id: Some(format!("server_{index}")),
                summary: "界".repeat(256),
                probe_supported: false,
                observations: vec![],
            });
        }
        let list = super::super::types::Capability_list {
            schema_version: 1,
            capabilities,
            next_cursor: None,
        };
        let mut cursor = None;
        let mut observed = Vec::new();
        loop {
            let page =
                capability_page(list.clone(), cursor.as_deref(), Some(16)).expect("bounded page");
            assert!(serde_json::to_vec(&page).unwrap().len() <= PAGE_RESPONSE_LIMIT);
            for item in page["capabilities"].as_array().unwrap() {
                observed.push(item["id"].as_str().unwrap().to_string());
            }
            cursor = page
                .get("next_cursor")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(observed.len(), 128);
        for (index, id) in observed.iter().enumerate() {
            assert_eq!(id, &format!("mcp_server_{index}"));
        }
        assert_eq!(
            capability_page(list, Some("999"), None).unwrap_err(),
            "invalid_cursor"
        );
    }

    #[test]
    fn paging_is_only_allowed_on_catalog_and_health_reads() {
        let page = b"{\"schema_version\":1,\"instance_id\":\"one\",\"request_id\":\"r1\",\"method\":\"get_health\",\"params\":{\"cursor\":\"8\",\"limit\":16}}\n";
        assert!(parse_request(page, "one").is_ok());
        let invalid = b"{\"schema_version\":1,\"instance_id\":\"one\",\"request_id\":\"r1\",\"method\":\"route_preview\",\"params\":{\"task\":\"task\",\"limit\":1}}\n";
        assert_eq!(parse_request(invalid, "one").unwrap_err(), "invalid_params");
    }

    #[test]
    fn rejects_mutation_and_stale_identity() {
        let mutation = b"{\"schema_version\":1,\"instance_id\":\"one\",\"request_id\":\"r1\",\"method\":\"probe_capability\",\"params\":{}}\n";
        assert_eq!(
            parse_request(mutation, "one").unwrap_err(),
            "method_not_allowed"
        );
        let stale = b"{\"schema_version\":1,\"instance_id\":\"old\",\"request_id\":\"r1\",\"method\":\"get_health\",\"params\":{}}\n";
        assert_eq!(parse_request(stale, "new").unwrap_err(), "stale_instance");
    }

    #[test]
    fn rejects_malformed_and_oversized_frames() {
        assert_eq!(parse_request(b"{}", "one").unwrap_err(), "malformed_frame");
        let oversized = vec![b'x'; FRAME_LIMIT + 1];
        assert_eq!(
            parse_request(&oversized, "one").unwrap_err(),
            "malformed_frame"
        );
    }
}
