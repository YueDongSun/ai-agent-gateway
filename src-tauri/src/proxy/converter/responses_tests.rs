use crate::proxy::canonical::{
    OAIChatResponse, OAIChoice, OAIMessage, OAIStreamChunk, StreamChoice, StreamDelta, Usage,
};
use crate::proxy::responses::ResponsesRequest;
use futures_core::stream::Stream;
use futures_util::StreamExt;
use serde_json::Value;
use std::collections::HashMap;
use std::pin::Pin;


use super::*;

fn empty_extra() -> HashMap<String, Value> {
    HashMap::new()
}

#[test]
fn responses_request_string_input_to_chat() {
    let req = ResponsesRequest {
        model: "gpt-5".into(),
        input: serde_json::json!("hello"),
        instructions: Some("be brief".into()),
        stream: false,
        temperature: Some(0.5),
        max_output_tokens: Some(64),
        tools: None,
        metadata: None,
        previous_response_id: None,
        extra: empty_extra(),
    };
    let chat = responses_to_chat(&req);
    assert_eq!(chat.model, "gpt-5");
    assert_eq!(chat.messages.len(), 2);
    assert_eq!(chat.messages[0].role, "system");
    assert_eq!(chat.messages[0].content, serde_json::json!("be brief"));
    assert_eq!(chat.messages[1].role, "user");
    assert_eq!(chat.messages[1].content, serde_json::json!("hello"));
    assert_eq!(chat.temperature, Some(0.5));
    assert_eq!(chat.max_tokens, Some(64));
}

#[test]
fn responses_request_array_input_to_chat() {
    let req = ResponsesRequest {
        model: "gpt-5".into(),
        input: serde_json::json!([
            { "role": "user", "content": "first" },
            { "role": "assistant", "content": "ok" },
            { "role": "user", "content": [
                { "type": "text", "text": "second" },
            ]},
        ]),
        instructions: None,
        stream: false,
        temperature: None,
        max_output_tokens: None,
        tools: None,
        metadata: None,
        previous_response_id: None,
        extra: empty_extra(),
    };
    let chat = responses_to_chat(&req);
    assert_eq!(chat.messages.len(), 3);
    assert_eq!(chat.messages[0].role, "user");
    assert_eq!(chat.messages[1].role, "assistant");
    assert_eq!(chat.messages[2].role, "user");
    let text = chat.messages[2].content.as_array().unwrap();
    assert_eq!(text[0]["text"], "second");
}

#[test]
fn chat_response_to_responses_envelope() {
    let chat = OAIChatResponse {
        id: "chatcmpl-1".into(),
        object: "chat.completion".into(),
        created: 1_700_000_000,
        model: "gpt-5".into(),
        choices: vec![OAIChoice {
            index: 0,
            message: OAIMessage {
                role: "assistant".into(),
                content: Some("hi there".into()),
                tool_calls: None,
                refusal: None,
                extra: empty_extra(),
            },
            finish_reason: Some("stop".into()),
            extra: Default::default(),
        }],
        usage: Some(Usage {
            prompt_tokens: 5,
            completion_tokens: 7,
            total_tokens: 12,
            extra: Default::default(),
        }),
        source_anthropic_response: None,
        source_gemini_response: None,
        source_responses_response: None,
        extra: Default::default(),
    };
    let r = chat_to_responses(&chat, "gpt-5");
    assert_eq!(r["object"], "response");
    assert_eq!(r["status"], "completed");
    assert_eq!(r["id"], "chatcmpl-1");
    assert!(
        r["error"].is_null(),
        "stateless envelope carries a null error"
    );
    let output = r["output"].as_array().unwrap();
    assert_eq!(output.len(), 1);
    assert_eq!(output[0]["type"], "message");
    assert_eq!(output[0]["role"], "assistant");
    assert_eq!(output[0]["content"][0]["type"], "output_text");
    assert_eq!(output[0]["content"][0]["text"], "hi there");
    assert_eq!(r["usage"]["input_tokens"], 5);
    assert_eq!(r["usage"]["output_tokens"], 7);
    assert_eq!(r["usage"]["total_tokens"], 12);
}

