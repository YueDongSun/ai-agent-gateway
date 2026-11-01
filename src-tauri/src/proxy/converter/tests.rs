use crate::proxy::canonical::{
    ChatCompletionRequest, ChatMessage, OAIChatResponse, OAIChoice, OAIMessage, OAIStreamChunk,
    StreamChoice, StreamDelta, Usage,
};
use futures_core::stream::Stream;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::pin::Pin;


use super::*;
use futures_util::stream;

pub fn empty_extra() -> HashMap<String, Value> {
    HashMap::new()
}

fn make_user_msg(content: &str) -> ChatMessage {
    ChatMessage {
        role: "user".into(),
        content: Value::String(content.into()),
        tool_calls: None,
        tool_call_id: None,
        extra: empty_extra(),
    }
}

#[test]
fn roundtrip_anthropic_req_basic() {
    let oai = ChatCompletionRequest {
        model: "claude-3-5-sonnet".into(),
        messages: vec![
            ChatMessage {
                role: "system".into(),
                content: Value::String("be brief".into()),
                tool_calls: None,
                tool_call_id: None,
                extra: empty_extra(),
            },
            make_user_msg("hi"),
        ],
        stream: false,
        temperature: Some(0.5),
        max_tokens: Some(100),
        tools: None,
        tool_choice: None,
        extra: empty_extra(),
        source_anthropic_request: None,
        source_gemini_request: None,
        source_responses_request: None,
        request_id: None,
        client_credential: None,
    };
    let ant = oai_req_to_anthropic_req(&oai).unwrap();
    assert_eq!(ant.max_tokens, 100);
    assert_eq!(ant.temperature, Some(0.5));
    assert_eq!(ant.system, Some(Value::String("be brief".into())));
    assert_eq!(ant.messages.len(), 1);
    assert_eq!(ant.messages[0].role, "user");
}

#[test]
fn stop_reason_mapping() {
    assert_eq!(stop_to_anthropic("stop"), "end_turn");
    assert_eq!(stop_from_anthropic("end_turn"), "stop");
    assert_eq!(stop_to_anthropic("length"), "max_tokens");
    assert_eq!(stop_from_anthropic("max_tokens"), "length");
    assert_eq!(stop_to_anthropic("tool_calls"), "tool_use");
    assert_eq!(stop_from_anthropic("tool_use"), "tool_calls");
}

#[test]
fn oai_to_anthropic_resp_text() {
    let oai = OAIChatResponse {
        id: "chatcmpl-x".into(),
        object: "chat.completion".into(),
        created: 1,
        model: "m".into(),
        choices: vec![OAIChoice {
            index: 0,
            message: OAIMessage {
                role: "assistant".into(),
                content: Some("hello".into()),
                tool_calls: None,
                refusal: None,
                extra: empty_extra(),
            },
            finish_reason: Some("stop".into()),
            extra: Default::default(),
        }],
        usage: Some(Usage {
            prompt_tokens: 5,
            completion_tokens: 5,
            total_tokens: 10,
            extra: Default::default(),
        }),
        source_anthropic_response: None,
        source_gemini_response: None,
        source_responses_response: None,
        extra: Default::default(),
    };
    let ant = oai_resp_to_anthropic_resp(&oai, "claude");
    assert_eq!(ant.content[0].text.as_deref(), Some("hello"));
    assert_eq!(ant.stop_reason.as_deref(), Some("end_turn"));
    assert_eq!(ant.usage.input_tokens, 5);
}

// ----- tool-aware tests (Phase 21) -----

