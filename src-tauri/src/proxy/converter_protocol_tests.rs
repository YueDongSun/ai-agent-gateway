use super::*;

use crate::proxy::canonical::ChatCompletionRequest;
use futures_util::StreamExt;
use crate::proxy::responses::ResponsesRequest;
use serde_json::{json, Value};


fn parse_sse_events(events: &[String]) -> Vec<Value> {
    events
        .iter()
        .filter_map(|event| {
            event
                .lines()
                .find_map(|line| line.strip_prefix("data: "))
                .filter(|data| *data != "[DONE]")
                .map(|data| serde_json::from_str(data).unwrap())
        })
        .collect()
}

async fn anthropic_events_to_responses(events: Vec<Value>) -> Vec<String> {
    let source = futures_util::stream::iter(events.into_iter().map(|event| event.to_string()));
    let chunks = anthropic_stream_to_oai_chunks(source, "fixture_model".into());
    chat_chunks_to_responses_stream(chunks, "fixture_alias")
        .collect()
        .await
}

fn tool_turn_events() -> Vec<Value> {
    vec![
        json!({"type": "message_start", "message": {"usage": {"input_tokens": 11, "output_tokens": 0}}}),
        json!({"type": "content_block_start", "index": 2, "content_block": {
            "type": "tool_use", "id": "call_fixture", "name": "read_fixture", "input": {}}}),
        json!({"type": "content_block_delta", "index": 2, "delta": {
            "type": "input_json_delta", "partial_json": "{\"path\":"}}),
        json!({"type": "content_block_delta", "index": 2, "delta": {
            "type": "input_json_delta", "partial_json": "\"fixture.txt\"}"}}),
        json!({"type": "content_block_stop", "index": 2}),
        json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 7}}),
        json!({"type": "message_stop"}),
    ]
}

fn parallel_tool_follow_up() -> ResponsesRequest {
    let mut input = vec![
        json!({"role": "user", "content": "Read four public fixtures"}),
        json!({"type": "message", "role": "assistant", "content": [
            {"type": "output_text", "text": "Reading the fixtures."}
        ]}),
    ];
    for index in 0..4 {
        input.push(
            json!({"type": "function_call", "call_id": format!("call_{index}"),
            "name": "read_fixture", "arguments": json!({"fixture_index": index}).to_string()}),
        );
    }
    for index in 0..4 {
        input.push(
            json!({"type": "function_call_output", "call_id": format!("call_{index}"),
            "output": format!("FIXTURE_{index}")}),
        );
    }
    serde_json::from_value(
        json!({"model": "fixture_model", "input": input, "stream": true,
        "tools": [{"type": "function", "name": "read_fixture", "parameters": {
            "type": "object", "properties": {"fixture_index": {"type": "integer"}}
        }}], "tool_choice": "auto"}),
    )
    .unwrap()
}

#[test]
fn responses_parallel_calls_share_one_assistant_turn() {
    let chat = responses_to_chat(&parallel_tool_follow_up());
    assert_eq!(
        chat.messages.len(),
        6,
        "user, assistant with four calls, four tool results"
    );
    assert_eq!(chat.messages[1].role, "assistant");
    assert_eq!(text_of(&chat.messages[1].content), "Reading the fixtures.");
    let calls = chat.messages[1].tool_calls.as_ref().unwrap();
    assert_eq!(calls.len(), 4);
    for index in 0..4 {
        assert_eq!(calls[index]["id"], format!("call_{index}"));
        assert_eq!(
            calls[index]["function"]["arguments"],
            json!({"fixture_index": index}).to_string()
        );
        assert_eq!(chat.messages[index + 2].role, "tool");
        assert_eq!(
            chat.messages[index + 2].tool_call_id.as_deref(),
            Some(format!("call_{index}").as_str())
        );
    }
}