#[test]
fn chat_response_reasoning_extension_becomes_a_reasoning_item() {
    let mut extra = HashMap::new();
    extra.insert(
        "reasoning_content".to_string(),
        Value::String("thinking it through".into()),
    );
    let chat = OAIChatResponse {
        id: "chatcmpl-r".into(),
        object: "chat.completion".into(),
        created: 1_700_000_000,
        model: "gpt-5".into(),
        choices: vec![OAIChoice {
            index: 0,
            message: OAIMessage {
                role: "assistant".into(),
                content: Some("the answer".into()),
                tool_calls: None,
                refusal: None,
                extra,
            },
            finish_reason: Some("stop".into()),
            extra: Default::default(),
        }],
        usage: None,
        source_anthropic_response: None,
        source_gemini_response: None,
        source_responses_response: None,
        extra: Default::default(),
    };
    let r = chat_to_responses(&chat, "gpt-5");
    let output = r["output"].as_array().unwrap();
    assert_eq!(output.len(), 2, "reasoning then message");
    assert_eq!(output[0]["type"], "reasoning");
    assert_eq!(output[0]["status"], "completed");
    assert_eq!(output[0]["content"][0]["type"], "reasoning_text");
    assert_eq!(output[0]["content"][0]["text"], "thinking it through");
    assert_eq!(output[1]["type"], "message");
    assert_eq!(output[1]["content"][0]["text"], "the answer");
}

#[test]
fn chat_response_tool_calls_become_function_call_items() {
    let chat = OAIChatResponse {
        id: "chatcmpl-2".into(),
        object: "chat.completion".into(),
        created: 1_700_000_000,
        model: "gpt-5".into(),
        choices: vec![OAIChoice {
            index: 0,
            message: OAIMessage {
                role: "assistant".into(),
                content: None,
                tool_calls: Some(vec![serde_json::json!({
                    "id": "call_abc",
                    "type": "function",
                    "function": { "name": "shell", "arguments": "{\"cmd\":\"ls\"}" },
                })]),
                refusal: None,
                extra: empty_extra(),
            },
            finish_reason: Some("tool_calls".into()),
            extra: Default::default(),
        }],
        usage: None,
        source_anthropic_response: None,
        source_gemini_response: None,
        source_responses_response: None,
        extra: Default::default(),
    };
    let r = chat_to_responses(&chat, "gpt-5");
    let output = r["output"].as_array().unwrap();
    assert_eq!(output.len(), 1, "tool-only response has no message item");
    assert_eq!(output[0]["type"], "function_call");
    assert_eq!(output[0]["call_id"], "call_abc");
    assert_eq!(output[0]["name"], "shell");
    assert_eq!(output[0]["arguments"], "{\"cmd\":\"ls\"}");
    assert_eq!(output[0]["status"], "completed");
}

#[test]
fn responses_input_tool_items_convert_round_trip() {
    let req = ResponsesRequest {
        model: "gpt-5".into(),
        input: serde_json::json!([
            { "type": "message", "role": "user", "content": "list files" },
            { "type": "function_call", "call_id": "call_1", "name": "shell", "arguments": "{\"cmd\":\"ls\"}" },
            { "type": "function_call_output", "call_id": "call_1", "output": "a.txt" },
            { "type": "reasoning", "summary": [] },
        ]),
        instructions: None,
        stream: false,
        temperature: None,
        max_output_tokens: None,
        tools: Some(serde_json::json!([
            { "type": "function", "name": "shell", "description": "run a command", "parameters": { "type": "object", "properties": {} } },
            { "type": "web_search" },
        ])),
        metadata: None,
        previous_response_id: None,
        extra: [(
            "tool_choice".to_string(),
            serde_json::json!({"type": "function", "name": "shell"}),
        )]
        .into_iter()
        .collect(),
    };
    let chat = responses_to_chat(&req);
    assert_eq!(chat.messages.len(), 3, "reasoning item is dropped");

    assert_eq!(chat.messages[1].role, "assistant");
    let calls = chat.messages[1].tool_calls.as_ref().unwrap();
    assert_eq!(calls[0]["id"], "call_1");
    assert_eq!(calls[0]["function"]["name"], "shell");
    assert_eq!(calls[0]["function"]["arguments"], "{\"cmd\":\"ls\"}");

    assert_eq!(chat.messages[2].role, "tool");
    assert_eq!(chat.messages[2].tool_call_id.as_deref(), Some("call_1"));
    assert_eq!(chat.messages[2].content, serde_json::json!("a.txt"));

    // Flat Responses tools -> nested chat tools; non-function tools dropped.
    let tools = chat.tools.as_ref().unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["type"], "function");
    assert_eq!(tools[0]["function"]["name"], "shell");
    // {type:function,name} tool_choice -> nested form.
    assert_eq!(chat.tool_choice.unwrap()["function"]["name"], "shell");
}