#[test]
fn roundtrip_anthropic_req_with_tools() {
    // Build an Anthropic request with one tool_use in an assistant
    // message and one tool_result in a user message; convert it to
    // canonical OpenAI; convert back; verify the tool calls survive.
    let mut tool_use_extra: HashMap<String, Value> = HashMap::new();
    tool_use_extra.insert("id".into(), json!("toolu_01"));
    tool_use_extra.insert("name".into(), json!("get_weather"));
    tool_use_extra.insert("input".into(), json!({"city": "SF"}));
    let ant_req = AnthropicRequest {
        model: "claude-opus-4-8".into(),
        max_tokens: 1024,
        system: Some(json!("you can call tools")),
        messages: vec![
            AnthropicMessage {
                role: "assistant".into(),
                content: json!([
                    { "type": "text", "text": "calling weather" },
                    { "type": "tool_use", "id": "toolu_01",
                      "name": "get_weather", "input": {"city": "SF"} },
                ]),
            },
            AnthropicMessage {
                role: "user".into(),
                content: json!([
                    { "type": "tool_result",
                      "tool_use_id": "toolu_01",
                      "content": "72F sunny" },
                ]),
            },
        ],
        temperature: None,
        stream: None,
        stop_sequences: None,
        extra: {
            let mut e = HashMap::new();
            e.insert(
                "tools".into(),
                json!([
                    { "name": "get_weather",
                      "description": "Get the weather",
                      "input_schema": { "type": "object",
                                         "properties": { "city": { "type": "string" } } } }
                ]),
            );
            e.insert(
                "tool_choice".into(),
                json!({"type": "tool", "name": "get_weather"}),
            );
            e
        },
    };
    let oai = anthropic_req_to_oai_req(&ant_req);
    assert_eq!(oai.model, "claude-opus-4-8");
    assert!(oai.tools.is_some(), "tools should propagate via extra");
    assert!(oai.tool_choice.is_some(), "tool_choice should propagate");
    // Three messages: system, assistant (with tool_calls), tool (tool_call_id).
    // The original Anthropic user message that contained only a
    // tool_result block was elided (its content moved into the
    // separate role:tool message).
    assert_eq!(oai.messages.len(), 3);
    assert_eq!(oai.messages[0].role, "system");
    let asst = &oai.messages[1];
    assert_eq!(asst.role, "assistant");
    let tcs = asst.tool_calls.as_ref().expect("assistant tool_calls");
    assert_eq!(tcs.len(), 1);
    assert_eq!(tcs[0]["id"], "toolu_01");
    assert_eq!(tcs[0]["function"]["name"], "get_weather");
    // tool_result block should have been promoted to a separate
    // role:tool message.
    let tool_msg = &oai.messages[2];
    assert_eq!(tool_msg.role, "tool");
    assert_eq!(tool_msg.tool_call_id.as_deref(), Some("toolu_01"));
    assert_eq!(tool_msg.content, json!("72F sunny"));

    // Round-trip back to Anthropic; verify the tool_use block survives.
    let ant_back = oai_req_to_anthropic_req(&oai).unwrap();
    assert_eq!(ant_back.messages.len(), 2);
    let asst_back = &ant_back.messages[0];
    let arr = asst_back
        .content
        .as_array()
        .expect("assistant content array");
    let tool_use_block = arr
        .iter()
        .find(|b| b.get("type").and_then(|v| v.as_str()) == Some("tool_use"))
        .expect("tool_use block present after round-trip");
    assert_eq!(tool_use_block["id"], "toolu_01");
    assert_eq!(tool_use_block["name"], "get_weather");
    assert_eq!(tool_use_block["input"]["city"], "SF");
    let tool_result_msg = &ant_back.messages[1];
    let arr2 = tool_result_msg.content.as_array().unwrap();
    assert_eq!(arr2[0]["type"], "tool_result");
    assert_eq!(arr2[0]["tool_use_id"], "toolu_01");
    assert_eq!(arr2[0]["content"], "72F sunny");
}