#[test]
fn anthropic_parallel_results_share_the_next_user_turn() {
    let anthropic = oai_req_to_anthropic_req(&responses_to_chat(&parallel_tool_follow_up())).unwrap();
    assert_eq!(
        anthropic.messages.len(),
        3,
        "all calls and all results stay in their respective turns"
    );
    assert_eq!(anthropic.messages[1].role, "assistant");
    let calls = anthropic.messages[1].content.as_array().unwrap();
    assert_eq!(calls.len(), 5, "preamble plus four tool_use blocks");
    assert_eq!(calls[0]["text"], "Reading the fixtures.");
    assert_eq!(anthropic.messages[2].role, "user");
    let results = anthropic.messages[2].content.as_array().unwrap();
    assert_eq!(results.len(), 4);
    for index in 0..4 {
        assert_eq!(calls[index + 1]["type"], "tool_use");
        assert_eq!(calls[index + 1]["input"], json!({"fixture_index": index}));
        assert_eq!(results[index]["type"], "tool_result");
        assert_eq!(results[index]["tool_use_id"], calls[index + 1]["id"]);
        assert_eq!(results[index]["content"], format!("FIXTURE_{index}"));
    }
}

#[test]
fn serial_tool_turns_remain_separate() {
    let mut request = parallel_tool_follow_up();
    let parallel_input = request.input.as_array().unwrap();
    request.input = json!([
        parallel_input[0],
        parallel_input[2],
        parallel_input[6],
        parallel_input[3],
        parallel_input[7]
    ]);
    let chat = responses_to_chat(&request);
    let chat_roles: Vec<&str> = chat
        .messages
        .iter()
        .map(|message| message.role.as_str())
        .collect();
    assert_eq!(
        chat_roles,
        vec!["user", "assistant", "tool", "assistant", "tool"]
    );
    assert_eq!(chat.messages[1].tool_calls.as_ref().unwrap().len(), 1);
    assert_eq!(chat.messages[3].tool_calls.as_ref().unwrap().len(), 1);
    let anthropic = oai_req_to_anthropic_req(&chat).unwrap();
    let roles: Vec<&str> = anthropic
        .messages
        .iter()
        .map(|message| message.role.as_str())
        .collect();
    assert_eq!(
        roles,
        vec!["user", "assistant", "user", "assistant", "user"]
    );
    assert_eq!(anthropic.messages[2].content[0]["tool_use_id"], "call_0");
    assert_eq!(anthropic.messages[4].content[0]["tool_use_id"], "call_1");
}