#[tokio::test]
async fn responses_stream_emits_created_delta_done_completed() {
    let chunks = vec![
        OAIStreamChunk {
            id: "x".into(),
            object: "chat.completion.chunk".into(),
            created: 1,
            model: "gpt-5".into(),
            choices: vec![StreamChoice {
                index: 0,
                delta: StreamDelta {
                    role: Some("assistant".into()),
                    content: Some("hel".into()),
                    tool_calls: None,
                    reasoning: None,
                    extra: Default::default(),
                },
                finish_reason: None,
                extra: empty_extra(),
            }],
            extra: empty_extra(),
            source_anthropic_event: None,
            source_gemini_event: None,
            source_responses_event: None,
        },
        OAIStreamChunk {
            id: "x".into(),
            object: "chat.completion.chunk".into(),
            created: 1,
            model: "gpt-5".into(),
            choices: vec![StreamChoice {
                index: 0,
                delta: StreamDelta {
                    role: None,
                    content: Some("lo".into()),
                    tool_calls: None,
                    reasoning: None,
                    extra: Default::default(),
                },
                finish_reason: Some("stop".into()),
                extra: empty_extra(),
            }],
            extra: empty_extra(),
            source_anthropic_event: None,
            source_gemini_event: None,
            source_responses_event: None,
        },
    ];
    let s = futures_util::stream::iter(chunks);
    let boxed: Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>> = Box::pin(s);
    let mut out = chat_chunks_to_responses_stream(boxed, "gpt-5");
    let mut events: Vec<String> = Vec::new();
    while let Some(line) = out.next().await {
        events.push(line);
    }
    assert_eq!(events.len(), 8, "got: {events:?}");
    assert!(events[0].starts_with("event: response.created\ndata: "));
    assert!(events[1].starts_with("event: response.output_item.added\ndata: "));
    assert!(
        events[2].contains("event: response.output_text.delta"),
        "{}",
        events[2]
    );
    assert!(events[2].contains("\"delta\":\"hel\""));
    assert!(events[3].contains("\"delta\":\"lo\""));
    assert!(events[4].starts_with("event: response.output_text.done\ndata: "));
    assert!(events[5].starts_with("event: response.output_item.done\ndata: "));
    assert!(events[6].starts_with("event: response.completed\ndata: "));
    assert_eq!(events[7], crate::proxy::responses::STREAM_DONE);
    // The completed envelope must carry the upstream usage, not zeros.
    let completed: Value =
        serde_json::from_str(events[6].split("data: ").nth(1).unwrap().trim_end()).unwrap();
    assert_eq!(completed["response"]["status"], "completed");
    assert_eq!(completed["response"]["output"][0]["type"], "message");
}

/// Streaming tool calls must surface as function_call items with
/// argument deltas — Codex CLI executes tools from exactly these
/// events, so dropping the chat `tool_calls` deltas (the previous
/// behavior) made every streamed tool turn unusable.
#[tokio::test]
async fn responses_stream_relays_tool_call_deltas_and_usage() {
    let chunks = vec![
        OAIStreamChunk {
            id: "x".into(),
            object: "chat.completion.chunk".into(),
            created: 1,
            model: "gpt-5".into(),
            choices: vec![StreamChoice {
                index: 0,
                delta: StreamDelta {
                    role: Some("assistant".into()),
                    content: None,
                    tool_calls: Some(vec![serde_json::json!({
                        "index": 0,
                        "id": "call_1",
                        "type": "function",
                        "function": { "name": "shell", "arguments": "" },
                    })]),
                    reasoning: None,
                    extra: Default::default(),
                },
                finish_reason: None,
                extra: empty_extra(),
            }],
            extra: empty_extra(),
            source_anthropic_event: None,
            source_gemini_event: None,
            source_responses_event: None,
        },
        OAIStreamChunk {
            id: "x".into(),
            object: "chat.completion.chunk".into(),
            created: 1,
            model: "gpt-5".into(),
            choices: vec![StreamChoice {
                index: 0,
                delta: StreamDelta {
                    role: None,
                    content: None,
                    tool_calls: Some(vec![serde_json::json!({
                        "index": 0,
                        "function": { "arguments": "{\"cmd\":\"ls\"}" },
                    })]),
                    reasoning: None,
                    extra: Default::default(),
                },
                finish_reason: Some("tool_calls".into()),
                extra: empty_extra(),
            }],
            extra: [(
                "usage".to_string(),
                serde_json::json!({
                    "prompt_tokens": 9,
                    "completion_tokens": 4,
                    "total_tokens": 13,
                }),
            )]
            .into_iter()
            .collect(),
            source_anthropic_event: None,
            source_gemini_event: None,
            source_responses_event: None,
        },
    ];
    let boxed: Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>> =
        Box::pin(futures_util::stream::iter(chunks));
    let mut out = chat_chunks_to_responses_stream(boxed, "gpt-5");
    let mut events: Vec<String> = Vec::new();
    while let Some(line) = out.next().await {
        events.push(line);
    }
    let types: Vec<&str> = events
        .iter()
        .filter_map(|line| line.strip_prefix("event: "))
        .filter_map(|rest| rest.split('\n').next())
        .collect();
    assert!(types.contains(&"response.output_item.added"), "{types:?}");
    assert!(
        types.contains(&"response.function_call_arguments.delta"),
        "{types:?}"
    );
    assert!(
        types.contains(&"response.function_call_arguments.done"),
        "{types:?}"
    );

    let added = events
        .iter()
        .find(|line| {
            line.contains("response.output_item.added") && line.contains("function_call")
        })
        .expect("function_call output_item.added event");
    assert!(added.contains("\"type\":\"function_call\""), "{added}");
    assert!(added.contains("\"name\":\"shell\""), "{added}");

    let args_delta = events
        .iter()
        .find(|line| line.contains("response.function_call_arguments.delta"))
        .unwrap();
    assert!(
        args_delta.contains("\"delta\":\"{\\\"cmd\\\""),
        "{args_delta}"
    );

    let completed_line = events
        .iter()
        .find(|line| line.starts_with("event: response.completed\n"))
        .expect("completed event");
    let completed: Value =
        serde_json::from_str(completed_line.split("data: ").nth(1).unwrap().trim_end())
            .unwrap();
    assert_eq!(completed["response"]["usage"]["input_tokens"], 9);
    assert_eq!(completed["response"]["usage"]["output_tokens"], 4);
    let output = completed["response"]["output"].as_array().unwrap();
    assert_eq!(
        output.len(),
        1,
        "tool-only turn has no phantom message item"
    );
    assert!(
        output
            .iter()
            .any(|item| item["type"] == "function_call"
                && item["arguments"] == "{\"cmd\":\"ls\"}"),
        "final output carries the full function call: {output:?}"
    );
}