#[test]
fn roundtrip_anthropic_resp_with_tool_use() {
    // Build an Anthropic response with one tool_use block; verify it
    // becomes an OpenAI tool_call and the finish_reason maps to
    // tool_calls.
    let mut tool_use = HashMap::new();
    tool_use.insert("id".into(), json!("toolu_42"));
    tool_use.insert("name".into(), json!("search"));
    tool_use.insert("input".into(), json!({"q": "rust"}));
    let ant = AnthropicResponse {
        id: "msg_x".into(),
        resp_type: "message".into(),
        role: "assistant".into(),
        model: "claude".into(),
        content: vec![
            AnthropicContentBlock {
                block_type: "text".into(),
                text: Some("let me search".into()),
                extra: empty_extra(),
            },
            AnthropicContentBlock {
                block_type: "tool_use".into(),
                text: None,
                extra: tool_use,
            },
        ],
        stop_reason: Some("tool_use".into()),
        stop_sequence: None,
        usage: AnthropicUsage {
            input_tokens: 10,
            output_tokens: 5,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            extra: Default::default(),
        },
        extra: Default::default(),
    };
    let oai = anthropic_resp_to_oai_resp(&ant, "claude");
    let choice = &oai.choices[0];
    assert_eq!(choice.message.content.as_deref(), Some("let me search"));
    let tcs = choice
        .message
        .tool_calls
        .as_ref()
        .expect("tool_calls present");
    assert_eq!(tcs.len(), 1);
    assert_eq!(tcs[0]["id"], "toolu_42");
    assert_eq!(tcs[0]["function"]["name"], "search");
    // canonical_json_string keeps the JSON canonical (no spaces), so
    // we compare the parsed value rather than the literal string.
    let args: Value =
        serde_json::from_str(tcs[0]["function"]["arguments"].as_str().unwrap()).unwrap();
    assert_eq!(args["q"], "rust");
    assert_eq!(choice.finish_reason.as_deref(), Some("tool_calls"));
}

#[test]
fn tool_choice_mapping_round_trip() {
    // Anthropic {"type":"tool","name":"foo"} -> OAI {type:function, ...} ->
    // back to Anthropic {"type":"tool","name":"foo"}.
    let ant_in = json!({"type": "tool", "name": "foo"});
    let oai = tool_choice_anthropic_to_oai(&ant_in);
    assert_eq!(oai["type"], "function");
    assert_eq!(oai["function"]["name"], "foo");
    let ant_back = tool_choice_oai_to_anthropic(&oai);
    assert_eq!(ant_back["type"], "tool");
    assert_eq!(ant_back["name"], "foo");

    // "any" <-> "required"
    assert_eq!(
        tool_choice_oai_to_anthropic(&json!("required")),
        json!({"type": "any"})
    );
    assert_eq!(tool_choice_anthropic_to_oai(&json!("any")), "required");
}

/// Parse an Anthropic SSE event payload (data string after `data:` strip)
/// by extracting just the JSON. We feed JSON strings into the stream
/// directly — the converter parses them with serde_json::from_str.
fn sse_chunk_from_data_json(json_str: &str) -> String {
    json_str.to_string()
}