#[tokio::test]
async fn anthropic_tool_stream_round_trips_through_responses_follow_up() {
    let events = anthropic_events_to_responses(tool_turn_events()).await;
    assert_eq!(events.last().unwrap(), crate::proxy::responses::STREAM_DONE);
    let parsed = parse_sse_events(&events);
    let types: Vec<&str> = parsed
        .iter()
        .map(|event| event["type"].as_str().unwrap())
        .collect();
    assert_eq!(
        types,
        vec![
            "response.created",
            "response.output_item.added",
            "response.function_call_arguments.delta",
            "response.function_call_arguments.delta",
            "response.function_call_arguments.done",
            "response.output_item.done",
            "response.completed"
        ]
    );
    let item_id = &parsed[1]["item"]["id"];
    for event in &parsed[2..=4] {
        assert_eq!(&event["item_id"], item_id);
        assert_eq!(event["output_index"], 0);
    }
    assert_eq!(parsed[2]["delta"], "{\"path\":");
    assert_eq!(parsed[3]["delta"], "\"fixture.txt\"}");
    assert_eq!(parsed[4]["arguments"], "{\"path\":\"fixture.txt\"}");
    let response = &parsed[6]["response"];
    assert_eq!(response["status"], "completed");
    assert_eq!(response["usage"]["input_tokens"], 11);
    assert_eq!(response["usage"]["output_tokens"], 7);
    let function_call = &response["output"][0];
    assert_eq!(response["output"].as_array().unwrap().len(), 1);
    assert_eq!(&parsed[5]["item"], function_call);
    assert_eq!(function_call["call_id"], "call_fixture");
    assert_eq!(function_call["name"], "read_fixture");
    assert_eq!(function_call["arguments"], parsed[4]["arguments"]);
    assert_eq!(function_call["status"], "completed");

    let follow_up: ResponsesRequest = serde_json::from_value(json!({
        "model": "fixture_alias", "stream": true, "tool_choice": "auto",
        "tools": [{"type": "function", "name": "read_fixture", "description": "Read a fixture",
            "parameters": {"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}}],
        "input": [
            {"role": "user", "content": "Read the public fixture"},
            function_call,
            {"type": "function_call_output", "call_id": "call_fixture", "output": "FIXTURE_OK"},
        ],
    })).unwrap();
    let anthropic = oai_req_to_anthropic_req(&responses_to_chat(&follow_up)).unwrap();
    assert_eq!(
        anthropic.extra["tools"][0]["input_schema"],
        follow_up.tools.unwrap()[0]["parameters"]
    );
    assert_eq!(anthropic.extra["tools"][0]["name"], "read_fixture");
    assert_eq!(anthropic.extra["tools"][0]["description"], "Read a fixture");
    assert_eq!(anthropic.messages[1].role, "assistant");
    assert_eq!(
        anthropic.messages[1].content,
        json!([{
            "type": "tool_use", "id": "call_fixture", "name": "read_fixture", "input": {"path": "fixture.txt"}
        }])
    );
    assert_eq!(anthropic.messages[2].role, "user");
    assert_eq!(
        anthropic.messages[2].content,
        json!([{
            "type": "tool_result", "tool_use_id": "call_fixture", "content": "FIXTURE_OK"
        }])
    );
}

#[tokio::test]
async fn anthropic_follow_up_text_emits_text_done_before_item_done() {
    let events = anthropic_events_to_responses(vec![
        json!({"type": "message_start", "message": {"usage": {"input_tokens": 13, "output_tokens": 0}}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "FIXTURE_OK"}}),
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 3}}),
        json!({"type": "message_stop"}),
    ]).await;
    let parsed = parse_sse_events(&events);
    let types: Vec<&str> = parsed
        .iter()
        .map(|event| event["type"].as_str().unwrap())
        .collect();
    assert_eq!(
        types,
        vec![
            "response.created",
            "response.output_item.added",
            "response.output_text.delta",
            "response.output_text.done",
            "response.output_item.done",
            "response.completed"
        ]
    );
    assert_eq!(parsed[3]["item_id"], parsed[1]["item"]["id"]);
    assert_eq!(parsed[3]["output_index"], 0);
    assert_eq!(parsed[3]["content_index"], 0);
    assert_eq!(parsed[3]["text"], "FIXTURE_OK");
    assert_eq!(parsed[4]["item"]["content"][0]["text"], parsed[3]["text"]);
    assert_eq!(parsed[5]["response"]["usage"]["output_tokens"], 3);
    assert_eq!(events.last().unwrap(), crate::proxy::responses::STREAM_DONE);
}

#[tokio::test]
async fn anthropic_interrupted_tool_stream_never_completes_a_call() {
    let mut events = tool_turn_events();
    events.pop(); // Missing message_stop remains an error, even after a stop reason.
    let output = anthropic_events_to_responses(events).await;
    let parsed = parse_sse_events(&output);
    assert_eq!(parsed.last().unwrap()["type"], "response.failed");
    assert_eq!(parsed.last().unwrap()["response"]["status"], "failed");
    assert!(!output
        .iter()
        .any(|event| event == crate::proxy::responses::STREAM_DONE));
    assert!(!parsed.iter().any(|event| matches!(
        event["type"].as_str(),
        Some(
            "response.completed"
                | "response.function_call_arguments.done"
                | "response.output_item.done"
        )
    )));
}