/// An in-stream error chunk must become `response.failed` with the
/// failed status envelope (not a chat-completions style `error` event).
#[tokio::test]
async fn responses_stream_error_becomes_response_failed() {
    let error_chunk =
        crate::proxy::canonical::stream_error_chunk("m", 502, "proxy_stream_error");
    let boxed: Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>> =
        Box::pin(futures_util::stream::iter(vec![error_chunk]));
    let mut out = chat_chunks_to_responses_stream(boxed, "gpt-5");
    let mut events: Vec<String> = Vec::new();
    while let Some(line) = out.next().await {
        events.push(line);
    }
    assert_eq!(events.len(), 1, "failed event is terminal, got: {events:?}");
    assert!(
        events[0].starts_with("event: response.failed\ndata: "),
        "{}",
        events[0]
    );
    let failed: Value =
        serde_json::from_str(events[0].split("data: ").nth(1).unwrap().trim_end()).unwrap();
    assert_eq!(failed["response"]["status"], "failed");
    assert_eq!(failed["response"]["error"]["code"], "proxy_stream_error");
    assert!(
        !events[0].contains("[DONE]"),
        "no success terminator after failure"
    );
}

#[test]
fn responses_request_drop_previous_response_id() {
    let req = ResponsesRequest {
        model: "gpt-5".into(),
        input: serde_json::json!("hi"),
        instructions: None,
        stream: false,
        temperature: None,
        max_output_tokens: None,
        tools: None,
        metadata: None,
        previous_response_id: Some("resp_xxx".into()),
        extra: empty_extra(),
    };
    let chat = responses_to_chat(&req);
    assert_eq!(chat.model, "gpt-5");
    assert_eq!(chat.messages.len(), 1);
}

#[test]
fn responses_request_drops_non_text_parts() {
    // Test that input_content_to_chat drops non-text parts
    let req = ResponsesRequest {
        model: "gpt-5".into(),
        input: serde_json::json!([
            { "role": "user", "content": [
                { "type": "input_image", "image_url": "https://example.com/image.png" },
                { "type": "input_text", "text": "describe this" },
            ]},
        ]),
        instructions: None,
        stream: false,
        temperature: None,
        max_output_tokens: None,
        tools: None,
        metadata: None,
        previous_response_id: None,
        extra: empty_extra(),
    };
    let chat = responses_to_chat(&req);
    assert_eq!(chat.messages.len(), 1);
    let content = &chat.messages[0].content;
    // The image part should be dropped, only text remains
    let arr = content.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["type"], "text");
    assert_eq!(arr[0]["text"], "describe this");
}

