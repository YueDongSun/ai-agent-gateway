use super::{BrokerError, ClientRequest};

// Native protocol names are preserved at this boundary. Each addition needs a
// policy decision; a new native RPC never gains authority merely by appearing.
const GLOBAL_READ_METHODS: &[&str] = &[
    "initialize",
    "model/list",
    "thread/list",
    "thread/search",
    "thread/loaded/list",
    "thread/attachmentOwner/list",
];
const THREAD_READ_METHODS: &[&str] = &[
    "thread/read",
    "thread/searchOccurrences",
    "thread/turns/list",
    "thread/items/list",
    "thread/items/read",
    "thread/timeline/list",
    "thread/attachment/list",
    "thread/goal/get",
    "thread/queue/list",
    "thread/backgroundTerminals/list",
];
const THREAD_MUTATION_METHODS: &[&str] = &[
    "turn/start",
    "turn/steer",
    "turn/interrupt",
    "turn/settings/update",
    "thread/fork",
    "thread/archive",
    "thread/delete",
    "thread/name/set",
    "thread/prediction/request",
    "thread/goal/set",
    "thread/goal/clear",
    "thread/queue/add",
    "thread/queue/update",
    "thread/queue/delete",
    "thread/queue/reorder",
    "thread/queue/start",
    "thread/metadata/update",
    "thread/attachment/add",
    "thread/attachment/remove",
    "thread/section/move",
    "thread/settings/update",
    "thread/memoryMode/set",
    "thread/unarchive",
    "thread/compact/start",
    "thread/revert",
    "thread/readState/update",
    "review/start",
];
const OWNER_CONTROLLED_FIELDS: &[&str] = &[
    "path",
    "history",
    "config",
    "modelProvider",
    "approvalPolicy",
    "approvalsReviewer",
    "permissions",
    "sandbox",
    "sandboxPolicy",
    "runtimeWorkspaceRoots",
    "environments",
    "selectedCapabilityRoots",
    "dynamicTools",
    "disabledPluginIds",
];
const INFLIGHT_CONTROL_METHODS: &[&str] = &["turn/steer", "turn/interrupt"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Operation {
    ReadGlobal,
    ReadThread,
    Resume,
    MutateThread,
}

pub(super) fn classify_request(request: &ClientRequest<'_>) -> Result<Operation, BrokerError> {
    let params = request
        .params
        .as_object()
        .ok_or(BrokerError::InvalidRequest)?;
    let operation = if GLOBAL_READ_METHODS.contains(&request.method) {
        Operation::ReadGlobal
    } else if THREAD_READ_METHODS.contains(&request.method) {
        Operation::ReadThread
    } else if request.method == "thread/resume" {
        Operation::Resume
    } else if THREAD_MUTATION_METHODS.contains(&request.method) {
        Operation::MutateThread
    } else {
        // Raw filesystem/process/command APIs, native history injection,
        // approval escalation, config/import/reset, and thread/start are not
        // remote client authority. New roots must be owner-mediated.
        return Err(BrokerError::ForbiddenMethod);
    };
    // Optional null fields mean no override in the native schema. Non-null
    // history (even []) and path (even "") are never forwarded.
    if OWNER_CONTROLLED_FIELDS
        .iter()
        .any(|key| params.get(*key).is_some_and(|value| !value.is_null()))
        || (matches!(operation, Operation::Resume | Operation::MutateThread)
            && params.get("cwd").is_some_and(|value| !value.is_null()))
    {
        return Err(BrokerError::ForbiddenOverride);
    }
    Ok(operation)
}

pub(super) fn validate_inflight_request(request: &ClientRequest<'_>) -> Result<(), BrokerError> {
    let operation = classify_request(request)?;
    if operation != Operation::MutateThread || !INFLIGHT_CONTROL_METHODS.contains(&request.method) {
        return Err(BrokerError::ForbiddenMethod);
    }
    Ok(())
}
