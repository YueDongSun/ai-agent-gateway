//! Mock provider - requires no network or API keys.
//!
//! Ported from `src/gateway/providers/mock.ts`.
//!
//! Special model names:
//! - `mock-fail` -> always 503 (retriable) - demonstrates circuit breaker + failover
//! - `mock-slow` -> 2s delay per request, succeeds
//! - anything else -> echo of the last user message

use crate::database::dao::proxy_providers::CircuitBreakerConfig;
use crate::proxy::canonical::{
    self, ChatCompletionRequest, OAIChatResponse, OAIChoice, OAIMessage, OAIStreamChunk,
    ProviderError, StreamDelta, Usage,
};
use crate::proxy::circuit_breaker::CircuitBreaker;
use crate::proxy::providers::Provider;
use async_trait::async_trait;
use futures_core::Stream;
use futures_util::StreamExt;
use std::pin::Pin;

pub struct MockProvider {
    id: String,
    models: Vec<String>,
    enabled: bool,
    breaker: CircuitBreaker,
}

impl MockProvider {
    pub fn new(id: String, models: Vec<String>, enabled: bool, cb_cfg: CircuitBreakerConfig) -> Self {
        Self {
            id,
            models,
            enabled,
            breaker: CircuitBreaker::new(cb_cfg),
        }
    }
}

fn mock_id() -> String {
    format!("chatcmpl-mock-{}", crate::proxy::converter::rand_hex(6))
}

fn build_reply(provider_id: &str, model: &str, text: &str) -> String {
    format!("MOCK[{}] :: {}\n\nYou said: {}", provider_id, model, text)
}

#[async_trait]
impl Provider for MockProvider {
    fn id(&self) -> &str { &self.id }
    fn enabled(&self) -> bool { self.enabled }
    fn models(&self) -> &[String] { &self.models }
    fn breaker(&self) -> &CircuitBreaker { &self.breaker }

    async fn complete(&self, req: &ChatCompletionRequest) -> Result<OAIChatResponse, ProviderError> {
        if req.model == "mock-fail" {
            return Err(canonical::make_error(
                format!("mock provider {} forced failure for model \"{}\"", self.id, req.model),
                Some(503),
                Some(true),
            ));
        }
        if req.model == "mock-slow" {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        let text = canonical::extract_text(req);
        let reply = build_reply(&self.id, &req.model, &text);
        Ok(OAIChatResponse {
            id: mock_id(),
            object: "chat.completion".into(),
            created: chrono::Utc::now().timestamp(),
            model: req.model.clone(),
            choices: vec![OAIChoice {
                index: 0,
                message: OAIMessage {
                    role: "assistant".into(),
                    content: Some(reply.clone()),
                    tool_calls: None,
                    refusal: None,
                    extra: Default::default(),
                },
                finish_reason: Some("stop".into()),
            }],
            usage: Some(Usage {
                prompt_tokens: text.len() as u32,
                completion_tokens: reply.len() as u32,
                total_tokens: (text.len() + reply.len()) as u32,
            }),
        })
    }

    fn stream(
        &self,
        req: &ChatCompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>, ProviderError> {
        if req.model == "mock-fail" {
            return Err(canonical::make_error(
                format!("mock {} forced stream failure", self.id),
                Some(503),
                Some(true),
            ));
        }
        let id = mock_id();
        let model = req.model.clone();
        let text = canonical::extract_text(req);
        let reply = build_reply(&self.id, &model, &text);

        // Split reply into whitespace-delimited tokens for streaming.
        let tokens: Vec<String> = if reply.is_empty() {
            vec![]
        } else {
            // Mimic JS `reply.match(/(\S+\s*|\s+)/g)` - greedy non-space runs
            // followed by whitespace, or whitespace runs.
            let mut toks = Vec::new();
            let mut chars = reply.chars().peekable();
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    let mut s = String::new();
                    while let Some(&c) = chars.peek() {
                        if c.is_whitespace() { s.push(c); chars.next(); } else { break; }
                    }
                    toks.push(s);
                } else {
                    let mut s = String::new();
                    while let Some(&c) = chars.peek() {
                        if !c.is_whitespace() { s.push(c); chars.next(); } else { break; }
                    }
                    // attach trailing whitespace to the token (greedy \S+\s*)
                    while let Some(&c) = chars.peek() {
                        if c.is_whitespace() { s.push(c); chars.next(); } else { break; }
                    }
                    toks.push(s);
                }
            }
            toks
        };

        let slow = model == "mock-slow";
        let stream = async_stream::stream! {
            // leading role chunk
            yield crate::proxy::converter::make_stream_chunk(
                &id, &model,
                StreamDelta { role: Some("assistant".into()), content: None, tool_calls: None, reasoning: None },
                None,
            );
            for tok in tokens {
                if slow { tokio::time::sleep(std::time::Duration::from_millis(120)).await; }
                yield crate::proxy::converter::make_stream_chunk(
                    &id, &model,
                    StreamDelta { role: None, content: Some(tok), tool_calls: None, reasoning: None },
                    None,
                );
            }
            yield crate::proxy::converter::make_stream_chunk(
                &id, &model,
                StreamDelta { role: None, content: None, tool_calls: None, reasoning: None },
                Some("stop".into()),
            );
        }
        .boxed();
        Ok(stream)
    }
}