#[test]
fn responses_request_image_only_collapses_to_empty_string() {
    // Image-only content (no text parts to keep) used to round-trip as
    // an empty `content` array, which strict OpenAI-compatible upstreams
    // reject with 400. Collapse to an empty string instead.
    let req = ResponsesRequest {
        model: "gpt-5".into(),
        input: serde_json::json!([
            { "role": "user", "content": [
                { "type": "input_image", "image_url": "https://example.com/image.png" },
            ]},
        ]),
        instructions: None,
        stream: false,
        temperature: None,
        max_output_tokens: None,
        tools: None,
        metadata: None,
        previous_response_id: None,
        extra: empty_extra(),
    };
    let chat = responses_to_chat(&req);
    assert_eq!(chat.messages.len(), 1);
    assert_eq!(chat.messages[0].content, serde_json::json!(""));
}

#[test]
fn responses_request_missing_content_becomes_empty_string() {
    // Items with no `content` key used to flatten into null content,
    // which upstreams reject. Empty string is always valid.
    let req = ResponsesRequest {
        model: "gpt-5".into(),
        input: serde_json::json!([
            { "role": "user" },
        ]),
        instructions: None,
        stream: false,
        temperature: None,
        max_output_tokens: None,
        tools: None,
        metadata: None,
        previous_response_id: None,
        extra: empty_extra(),
    };
    let chat = responses_to_chat(&req);
    assert_eq!(chat.messages.len(), 1);
    assert_eq!(chat.messages[0].content, serde_json::json!(""));
}

#[test]
fn responses_request_image_only_serializes_as_empty_string() {
    let req = ResponsesRequest {
        model: "gpt-5".into(),
        input: serde_json::json!([
            { "role": "user", "content": [
                { "type": "input_image", "image_url": "https://example.com/image.png" },
            ]},
        ]),
        instructions: None,
        stream: false,
        temperature: None,
        max_output_tokens: None,
        tools: None,
        metadata: None,
        previous_response_id: None,
        extra: empty_extra(),
    };
    let chat = responses_to_chat(&req);
    let serialized = serde_json::to_value(&chat).unwrap();
    let messages = serialized["messages"].as_array().unwrap();
    assert_eq!(messages[0]["content"], serde_json::json!(""));
}

#[test]
fn responses_tools_flat_get_wrapped_for_chat_completions() {
    // The Responses API sends tools in a flat shape; Chat Completions
    // requires the function payload nested under `function`. Without
    // this conversion every Codex request is a 400 upstream.
    let req = ResponsesRequest {
        model: "gpt-5".into(),
        input: serde_json::json!("hi"),
        instructions: None,
        stream: false,
        temperature: None,
        max_output_tokens: None,
        tools: Some(serde_json::json!([
            {
                "type": "function",
                "name": "shell",
                "description": "run shell",
                "parameters": { "type": "object", "properties": { "cmd": {"type": "string"} } },
                "strict": false
            },
            {
                "type": "function",
                "name": "apply_patch",
                "description": "apply a patch",
                "strict": false
            }
        ])),
        metadata: None,
        previous_response_id: None,
        extra: empty_extra(),
    };
    let chat = responses_to_chat(&req);
    let tools = chat.tools.expect("tools must survive conversion");
    assert_eq!(tools.len(), 2);
    let shell = &tools[0];
    assert_eq!(shell["type"], "function");
    assert_eq!(shell["function"]["name"], "shell");
    assert_eq!(shell["function"]["description"], "run shell");
    assert_eq!(shell["function"]["strict"], false);
    assert_eq!(shell["function"]["parameters"]["type"], "object");
    let patch = &tools[1];
    assert_eq!(patch["function"]["name"], "apply_patch");
    // apply_patch ships without parameters; default an empty schema so
    // strict upstreams don't reject the request.
    assert_eq!(patch["function"]["parameters"]["type"], "object");
    assert_eq!(
        patch["function"]["parameters"]["properties"],
        serde_json::json!({})
    );
}

#[test]
fn responses_tools_non_function_types_are_dropped() {
    let req = ResponsesRequest {
        model: "gpt-5".into(),
        input: serde_json::json!("hi"),
        instructions: None,
        stream: false,
        temperature: None,
        max_output_tokens: None,
        tools: Some(serde_json::json!([
            { "type": "web_search" },
            { "type": "function", "name": "shell", "parameters": {} },
        ])),
        metadata: None,
        previous_response_id: None,
        extra: empty_extra(),
    };
    let chat = responses_to_chat(&req);
    let tools = chat.tools.expect("function tool should survive");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["function"]["name"], "shell");
}

#[test]
fn responses_tools_already_in_chat_shape_pass_through() {
    let req = ResponsesRequest {
        model: "gpt-5".into(),
        input: serde_json::json!("hi"),
        instructions: None,
        stream: false,
        temperature: None,
        max_output_tokens: None,
        tools: Some(serde_json::json!([
            { "type": "function", "function": { "name": "shell", "parameters": {} } }
        ])),
        metadata: None,
        previous_response_id: None,
        extra: empty_extra(),
    };
    let chat = responses_to_chat(&req);
    let tools = chat.tools.expect("already-shaped tool must survive");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["function"]["name"], "shell");
}