#[tokio::test]
async fn streaming_oai_tool_calls_to_anthropic() {
    // Feed three OAI chunks: (1) role + tool_calls index 0 with id+name,
    // (2) tool_calls index 0 with arguments fragment, (3) finish_reason
    // tool_calls. Expect: message_start, content_block_start tool_use,
    // input_json_delta, content_block_stop, message_delta, message_stop.
    let chunks = vec![
        OAIStreamChunk {
            id: "x".into(),
            object: "chat.completion.chunk".into(),
            created: 1,
            model: "m".into(),
            choices: vec![StreamChoice {
                index: 0,
                delta: StreamDelta {
                    role: Some("assistant".into()),
                    content: None,
                    tool_calls: Some(vec![json!({
                        "index": 0,
                        "id": "toolu_1",
                        "type": "function",
                        "function": { "name": "lookup" },
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
            model: "m".into(),
            choices: vec![StreamChoice {
                index: 0,
                delta: StreamDelta {
                    role: None,
                    content: None,
                    tool_calls: Some(vec![json!({
                        "index": 0,
                        "function": { "arguments": "{\"q\":" },
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
            model: "m".into(),
            choices: vec![StreamChoice {
                index: 0,
                delta: StreamDelta {
                    role: None,
                    content: None,
                    tool_calls: Some(vec![json!({
                        "index": 0,
                        "function": { "arguments": "\"rust\"}" },
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
    let s = stream::iter(chunks);
    let boxed: Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>> = Box::pin(s);
    let mut out = oai_chunks_to_anthropic_stream(boxed, "claude".to_string());
    let mut events: Vec<String> = Vec::new();
    while let Some(line) = out.next().await {
        events.push(line);
    }
    // Expect (at minimum) these event types in order:
    let types: Vec<&str> = events
        .iter()
        .filter_map(|s| {
            if s.starts_with("event: ") {
                Some(s.trim_start_matches("event: ").lines().next().unwrap_or(""))
            } else {
                None
            }
        })
        .collect();
    assert!(types.contains(&"message_start"), "events: {types:?}");
    let tool_start_idx = types
        .iter()
        .position(|t| *t == "content_block_start")
        .expect("content_block_start");
    let tool_data: &str = events[tool_start_idx]
        .split("data: ")
        .nth(1)
        .unwrap_or("")
        .split("\n\n")
        .next()
        .unwrap_or("");
    let tool_data: Value = serde_json::from_str(tool_data).unwrap();
    assert_eq!(tool_data["type"], "content_block_start");
    assert_eq!(tool_data["content_block"]["type"], "tool_use");
    assert_eq!(tool_data["content_block"]["id"], "toolu_1");
    assert_eq!(tool_data["content_block"]["name"], "lookup");
    // Two input_json_delta events from the two argument fragments.
    let deltas: Vec<&String> = events
        .iter()
        .filter(|s| s.starts_with("event: content_block_delta"))
        .collect();
    assert_eq!(deltas.len(), 2, "events: {events:?}");
    // Combined arguments should reassemble to {"q":"rust"}.
    let mut combined = String::new();
    for d in &deltas {
        let payload = d
            .split("data: ")
            .nth(1)
            .unwrap_or("")
            .split("\n\n")
            .next()
            .unwrap_or("");
        let v: Value = serde_json::from_str(payload).unwrap();
        if let Some(pj) = v["delta"]["partial_json"].as_str() {
            combined.push_str(pj);
        }
    }
    let parsed: Value = serde_json::from_str(&combined).unwrap();
    assert_eq!(parsed["q"], "rust");
    // message_delta + message_stop
    assert!(types.contains(&"message_delta"));
    assert!(types.contains(&"message_stop"));
    let md_data: Value = serde_json::from_str(
        events
            .iter()
            .find(|s| s.starts_with("event: message_delta"))
            .unwrap()
            .split("data: ")
            .nth(1)
            .unwrap()
            .split("\n\n")
            .next()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(md_data["delta"]["stop_reason"], "tool_use");
}

#[tokio::test]
async fn streaming_oai_text_then_tool_calls() {
    // OpenAI often emits text content first then a separate chunk
    // carrying the tool_calls (id, name, arguments). The text block
    // should close before the tool block opens.
    let chunks = vec![
        OAIStreamChunk {
            id: "x".into(),
            object: "chat.completion.chunk".into(),
            created: 1,
            model: "m".into(),
            choices: vec![StreamChoice {
                index: 0,
                delta: StreamDelta {
                    role: Some("assistant".into()),
                    content: Some("hold on".into()),
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
            model: "m".into(),
            choices: vec![StreamChoice {
                index: 0,
                delta: StreamDelta {
                    role: None,
                    content: None,
                    tool_calls: Some(vec![json!({
                        "index": 0,
                        "id": "toolu_1",
                        "type": "function",
                        "function": { "name": "lookup", "arguments": "{}" },
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
    let s = stream::iter(chunks);
    let boxed: Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>> = Box::pin(s);
    let mut out = oai_chunks_to_anthropic_stream(boxed, "claude".to_string());
    let mut events: Vec<String> = Vec::new();
    while let Some(line) = out.next().await {
        events.push(line);
    }
    let types: Vec<&str> = events
        .iter()
        .filter_map(|s| {
            if s.starts_with("event: ") {
                Some(s.trim_start_matches("event: ").lines().next().unwrap_or(""))
            } else {
                None
            }
        })
        .collect();
    // text block start, text delta, text block stop, tool block start.
    let text_start = types
        .iter()
        .position(|t| *t == "content_block_start")
        .expect("first content_block_start");
    // Make sure the text block opens first (text_delta appears before
    // the second content_block_start).
    let second_start = types
        .iter()
        .skip(text_start + 1)
        .position(|t| *t == "content_block_start")
        .map(|p| p + text_start + 1)
        .expect("second content_block_start");
    assert!(second_start > text_start);
    // Verify we got at least one text_delta and one input_json_delta.
    assert!(events
        .iter()
        .any(|s| s.starts_with("event: content_block_delta") && s.contains("text_delta")));
    assert!(
        events
            .iter()
            .any(|s| s.starts_with("event: content_block_delta")
                && s.contains("input_json_delta"))
    );
}

#[tokio::test]
async fn streaming_anthropic_tool_use_to_oai() {
    // Feed Anthropic SSE events: message_start, content_block_start
    // tool_use, content_block_delta input_json_delta, content_block_stop,
    // message_delta (tool_use), message_stop. Expect an OAI delta
    // carrying tool_calls[0] with id + name + arguments.
    let events = vec![
        json!({ "type": "message_start", "message": { "id": "m1", "type": "message", "role": "assistant", "model": "claude", "content": [], "usage": { "input_tokens": 0, "output_tokens": 0 } } }).to_string(),
        json!({ "type": "content_block_start", "index": 1,
               "content_block": { "type": "tool_use", "id": "toolu_1", "name": "lookup" } }).to_string(),
        json!({ "type": "content_block_delta", "index": 1,
               "delta": { "type": "input_json_delta", "partial_json": "{\"q\":" } }).to_string(),
        json!({ "type": "content_block_delta", "index": 1,
               "delta": { "type": "input_json_delta", "partial_json": "\"rust\"}" } }).to_string(),
        json!({ "type": "content_block_stop", "index": 1 }).to_string(),
        json!({ "type": "message_delta", "delta": { "stop_reason": "tool_use", "stop_sequence": null }, "usage": { "input_tokens": 1, "output_tokens": 2 } }).to_string(),
        json!({ "type": "message_stop" }).to_string(),
    ];
    let s = stream::iter(events.into_iter().map(|s| sse_chunk_from_data_json(&s)));
    let boxed: Pin<Box<dyn Stream<Item = String> + Send>> = Box::pin(s);
    let mut out = anthropic_stream_to_oai_chunks(boxed, "claude".to_string());
    let mut chunks = Vec::new();
    while let Some(c) = out.next().await {
        chunks.push(c);
    }
    // First chunk should be the role chunk.
    assert_eq!(
        chunks[0].choices[0].delta.role.as_deref(),
        Some("assistant")
    );
    // Find the chunk carrying tool_calls[0].id
    let id_chunk = chunks
        .iter()
        .find(|c| {
            c.choices.iter().any(|ch| {
                ch.delta
                    .tool_calls
                    .as_ref()
                    .map(|tcs| tcs.iter().any(|tc| tc.get("id").is_some()))
                    .unwrap_or(false)
            })
        })
        .expect("a chunk with tool_calls[0].id");
    let tcs = id_chunk.choices[0].delta.tool_calls.as_ref().unwrap();
    assert_eq!(tcs[0]["id"], "toolu_1");
    assert_eq!(tcs[0]["type"], "function");
    assert_eq!(tcs[0]["function"]["name"], "lookup");
    // Find the chunk carrying arguments (will be the next chunk).
    let arg_chunks: Vec<&OAIStreamChunk> = chunks
        .iter()
        .filter(|c| {
            c.choices.iter().any(|ch| {
                ch.delta
                    .tool_calls
                    .as_ref()
                    .map(|tcs| {
                        tcs.iter().any(|tc| {
                            tc.get("function")
                                .and_then(|f| f.get("arguments"))
                                .is_some()
                        })
                    })
                    .unwrap_or(false)
            })
        })
        .collect();
    let mut combined = String::new();
    for c in &arg_chunks {
        for ch in &c.choices {
            if let Some(tcs) = &ch.delta.tool_calls {
                for tc in tcs {
                    if let Some(args) = tc
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(|v| v.as_str())
                    {
                        combined.push_str(args);
                    }
                }
            }
        }
    }
    let parsed: Value = serde_json::from_str(&combined).unwrap();
    assert_eq!(parsed["q"], "rust");
    let fin = chunks
        .iter()
        .find(|chunk| {
            chunk
                .choices
                .first()
                .and_then(|choice| choice.finish_reason.as_deref())
                == Some("tool_calls")
        })
        .expect("finish chunk");
    assert_eq!(fin.choices[0].finish_reason.as_deref(), Some("tool_calls"));
}

#[test]
fn anthropic_request_source_preserves_thinking_and_block_order() {
    let request = AnthropicRequest {
        model: "claude-opus-4-8".into(),
        max_tokens: 4096,
        system: Some(json!([
            {"type": "text", "text": "system", "cache_control": {"type": "ephemeral"}}
        ])),
        messages: vec![
            AnthropicMessage {
                role: "assistant".into(),
                content: json!([
                    {"type": "thinking", "thinking": "summary", "signature": "opaque-signature"},
                    {"type": "text", "text": "working"},
                    {"type": "tool_use", "id": "tool_1", "name": "lookup", "input": {"q": "rust"}}
                ]),
            },
            AnthropicMessage {
                role: "user".into(),
                content: json!([
                    {"type": "tool_result", "tool_use_id": "tool_1", "content": "result"}
                ]),
            },
        ],
        temperature: None,
        stream: Some(false),
        stop_sequences: Some(vec!["done".into()]),
        extra: [
            (
                "thinking".into(),
                json!({"type": "enabled", "budget_tokens": 2048}),
            ),
            ("metadata".into(), json!({"user_id": "local-user"})),
        ]
        .into_iter()
        .collect(),
    };

    let mut canonical = anthropic_req_to_oai_req(&request);
    canonical.model = "MiniMax-M3".into();
    canonical.stream = true;
    let forwarded = oai_req_to_anthropic_req(&canonical).unwrap();

    assert_eq!(forwarded.model, "MiniMax-M3");
    assert_eq!(forwarded.stream, Some(true));
    assert_eq!(forwarded.system, request.system);
    assert_eq!(
        serde_json::to_value(&forwarded.messages).unwrap(),
        serde_json::to_value(&request.messages).unwrap()
    );
    assert_eq!(forwarded.stop_sequences, request.stop_sequences);
    assert_eq!(forwarded.extra["thinking"], request.extra["thinking"]);
    assert_eq!(forwarded.extra["metadata"], request.extra["metadata"]);
}

#[test]
fn anthropic_response_source_preserves_unknown_fields_and_order() {
    let source = json!({
        "id": "msg_source",
        "type": "message",
        "role": "assistant",
        "model": "MiniMax-M3",
        "content": [
            {"type": "thinking", "thinking": "summary", "signature": "opaque"},
            {"type": "text", "text": "answer"}
        ],
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": {"input_tokens": 5, "output_tokens": 7, "cache_read_input_tokens": 2},
        "base_resp": {"status_code": 0}
    });
    let parsed: AnthropicResponse = serde_json::from_value(source.clone()).unwrap();
    let mut canonical = anthropic_resp_to_oai_resp(&parsed, "claude-opus-4-8");
    canonical.source_anthropic_response = Some(source);

    let relayed = oai_resp_to_anthropic_resp(&canonical, "claude-opus-4-8");
    let relayed_value = serde_json::to_value(relayed).unwrap();
    assert_eq!(relayed_value["model"], "claude-opus-4-8");
    assert_eq!(relayed_value["content"][0]["signature"], "opaque");
    assert_eq!(relayed_value["content"][1]["type"], "text");
    assert_eq!(relayed_value["base_resp"]["status_code"], 0);
    assert_eq!(relayed_value["usage"]["cache_read_input_tokens"], 2);
}

#[tokio::test]
async fn anthropic_stream_relay_preserves_signature_and_usage() {
    let source_events = vec![
        json!({"type": "message_start", "message": {
            "id": "msg_1", "type": "message", "role": "assistant", "model": "MiniMax-M3",
            "content": [], "stop_reason": null, "stop_sequence": null,
            "usage": {"input_tokens": 5, "output_tokens": 0, "cache_read_input_tokens": 2, "cache_creation_input_tokens": 1}
        }}),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "reason"}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "opaque-signature"}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "answer"}}),
        json!({"type": "content_block_stop", "index": 1}),
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn", "stop_sequence": null}, "usage": {"output_tokens": 7}}),
        json!({"type": "message_stop"}),
    ];
    let source_stream = stream::iter(
        source_events
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>(),
    );
    let chunks: Vec<OAIStreamChunk> =
        anthropic_stream_to_oai_chunks(source_stream, "MiniMax-M3".into())
            .collect()
            .await;

    let usage = chunks
        .iter()
        .filter_map(|chunk| chunk.extra.get("usage"))
        .last()
        .expect("normalized usage");
    assert_eq!(usage["prompt_tokens"], 8);
    assert_eq!(usage["completion_tokens"], 7);
    assert_eq!(usage["total_tokens"], 15);

    let mut relayed =
        oai_chunks_to_anthropic_stream(stream::iter(chunks), "claude-opus-4-8".into());
    let mut relayed_values = Vec::new();
    while let Some(frame) = relayed.next().await {
        let payload = frame
            .split("data: ")
            .nth(1)
            .unwrap()
            .split("\n\n")
            .next()
            .unwrap();
        relayed_values.push(serde_json::from_str::<Value>(payload).unwrap());
    }
    let relayed_types: Vec<&str> = relayed_values
        .iter()
        .filter_map(|value| value.get("type").and_then(Value::as_str))
        .collect();
    let source_types: Vec<&str> = source_events
        .iter()
        .filter_map(|value| value.get("type").and_then(Value::as_str))
        .collect();
    assert_eq!(relayed_types, source_types);
    assert_eq!(relayed_values[0]["message"]["model"], "claude-opus-4-8");
    assert_eq!(relayed_values[3]["delta"]["signature"], "opaque-signature");
    assert_eq!(relayed_values[8]["usage"]["output_tokens"], 7);
}

#[test]
fn stream_delta_accepts_reasoning_content_alias() {
    let delta: StreamDelta = serde_json::from_value(json!({
        "reasoning_content": "thinking"
    }))
    .unwrap();
    assert_eq!(delta.reasoning.as_deref(), Some("thinking"));
    let serialized = serde_json::to_value(delta).unwrap();
    assert_eq!(serialized["reasoning_content"], "thinking");
    assert!(serialized.get("reasoning").is_none());
}

#[test]
fn strip_billing_header() {
    assert_eq!(
        strip_leading_anthropic_billing_header(
            "x-anthropic-billing-header: key=abc\nreal system prompt"
        ),
        "real system prompt"
    );
    assert_eq!(
        strip_leading_anthropic_billing_header("no header here"),
        "no header here"
    );
}

/// OpenAI image parts must convert to Anthropic image blocks: data URLs
/// become base64 sources and http(s) URLs become URL sources — an image sent
/// to an Anthropic hop may never be silently dropped (P1 regression).
#[test]
fn anthropic_egress_converts_image_parts() {
    let oai = ChatCompletionRequest {
        model: "claude-3-5-sonnet".into(),
        messages: vec![ChatMessage {
            role: "user".into(),
            content: Value::Array(vec![
                json!({ "type": "text", "text": "look:" }),
                json!({
                    "type": "image_url",
                    "image_url": { "url": "data:image/png;base64,aGVsbG8=" },
                }),
                json!({
                    "type": "image_url",
                    "image_url": { "url": "https://example.com/cat.png" },
                }),
            ]),
            tool_calls: None,
            tool_call_id: None,
            extra: empty_extra(),
        }],
        stream: false,
        temperature: None,
        max_tokens: Some(100),
        tools: None,
        tool_choice: None,
        extra: empty_extra(),
        source_anthropic_request: None,
        source_gemini_request: None,
        source_responses_request: None,
        request_id: None,
        client_credential: None,
    };
    let ant = oai_req_to_anthropic_req(&oai).unwrap();
    let blocks = ant.messages[0].content.as_array().unwrap();
    assert_eq!(blocks.len(), 3);
    assert_eq!(blocks[1]["type"], "image");
    assert_eq!(blocks[1]["source"]["type"], "base64");
    assert_eq!(blocks[1]["source"]["media_type"], "image/png");
    assert_eq!(blocks[1]["source"]["data"], "aGVsbG8=");
    assert_eq!(blocks[2]["type"], "image");
    assert_eq!(blocks[2]["source"]["type"], "url");
    assert_eq!(blocks[2]["source"]["url"], "https://example.com/cat.png");
}

/// Parts with no Anthropic representation (audio, files, malformed image
/// URLs) are rejected before send with stable error codes — never silently
/// dropped (P1 regression).
#[test]
fn anthropic_egress_rejects_unrepresentable_parts() {
    let base = ChatCompletionRequest {
        model: "claude-3-5-sonnet".into(),
        messages: vec![make_user_msg("hi")],
        stream: false,
        temperature: None,
        max_tokens: Some(100),
        tools: None,
        tool_choice: None,
        extra: empty_extra(),
        source_anthropic_request: None,
        source_gemini_request: None,
        source_responses_request: None,
        request_id: None,
        client_credential: None,
    };
    let with_part = |part: Value| {
        let mut req = base.clone();
        req.messages[0].content = Value::Array(vec![part]);
        req
    };
    let audio_err = oai_req_to_anthropic_req(&with_part(json!({
        "type": "input_audio",
        "input_audio": { "data": "AAAA", "format": "wav" },
    })))
    .unwrap_err();
    assert_eq!(audio_err.code, "anthropic_feature_unsupported");
    assert_eq!(audio_err.status, Some(400));

    let bad_url_err = oai_req_to_anthropic_req(&with_part(json!({
        "type": "image_url",
        "image_url": { "url": "file:///etc/passwd" },
    })))
    .unwrap_err();
    assert_eq!(bad_url_err.code, "anthropic_invalid_request");
    assert_eq!(bad_url_err.status, Some(400));

    let missing_url_err = oai_req_to_anthropic_req(&with_part(json!({
        "type": "image_url",
    })))
    .unwrap_err();
    assert_eq!(missing_url_err.code, "anthropic_invalid_request");
}

/// Anthropic tool definitions normalize into the canonical function wrapper
/// at the ingress so Chat/Responses/Gemini upstreams see real parameters —
/// `input_schema` must never ride through as an unknown shape (P2 regression).
#[test]
fn anthropic_ingress_normalizes_tools() {
    let ant_req = AnthropicRequest {
        model: "claude-3-5-sonnet".into(),
        max_tokens: 100,
        system: None,
        messages: vec![AnthropicMessage {
            role: "user".into(),
            content: json!([{ "type": "text", "text": "hi" }]),
        }],
        temperature: None,
        stream: None,
        stop_sequences: None,
        extra: {
            let mut extra = empty_extra();
            extra.insert(
                "tools".into(),
                json!([{
                    "name": "get_weather",
                    "description": "look up weather",
                    "input_schema": { "type": "object", "properties": { "city": { "type": "string" } } },
                }]),
            );
            extra
        },
    };
    let oai = anthropic_req_to_oai_req(&ant_req);
    let tools = oai.tools.unwrap();
    assert_eq!(tools[0]["type"], "function");
    assert_eq!(tools[0]["function"]["name"], "get_weather");
    assert_eq!(tools[0]["function"]["description"], "look up weather");
    assert_eq!(
        tools[0]["function"]["parameters"]["properties"]["city"]["type"],
        "string"
    );
    assert!(tools[0].get("input_schema").is_none());

    // Unknown/native definitions stay intact for the provider to reject.
    let passthrough = anthropic_tool_to_oai_tool(&json!({ "type": "custom", "name": "bash" }));
    assert_eq!(passthrough["type"], "custom");
}
