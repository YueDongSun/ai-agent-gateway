//! Request-local translation of Responses namespaces to flat function tools.
//! Native Responses envelopes do not use this projection. No mapping is global.

use crate::proxy::canonical::{self, ProviderError};
use crate::proxy::responses::ResponsesRequest;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

const MAX_TOOL_IDENTITIES: usize = 1024;
const MAX_FLAT_NAME_BYTES: usize = 64;

#[derive(Clone, Default)]
#[allow(non_camel_case_types)]
pub struct Responses_namespace_map {
    by_identity: BTreeMap<(String, String), String>,
    by_flat_name: BTreeMap<String, (String, String)>,
}

fn invalid_tools() -> ProviderError {
    canonical::make_provider_error(
        "responses_feature_unsupported",
        "request_conversion",
        Some(400),
        Some(false),
        None,
    )
}

fn flat_name_valid(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_FLAT_NAME_BYTES
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn identity(
    item: &Value,
    namespace: Option<&str>,
) -> Result<(Option<String>, String), ProviderError> {
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(invalid_tools)?;
    let namespace = match namespace {
        Some(namespace) => Some(namespace),
        None => match item.get("namespace") {
            None | Some(Value::Null) => None,
            Some(Value::String(namespace)) => Some(namespace.as_str()),
            _ => return Err(invalid_tools()),
        },
    };
    if name.chars().any(char::is_control)
        || namespace.is_some_and(|namespace| {
            namespace.is_empty() || namespace.chars().any(char::is_control)
        })
        || (namespace.is_none() && !flat_name_valid(name))
    {
        return Err(invalid_tools());
    }
    Ok((namespace.map(str::to_string), name.to_string()))
}

fn flat_namespace_name(namespace: &str, name: &str) -> String {
    let readable = format!("{namespace}__{name}");
    if flat_name_valid(&readable) {
        return readable;
    }
    // A bounded wire identity is needed when the qualified name cannot fit the
    // upstream's 64-byte function-name contract. Collision checks below still
    // compare full original identities before admitting the projection.
    let mut digest = Sha256::new();
    digest.update((namespace.len() as u64).to_be_bytes());
    digest.update(namespace.as_bytes());
    digest.update(name.as_bytes());
    format!("aag_ns_{}", &hex::encode(digest.finalize())[..56])
}

impl Responses_namespace_map {
    pub fn from_request(request: &ResponsesRequest) -> Result<Self, ProviderError> {
        let mut identities = BTreeSet::new();
        let mut declared = BTreeSet::new();
        let mut add_declaration = |item: &Value, namespace: Option<&str>| {
            let item_identity = identity(item, namespace)?;
            if !declared.insert(item_identity.clone()) {
                return Err(invalid_tools());
            }
            identities.insert(item_identity);
            if identities.len() > MAX_TOOL_IDENTITIES {
                return Err(invalid_tools());
            }
            Ok(())
        };
        if let Some(tools) = &request.tools {
            let tools: Vec<_> = tools
                .as_array()
                .map(|tools| tools.iter().collect())
                .unwrap_or_else(|| vec![tools]);
            for tool in tools {
                match tool.get("type").and_then(Value::as_str) {
                    Some("function") => add_declaration(tool, None)?,
                    Some("namespace") => {
                        let namespace = tool
                            .get("name")
                            .and_then(Value::as_str)
                            .filter(|name| !name.is_empty())
                            .ok_or_else(invalid_tools)?;
                        let children = tool
                            .get("tools")
                            .and_then(Value::as_array)
                            .filter(|tools| !tools.is_empty())
                            .ok_or_else(invalid_tools)?;
                        for child in children {
                            if child.get("type").and_then(Value::as_str) != Some("function") {
                                return Err(invalid_tools());
                            }
                            add_declaration(child, Some(namespace))?;
                        }
                    }
                    // Existing strict validation owns other tool kinds. The
                    // lenient/native path must continue carrying their raw data.
                    _ => {}
                }
            }
        }
        drop(add_declaration);
        for item in request.input.as_array().into_iter().flatten() {
            if item.get("type").and_then(Value::as_str) == Some("function_call") {
                identities.insert(identity(item, None)?);
                if identities.len() > MAX_TOOL_IDENTITIES {
                    return Err(invalid_tools());
                }
            }
        }
        if let Some(choice) = request.extra.get("tool_choice") {
            if choice.get("type").and_then(Value::as_str) == Some("function") {
                identities.insert(identity(choice, None)?);
            }
        }
        if identities.len() > MAX_TOOL_IDENTITIES {
            return Err(invalid_tools());
        }
        let ordinary: BTreeSet<_> = identities
            .iter()
            .filter(|(namespace, _)| namespace.is_none())
            .map(|(_, name)| name.as_str())
            .collect();
        let mut map = Self::default();
        for (namespace, name) in &identities {
            let Some(namespace) = namespace else { continue };
            let flat = flat_namespace_name(namespace, name);
            if ordinary.contains(flat.as_str()) || map.by_flat_name.contains_key(&flat) {
                return Err(invalid_tools());
            }
            map.by_identity
                .insert((namespace.clone(), name.clone()), flat.clone());
            map.by_flat_name
                .insert(flat, (namespace.clone(), name.clone()));
        }
        Ok(map)
    }

    pub fn normalize_request<'a>(
        &self,
        request: &'a ResponsesRequest,
    ) -> Result<Cow<'a, ResponsesRequest>, ProviderError> {
        if self.by_identity.is_empty() {
            return Ok(Cow::Borrowed(request));
        }
        let mut normalized = request.clone();
        if let Some(tools) = &request.tools {
            let tools: Vec<_> = tools
                .as_array()
                .map(|tools| tools.iter().collect())
                .unwrap_or_else(|| vec![tools]);
            let mut flattened = Vec::new();
            for tool in tools {
                if tool.get("type").and_then(Value::as_str) != Some("namespace") {
                    flattened.push(tool.clone());
                    continue;
                }
                let namespace = tool["name"].as_str().ok_or_else(invalid_tools)?;
                for child in tool["tools"].as_array().ok_or_else(invalid_tools)? {
                    let mut child = child.clone();
                    child["namespace"] = Value::String(namespace.to_string());
                    self.flatten_call(&mut child)?;
                    if let Some(context) = tool
                        .get("description")
                        .and_then(Value::as_str)
                        .filter(|text| !text.is_empty())
                    {
                        let description = child
                            .get("description")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        child["description"] = Value::String(format!("{context}\n\n{description}"));
                    }
                    flattened.push(child);
                }
            }
            normalized.tools = Some(Value::Array(flattened));
        }
        for item in normalized.input.as_array_mut().into_iter().flatten() {
            if item.get("type").and_then(Value::as_str) == Some("function_call") {
                self.flatten_call(item)?;
            }
        }
        if let Some(choice) = normalized.extra.get_mut("tool_choice") {
            if choice.get("type").and_then(Value::as_str) == Some("function") {
                self.flatten_call(choice)?;
            }
        }
        Ok(Cow::Owned(normalized))
    }

    fn flatten_call(&self, item: &mut Value) -> Result<(), ProviderError> {
        let (namespace, name) = identity(item, None)?;
        if let Some(namespace) = namespace {
            let flat = self
                .by_identity
                .get(&(namespace, name))
                .ok_or_else(invalid_tools)?;
            item["name"] = Value::String(flat.clone());
            item.as_object_mut()
                .ok_or_else(invalid_tools)?
                .remove("namespace");
        }
        Ok(())
    }

    fn restore_item(&self, item: &mut Value) -> bool {
        if item.get("type").and_then(Value::as_str) != Some("function_call")
            || item.get("namespace").is_some()
        {
            return false;
        }
        let Some((namespace, name)) = item
            .get("name")
            .and_then(Value::as_str)
            .and_then(|name| self.by_flat_name.get(name))
        else {
            return false;
        };
        item["name"] = Value::String(name.clone());
        item["namespace"] = Value::String(namespace.clone());
        true
    }

    pub fn restore_response(&self, response: &mut Value) -> bool {
        let mut changed = false;
        for item in response
            .get_mut("output")
            .and_then(Value::as_array_mut)
            .into_iter()
            .flatten()
        {
            changed |= self.restore_item(item);
        }
        changed
    }

    /// Called only on synthesized frames, never on native source events.
    pub fn restore_generated_frame(&self, frame: String) -> String {
        if self.by_flat_name.is_empty() {
            return frame;
        }
        let Some((prefix, payload)) = frame.split_once("data: ") else {
            return frame;
        };
        let Ok(mut event) = serde_json::from_str::<Value>(payload.trim()) else {
            return frame;
        };
        let changed = match event.get("type").and_then(Value::as_str) {
            Some("response.output_item.added" | "response.output_item.done") => event
                .get_mut("item")
                .is_some_and(|item| self.restore_item(item)),
            Some("response.completed" | "response.incomplete") => event
                .get_mut("response")
                .is_some_and(|response| self.restore_response(response)),
            _ => false,
        };
        if changed {
            format!("{prefix}data: {event}\n\n")
        } else {
            frame
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::canonical::{OAIChatResponse, StreamDelta};
    use crate::proxy::converter::{
        chat_chunks_to_responses_stream_with_namespaces, chat_to_responses_with_namespaces,
        make_stream_chunk, responses_request_body, responses_to_chat, try_responses_to_chat,
    };
    use crate::proxy::responses::ResponsesSseEvent;
    use futures_util::StreamExt;
    use serde_json::json;

    fn namespace_request() -> ResponsesRequest {
        serde_json::from_value(json!({
            "model":"fixture", "input":[
                {"type":"message", "role":"user", "content":"fixture"},
                {"type":"function_call", "id":"client_call", "call_id":"call_1", "namespace":"crm", "name":"lookup", "arguments":"{}", "status":"completed"},
                {"type":"function_call_output", "call_id":"call_1", "output":"fixture result"}
            ],
            "tools":[
                {"type":"function", "name":"ordinary", "parameters":{"type":"object"}},
                {"type":"namespace", "name":"crm", "description":"Customer records.", "tools":[
                    {"type":"function", "name":"lookup", "description":"Find a customer.", "parameters":{"type":"object", "properties":{"id":{"type":"string"}}}, "strict":true}
                ]},
                {"type":"namespace", "name":"docs", "description":"Documentation.", "tools":[
                    {"type":"function", "name":"lookup", "parameters":{"type":"object"}}
                ]}
            ],
            "tool_choice":{"type":"function", "namespace":"crm", "name":"lookup"}
        })).unwrap()
    }

    fn tool_response(name: &str) -> OAIChatResponse {
        serde_json::from_value(json!({
            "id":"fixture_response", "object":"chat.completion", "created":0, "model":"fixture",
            "choices":[{"index":0, "message":{"role":"assistant", "content":null, "tool_calls":[
                {"id":"call_output", "type":"function", "function":{"name":name,"arguments":"{\"literal\":\"crm__lookup\"}"}}
            ]}, "finish_reason":"tool_calls"}]
        })).unwrap()
    }

    #[test]
    fn namespaced_functions_flatten_declarations_choice_and_history_without_wire_leakage() {
        let request = namespace_request();
        let canonical = try_responses_to_chat(&request).unwrap();
        let tools = canonical.tools.as_ref().unwrap();
        assert_eq!(tools.len(), 3);
        assert_eq!(tools[0]["function"]["name"], "ordinary");
        assert_eq!(tools[1]["function"]["name"], "crm__lookup");
        assert_eq!(tools[2]["function"]["name"], "docs__lookup");
        assert_eq!(tools[1]["function"]["strict"], true);
        assert!(tools[1]["function"]["description"]
            .as_str()
            .unwrap()
            .contains("Customer records."));
        assert_eq!(
            canonical.tool_choice.as_ref().unwrap()["function"]["name"],
            "crm__lookup"
        );
        assert_eq!(
            canonical.messages[1].tool_calls.as_ref().unwrap()[0]["function"]["name"],
            "crm__lookup"
        );
        assert_eq!(
            canonical.messages[2].tool_call_id.as_deref(),
            Some("call_1")
        );
        let wire = serde_json::to_value(&canonical).unwrap();
        assert!(!wire.to_string().contains("\"namespace\""));
        assert_eq!(
            serde_json::to_value(canonical.source_responses_request.unwrap()).unwrap(),
            serde_json::to_value(&request).unwrap()
        );
        let map = Responses_namespace_map::from_request(&request).unwrap();
        let restored =
            chat_to_responses_with_namespaces(&tool_response("crm__lookup"), "fixture", Some(&map));
        assert_eq!(restored["output"][0]["name"], "lookup");
        assert_eq!(restored["output"][0]["namespace"], "crm");
        assert_eq!(
            restored["output"][0]["arguments"],
            "{\"literal\":\"crm__lookup\"}"
        );
        let ordinary =
            chat_to_responses_with_namespaces(&tool_response("ordinary"), "fixture", Some(&map));
        assert_eq!(ordinary["output"][0]["name"], "ordinary");
        assert!(ordinary["output"][0].get("namespace").is_none());
    }

    #[test]
    fn namespace_projection_refuses_collisions_and_unsupported_children() {
        for tools in [
            json!([
                {"type":"function","name":"crm__lookup"},
                {"type":"namespace","name":"crm","tools":[{"type":"function","name":"lookup"}]}
            ]),
            json!([{ "type":"namespace","name":"crm","tools":[{"type":"function","name":"lookup"},{"type":"function","name":"lookup"}] }]),
            json!([{ "type":"namespace","name":"crm","tools":[{"type":"custom","name":"lookup"}] }]),
            json!([{ "type":"web_search" }]),
        ] {
            let request: ResponsesRequest =
                serde_json::from_value(json!({"model":"fixture","input":"fixture","tools":tools}))
                    .unwrap();
            assert!(try_responses_to_chat(&request).is_err());
            // Native source envelopes remain available even for declarations
            // the stateless translator cannot safely represent.
            let body: Value = serde_json::from_slice(
                &responses_request_body(&responses_to_chat(&request), "fixture", false).unwrap(),
            )
            .unwrap();
            assert_eq!(body["tools"], request.tools.unwrap());
        }
    }

    #[test]
    fn namespace_projection_names_are_stable_bounded_and_distinct() {
        let namespace_a = "namespace_a".repeat(12);
        let namespace_b = "namespace_b".repeat(12);
        let request: ResponsesRequest = serde_json::from_value(json!({
            "model":"fixture", "input":"fixture", "tools":[
                {"type":"namespace","name":namespace_a,"tools":[{"type":"function","name":"lookup"}]},
                {"type":"namespace","name":namespace_b,"tools":[{"type":"function","name":"lookup"}]}
            ]
        })).unwrap();
        let first = try_responses_to_chat(&request).unwrap();
        let mut reordered = request.clone();
        reordered
            .tools
            .as_mut()
            .unwrap()
            .as_array_mut()
            .unwrap()
            .reverse();
        let second = try_responses_to_chat(&reordered).unwrap();
        let names: Vec<_> = first
            .tools
            .as_ref()
            .unwrap()
            .iter()
            .map(|tool| tool["function"]["name"].as_str().unwrap())
            .collect();
        assert_ne!(names[0], names[1]);
        assert!(names.iter().all(|name| flat_name_valid(name)));
        assert_eq!(
            first.tools.as_ref().unwrap()[0]["function"]["name"],
            second.tools.as_ref().unwrap()[1]["function"]["name"]
        );
        let map = Responses_namespace_map::from_request(&request).unwrap();
        let restored =
            chat_to_responses_with_namespaces(&tool_response(names[1]), "fixture", Some(&map));
        assert_eq!(restored["output"][0]["namespace"], namespace_b);
        assert_eq!(restored["output"][0]["name"], "lookup");
    }

    #[tokio::test]
    async fn namespace_projection_restores_generated_stream_items_and_terminal_output() {
        let map = Responses_namespace_map::from_request(&namespace_request()).unwrap();
        let chunk = make_stream_chunk(
            "fixture_response",
            "fixture",
            StreamDelta {
                role: Some("assistant".into()),
                content: None,
                reasoning: None,
                extra: Default::default(),
                tool_calls: Some(vec![
                    json!({"index":0,"id":"call_1","function":{"name":"docs__lookup","arguments":"{\"literal\":\"crm__lookup\"}"}}),
                ]),
            },
            Some("tool_calls".into()),
        );
        let frames = chat_chunks_to_responses_stream_with_namespaces(
            Box::pin(futures_util::stream::iter([chunk])),
            "fixture",
            Some(map),
        )
        .collect::<Vec<_>>()
        .await;
        let events: Vec<Value> = frames
            .iter()
            .flat_map(|frame| frame.lines())
            .filter_map(|line| line.strip_prefix("data: "))
            .filter_map(|data| serde_json::from_str(data).ok())
            .collect();
        for kind in ["response.output_item.added", "response.output_item.done"] {
            let item = &events.iter().find(|event| event["type"] == kind).unwrap()["item"];
            assert_eq!(item["name"], "lookup");
            assert_eq!(item["namespace"], "docs");
        }
        let terminal = events
            .iter()
            .find(|event| event["type"] == "response.completed")
            .unwrap();
        assert_eq!(terminal["response"]["output"][0]["name"], "lookup");
        assert_eq!(terminal["response"]["output"][0]["namespace"], "docs");
        assert_eq!(
            terminal["response"]["output"][0]["arguments"],
            "{\"literal\":\"crm__lookup\"}"
        );
    }

    #[tokio::test]
    async fn namespace_projection_never_rewrites_native_requests_responses_or_events() {
        let request = namespace_request();
        let map = Responses_namespace_map::from_request(&request).unwrap();
        let body: Value = serde_json::from_slice(
            &responses_request_body(&responses_to_chat(&request), "fixture", false).unwrap(),
        )
        .unwrap();
        assert_eq!(body["tools"], request.tools.unwrap());
        assert_eq!(body["input"], request.input);
        assert_eq!(body["tool_choice"], request.extra["tool_choice"]);
        let source = json!({"id":"native", "status":"completed", "output":[{"type":"function_call","name":"crm__lookup","namespace":"native_namespace","call_id":"native_call","arguments":"{}"}]});
        let mut response = tool_response("crm__lookup");
        response.source_responses_response = Some(source.clone());
        assert_eq!(
            chat_to_responses_with_namespaces(&response, "fixture", Some(&map)),
            source
        );
        let source_event = ResponsesSseEvent {
            event: Some("response.completed".into()),
            id: Some("native_event".into()),
            retry: None,
            data: json!({"type":"response.completed","response":source}).to_string(),
        };
        let expected_frame = source_event.to_sse();
        let mut chunk = make_stream_chunk(
            "native",
            "fixture",
            StreamDelta {
                role: None,
                content: None,
                tool_calls: None,
                reasoning: None,
                extra: Default::default(),
            },
            Some("stop".into()),
        );
        chunk.source_responses_event = Some(source_event);
        let frames = chat_chunks_to_responses_stream_with_namespaces(
            Box::pin(futures_util::stream::iter([chunk])),
            "fixture",
            Some(map),
        )
        .collect::<Vec<_>>()
        .await;
        assert_eq!(frames[0], expected_frame);
    }
}