#[test]
fn responses_tools_all_unusable_becomes_none() {
    // Don't emit `"tools": []` on the wire — that is a guaranteed 400
    // from strict upstreams.
    let req = ResponsesRequest {
        model: "gpt-5".into(),
        input: serde_json::json!("hi"),
        instructions: None,
        stream: false,
        temperature: None,
        max_output_tokens: None,
        tools: Some(serde_json::json!([
            { "type": "web_search" },
            { "type": "local_shell" }
        ])),
        metadata: None,
        previous_response_id: None,
        extra: empty_extra(),
    };
    let chat = responses_to_chat(&req);
    assert!(chat.tools.is_none());
}

#[test]
fn responses_function_call_items_become_assistant_tool_calls() {
    let req = ResponsesRequest {
        model: "gpt-5".into(),
        input: serde_json::json!([
            { "role": "user", "content": "list files" },
            { "type": "function_call",
              "call_id": "call_1",
              "name": "shell",
              "arguments": "{\"cmd\":\"ls\"}" },
            { "type": "function_call_output",
              "call_id": "call_1",
              "output": "file1.txt\nfile2.txt" },
            { "type": "reasoning",
              "id": "r_1",
              "summary": [{ "type": "summary_text", "text": "thinking" }] }
        ]),
        instructions: None,
        stream: false,
        temperature: None,
        max_output_tokens: None,
        tools: None,
        metadata: None,
        previous_response_id: None,
        extra: empty_extra(),
    };
    let chat = responses_to_chat(&req);
    // user + assistant(tool_calls) + tool — reasoning is dropped.
    assert_eq!(chat.messages.len(), 3);
    assert_eq!(chat.messages[0].role, "user");
    assert_eq!(chat.messages[1].role, "assistant");
    assert!(
        chat.messages[1].content.is_null(),
        "assistant tool_calls message content must be null, got {:?}",
        chat.messages[1].content
    );
    let tc = chat.messages[1]
        .tool_calls
        .as_ref()
        .expect("assistant tool_calls must be set");
    assert_eq!(tc.len(), 1);
    assert_eq!(tc[0]["id"], "call_1");
    assert_eq!(tc[0]["type"], "function");
    assert_eq!(tc[0]["function"]["name"], "shell");
    assert_eq!(tc[0]["function"]["arguments"], "{\"cmd\":\"ls\"}");
    assert_eq!(chat.messages[2].role, "tool");
    assert_eq!(chat.messages[2].tool_call_id.as_deref(), Some("call_1"));
    assert_eq!(
        chat.messages[2].content,
        serde_json::json!("file1.txt\nfile2.txt")
    );
}

#[test]
fn responses_developer_role_maps_to_system() {
    let req = ResponsesRequest {
        model: "gpt-5".into(),
        input: serde_json::json!([
            { "role": "developer", "content": "be terse" },
            { "role": "developer", "content": [
                { "type": "input_text", "text": "be terse with parts" }
            ]},
        ]),
        instructions: None,
        stream: false,
        temperature: None,
        max_output_tokens: None,
        tools: None,
        metadata: None,
        previous_response_id: None,
        extra: empty_extra(),
    };
    let chat = responses_to_chat(&req);
    assert_eq!(chat.messages.len(), 2);
    assert_eq!(chat.messages[0].role, "system");
    assert_eq!(chat.messages[0].content, serde_json::json!("be terse"));
    assert_eq!(chat.messages[1].role, "system");
    let arr = chat.messages[1].content.as_array().unwrap();
    assert_eq!(arr[0]["text"], "be terse with parts");
}

#[test]
fn responses_tool_choice_string_passes_through() {
    let mut extra = empty_extra();
    extra.insert("tool_choice".into(), Value::String("auto".into()));
    let req = ResponsesRequest {
        model: "gpt-5".into(),
        input: serde_json::json!("hi"),
        instructions: None,
        stream: false,
        temperature: None,
        max_output_tokens: None,
        tools: None,
        metadata: None,
        previous_response_id: None,
        extra,
    };
    let chat = responses_to_chat(&req);
    assert_eq!(chat.tool_choice, Some(serde_json::json!("auto")));
}