#[tokio::test]
async fn split_tool_identity_flushes_buffered_arguments_after_added() {
    let deltas = vec![
        json!({"index": 0, "function": {"arguments": "{\"path\":"}}),
        json!({"index": 0, "function": {"name": "read_fixture", "arguments": "\"fixture.txt\"}"}}),
        json!({"index": 0, "id": "call_fixture"}),
    ];
    let chunks = deltas
        .into_iter()
        .map(|tool_delta| {
            let delta = serde_json::from_value(json!({"tool_calls": [tool_delta]})).unwrap();
            make_stream_chunk("fixture", "fixture_model", delta, None)
        })
        .chain(std::iter::once(make_stream_chunk(
            "fixture",
            "fixture_model",
            serde_json::from_value(json!({})).unwrap(),
            Some("tool_calls".into()),
        )));
    let output: Vec<String> = chat_chunks_to_responses_stream(
        Box::pin(futures_util::stream::iter(chunks)),
        "fixture_alias",
    )
    .collect()
    .await;
    let parsed = parse_sse_events(&output);
    assert_eq!(parsed[1]["type"], "response.output_item.added");
    assert_eq!(parsed[1]["item"]["call_id"], "call_fixture");
    assert_eq!(parsed[1]["item"]["name"], "read_fixture");
    let argument_deltas: String = parsed
        .iter()
        .filter(|event| event["type"] == "response.function_call_arguments.delta")
        .map(|event| event["delta"].as_str().unwrap())
        .collect();
    assert_eq!(argument_deltas, "{\"path\":\"fixture.txt\"}");
    let completed = parsed.last().unwrap();
    assert_eq!(
        completed["response"]["output"][0]["arguments"],
        argument_deltas
    );
    assert_eq!(
        completed["response"]["output"][0]["id"],
        parsed[1]["item"]["id"]
    );
}

#[test]
fn responses_tool_choice_uses_anthropic_object_shape() {
    for (choice, expected) in [
        (json!("auto"), json!({"type": "auto"})),
        (json!("none"), json!({"type": "none"})),
        (json!("required"), json!({"type": "any"})),
        (
            json!({"type": "function", "name": "ping"}),
            json!({"type": "tool", "name": "ping"}),
        ),
    ] {
        let request: ResponsesRequest = serde_json::from_value(json!({
            "model": "fixture_model", "input": "fixture",
            "tools": [{"type": "function", "name": "ping"}],
            "tool_choice": choice,
        }))
        .unwrap();
        assert_eq!(
            oai_req_to_anthropic_req(&responses_to_chat(&request)).unwrap().extra["tool_choice"],
            expected
        );
    }
}

#[tokio::test]
async fn incomplete_tool_identity_fails_instead_of_dropping_the_call() {
    for tool_delta in [
        json!({"index": 0, "function": {"name": "read_fixture", "arguments": "{}"}}),
        json!({"index": 0, "id": "call_fixture", "function": {"arguments": "{}"}}),
        json!({"index": 0, "function": {"arguments": "{}"}}),
    ] {
        let delta = serde_json::from_value(json!({"tool_calls": [tool_delta]})).unwrap();
        let chunks = vec![make_stream_chunk(
            "fixture",
            "fixture_model",
            delta,
            Some("tool_calls".into()),
        )];
        let output: Vec<String> = chat_chunks_to_responses_stream(
            Box::pin(futures_util::stream::iter(chunks)),
            "fixture_alias",
        )
        .collect()
        .await;
        let parsed = parse_sse_events(&output);
        assert_eq!(parsed.last().unwrap()["type"], "response.failed");
        assert_eq!(
            parsed.last().unwrap()["response"]["error"]["code"],
            "proxy_stream_error"
        );
        assert!(!parsed
            .iter()
            .any(|event| event["type"] == "response.completed"));
        assert!(!output
            .iter()
            .any(|event| event == crate::proxy::responses::STREAM_DONE));
    }
}