#[test]
fn responses_tool_choice_function_object_is_rewrapped() {
    let mut extra = empty_extra();
    extra.insert(
        "tool_choice".into(),
        serde_json::json!({ "type": "function", "name": "shell" }),
    );
    let req = ResponsesRequest {
        model: "gpt-5".into(),
        input: serde_json::json!("hi"),
        instructions: None,
        stream: false,
        temperature: None,
        max_output_tokens: None,
        tools: None,
        metadata: None,
        previous_response_id: None,
        extra,
    };
    let chat = responses_to_chat(&req);
    assert_eq!(
        chat.tool_choice,
        Some(serde_json::json!({ "type": "function", "function": { "name": "shell" } }))
    );
}

#[test]
fn chat_response_with_tool_calls_emits_function_call_items() {
    let chat = OAIChatResponse {
        id: "chatcmpl-1".into(),
        object: "chat.completion".into(),
        created: 1_700_000_000,
        model: "gpt-5".into(),
        choices: vec![OAIChoice {
            index: 0,
            message: OAIMessage {
                role: "assistant".into(),
                content: None,
                tool_calls: Some(vec![serde_json::json!({
                    "id": "call_1",
                    "type": "function",
                    "function": { "name": "shell", "arguments": "{\"cmd\":\"ls\"}" }
                })]),
                refusal: None,
                extra: empty_extra(),
            },
            finish_reason: Some("tool_calls".into()),
            extra: Default::default(),
        }],
        usage: None,
        source_anthropic_response: None,
        source_gemini_response: None,
        source_responses_response: None,
        extra: Default::default(),
    };
    let r = chat_to_responses(&chat, "gpt-5");
    assert_eq!(r["object"], "response");
    // A tool-only assistant turn must not gain a phantom empty message.
    let output = r["output"].as_array().unwrap();
    assert_eq!(output.len(), 1);
    assert_eq!(output[0]["type"], "function_call");
    assert_eq!(output[0]["call_id"], "call_1");
    assert_eq!(output[0]["name"], "shell");
    assert_eq!(output[0]["arguments"], "{\"cmd\":\"ls\"}");
    assert_eq!(output[0]["status"], "completed");
}

#[tokio::test]
async fn responses_stream_with_tool_call_deltas_emits_function_call_events() {
    let chunks = vec![
        OAIStreamChunk {
            id: "x".into(),
            object: "chat.completion.chunk".into(),
            created: 1,
            model: "gpt-5".into(),
            choices: vec![StreamChoice {
                index: 0,
                delta: StreamDelta {
                    role: Some("assistant".into()),
                    content: None,
                    tool_calls: Some(vec![serde_json::json!({
                        "index": 0,
                        "id": "call_1",
                        "type": "function",
                        "function": { "name": "shell", "arguments": "{\"cmd\":" }
                    })]),
                    reasoning: None,
                    extra: Default::default(),
                },
                finish_reason: None,
                extra: empty_extra(),
            }],
            extra: empty_extra(),
            source_anthropic_event: None,
            source_gemini_event: None,
            source_responses_event: None,
        },
        OAIStreamChunk {
            id: "x".into(),
            object: "chat.completion.chunk".into(),
            created: 1,
            model: "gpt-5".into(),
            choices: vec![StreamChoice {
                index: 0,
                delta: StreamDelta {
                    role: None,
                    content: None,
                    tool_calls: Some(vec![serde_json::json!({
                        "index": 0,
                        "function": { "arguments": "\"ls\"}" }
                    })]),
                    reasoning: None,
                    extra: Default::default(),
                },
                finish_reason: Some("tool_calls".into()),
                extra: empty_extra(),
            }],
            extra: empty_extra(),
            source_anthropic_event: None,
            source_gemini_event: None,
            source_responses_event: None,
        },
    ];
    let s = futures_util::stream::iter(chunks);
    let boxed: Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>> = Box::pin(s);
    let mut out = chat_chunks_to_responses_stream(boxed, "gpt-5");
    let mut events: Vec<String> = Vec::new();
    while let Some(line) = out.next().await {
        events.push(line);
    }
    // A tool-only turn opens only the function_call item: created,
    // item added, argument deltas, arguments done, item done,
    // response.completed, [DONE].
    assert_eq!(events.len(), 8, "got: {events:?}");
    assert!(events[0].starts_with("event: response.created\ndata: "));
    assert!(events[1].starts_with("event: response.output_item.added\ndata: "));
    assert!(events[1].contains("\"output_index\":0"));
    assert!(events[1].contains("\"type\":\"function_call\""));
    assert!(events[1].contains("\"call_id\":\"call_1\""));
    assert!(events[2].starts_with("event: response.function_call_arguments.delta\ndata: "));
    assert!(events[3].starts_with("event: response.function_call_arguments.delta\ndata: "));
    assert!(events[4].starts_with("event: response.function_call_arguments.done\ndata: "));
    assert!(events[4].contains("{\\\"cmd\\\":\\\"ls\\\"}"));
    assert!(events[5].starts_with("event: response.output_item.done\ndata: "));
    assert!(events[5].contains("\"output_index\":0"));
    assert!(events[5].contains("\"type\":\"function_call\""));
    // event 6: response.completed containing the function_call item.
    assert!(events[6].starts_with("event: response.completed\ndata: "));
    let completed: serde_json::Value = serde_json::from_str(
        events[6]
            .trim_start_matches("event: response.completed\ndata: ")
            .trim(),
    )
    .unwrap();
    let output = completed["response"]["output"].as_array().unwrap();
    assert_eq!(output.len(), 1);
    assert_eq!(output[0]["type"], "function_call");
    assert_eq!(output[0]["call_id"], "call_1");
    assert_eq!(output[0]["arguments"], "{\"cmd\":\"ls\"}");
    assert_eq!(events[7], crate::proxy::responses::STREAM_DONE);
}

fn native_chunk(event: crate::proxy::responses::ResponsesSseEvent) -> OAIStreamChunk {
    OAIStreamChunk {
        id: "x".into(),
        object: "chat.completion.chunk".into(),
        created: 1,
        model: "gpt-5".into(),
        choices: vec![],
        extra: empty_extra(),
        source_anthropic_event: None,
        source_gemini_event: None,
        source_responses_event: Some(event),
    }
}

/// Native Responses relay streams must end with the `data: [DONE]` success
/// terminator after a successful terminal frame — the surface contract
/// states every stream is terminated by it (P4 regression).
#[tokio::test]
async fn native_responses_stream_appends_done_after_completed() {
    let created = crate::proxy::responses::ResponsesSseEvent {
        event: Some("response.created".into()),
        id: Some("evt_1".into()),
        retry: None,
        data: r#"{"type":"response.created","response":{"id":"resp_1"}}"#.into(),
    };
    let completed = crate::proxy::responses::ResponsesSseEvent {
        event: Some("response.completed".into()),
        id: Some("evt_2".into()),
        retry: None,
        data: r#"{"type":"response.completed","response":{"id":"resp_1","status":"completed"}}"#
            .into(),
    };
    let s = futures_util::stream::iter(vec![
        native_chunk(created),
        native_chunk(completed),
    ]);
    let boxed: Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>> = Box::pin(s);
    let mut out = responses_stream_to_sse(boxed, "gpt-5");
    let mut events: Vec<String> = Vec::new();
    while let Some(line) = out.next().await {
        events.push(line);
    }
    assert_eq!(events.len(), 3, "got: {events:?}");
    assert!(
        events[0].starts_with("event: response.created\nid: evt_1\ndata: "),
        "got: {:?}",
        events[0]
    );
    assert!(
        events[1].starts_with("event: response.completed\nid: evt_2\ndata: "),
        "got: {:?}",
        events[1]
    );
    assert_eq!(events[2], crate::proxy::responses::STREAM_DONE);
}

/// Failure-class terminal frames (failed, cancelled, error) end the stream
/// without the success terminator — same contract as the synthesized path
/// (P4 regression).
#[tokio::test]
async fn native_responses_stream_omits_done_after_failure() {
    for (event_name, payload) in [
        (
            "response.failed",
            r#"{"type":"response.failed","response":{"id":"resp_1","status":"failed"}}"#,
        ),
        (
            "response.cancelled",
            r#"{"type":"response.cancelled","response":{"id":"resp_1","status":"cancelled"}}"#,
        ),
    ] {
        let created = crate::proxy::responses::ResponsesSseEvent {
            event: Some("response.created".into()),
            id: Some("evt_1".into()),
            retry: None,
            data: r#"{"type":"response.created","response":{"id":"resp_1"}}"#.into(),
        };
        let terminal = crate::proxy::responses::ResponsesSseEvent {
            event: Some(event_name.into()),
            id: Some("evt_2".into()),
            retry: None,
            data: payload.into(),
        };
        let s = futures_util::stream::iter(vec![native_chunk(created), native_chunk(terminal)]);
        let boxed: Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>> = Box::pin(s);
        let mut out = responses_stream_to_sse(boxed, "gpt-5");
        let mut events: Vec<String> = Vec::new();
        while let Some(line) = out.next().await {
            events.push(line);
        }
        assert_eq!(events.len(), 2, "got: {events:?}");
        assert!(
            events[1].starts_with(&format!("event: {event_name}\nid: evt_2\ndata: ")),
            "got: {:?}",
            events[1]
        );
        assert!(events.iter().all(|frame| frame != crate::proxy::responses::STREAM_DONE));
    }
}