#[tokio::test]
async fn complete_tool_on_final_chunk_emits_all_tool_events() {
    let delta = serde_json::from_value(json!({"tool_calls": [{
        "index": 0, "id": "call_fixture", "function": {"name": "read_fixture", "arguments": "{}"}
    }]}))
    .unwrap();
    let chunk = make_stream_chunk("fixture", "fixture_model", delta, Some("tool_calls".into()));
    let output: Vec<String> = chat_chunks_to_responses_stream(
        Box::pin(futures_util::stream::iter(vec![chunk])),
        "fixture_alias",
    )
    .collect()
    .await;
    let parsed = parse_sse_events(&output);
    let types: Vec<&str> = parsed
        .iter()
        .map(|event| event["type"].as_str().unwrap())
        .collect();
    assert_eq!(
        types,
        vec![
            "response.created",
            "response.output_item.added",
            "response.function_call_arguments.delta",
            "response.function_call_arguments.done",
            "response.output_item.done",
            "response.completed"
        ]
    );
    assert_eq!(parsed[1]["item"]["call_id"], "call_fixture");
    assert_eq!(parsed[2]["delta"], "{}");
    assert_eq!(parsed[3]["arguments"], "{}");
    assert_eq!(parsed[4]["item"], parsed[5]["response"]["output"][0]);
    assert_eq!(output.last().unwrap(), crate::proxy::responses::STREAM_DONE);
}

#[test]
fn responses_function_reaches_anthropic_with_native_schema() {
    let request: ResponsesRequest = serde_json::from_value(json!({
        "model": "fixture_model", "input": "public fixture", "stream": false,
        "tools": [{"type": "function", "name": "read_fixture",
            "description": "Read the fixture", "strict": true,
            "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}}]
    }))
    .unwrap();
    let converted = oai_req_to_anthropic_req(&responses_to_chat(&request)).unwrap();
    assert_eq!(
        converted.extra["tools"],
        json!([{
            "name": "read_fixture", "description": "Read the fixture",
            "input_schema": {"type": "object", "properties": {"path": {"type": "string"}}}
        }])
    );
}

#[test]
fn canonical_function_without_parameters_has_empty_object_schema() {
    let request: ChatCompletionRequest = serde_json::from_value(json!({
        "model": "fixture_model", "messages": [{"role": "user", "content": "fixture"}],
        "tools": [{"type": "function", "function": {"name": "ping", "strict": true}}]
    }))
    .unwrap();
    assert_eq!(
        oai_req_to_anthropic_req(&request).unwrap().extra["tools"],
        json!([{
            "name": "ping", "input_schema": {"type": "object", "properties": {}}
        }])
    );
}

#[test]
fn native_and_invalid_tool_definitions_are_not_dropped() {
    for tool in [
        json!({"name": "native", "input_schema": {"type": "object"}, "cache_control": {"type": "ephemeral"}}),
        json!({"type": "function", "function": {"parameters": {"type": "object"}}}),
        json!({"type": "unknown_future_tool"}),
    ] {
        assert_eq!(oai_tool_to_anthropic_tool(&tool), tool);
    }
}

#[test]
fn native_anthropic_source_tool_envelope_stays_intact() {
    let native = json!({
        "model": "fixture_model", "max_tokens": 64,
        "messages": [{"role": "user", "content": "fixture"}],
        "tools": [{"name": "native", "input_schema": {"type": "object"},
            "cache_control": {"type": "ephemeral"}}],
        "tool_choice": {"type": "tool", "name": "native", "disable_parallel_tool_use": true}
    });
    let mut request: ChatCompletionRequest = serde_json::from_value(json!({
        "model": "fixture_model", "messages": []
    }))
    .unwrap();
    request.source_anthropic_request = Some(native.clone());
    assert_eq!(
        oai_req_to_anthropic_req(&request).unwrap().extra["tools"],
        native["tools"]
    );
    assert_eq!(
        oai_req_to_anthropic_req(&request).unwrap().extra["tool_choice"],
        native["tool_choice"]
    );
}
