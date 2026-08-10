//! Native Anthropic Messages API backend — the proxied paid tier.
//!
//! Why native rather than the OpenAI-compat endpoint: prompt caching. The
//! coach's system persona is stable across a session, so it is sent as a
//! `system` text block carrying `cache_control: {"type": "ephemeral"}` —
//! after the first request the prefix is served from cache (reads bill ~0.1x).
//!
//! Wire-format notes (Messages API, `anthropic-version: 2023-06-01`):
//! - tools use `input_schema` (not `parameters`)
//! - tool results are `tool_result` blocks inside a *user* message, and all
//!   results answering one assistant turn must share a single user message —
//!   our transcript stores them as consecutive [`Role::Tool`] messages, so
//!   adjacent runs are collapsed here
//! - response `content` is an array of `text` / `tool_use` blocks, with
//!   `tool_use.input` already a JSON object (not a string)

use super::sse::{sse_data_payload, SseLineBuffer};
use super::{
    Capabilities, CoachModel, CompletionRequest, CompletionResponse, LlmError, Message, Role,
    StreamEvent, ToolCall, Usage,
};
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::mpsc::Sender;

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const MAX_TOKENS: u32 = 1024;

pub struct AnthropicBackend {
    http: reqwest::Client,
    api_key: String,
    model: String,
    base_url: String,
}

impl AnthropicBackend {
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_key,
            model,
            base_url: DEFAULT_BASE_URL.to_string(),
        }
    }

    pub fn with_base_url(mut self, base_url: String) -> Self {
        self.base_url = base_url;
        self
    }
}

/// Build the `/v1/messages` request body. Pure so the message mapping —
/// especially adjacent-tool-result collapsing and cache_control placement —
/// can be unit-tested without a network.
fn build_request_body(model: &str, req: &CompletionRequest) -> Value {
    let mut body = json!({
        "model": model,
        "max_tokens": MAX_TOKENS,
        "messages": wire_messages(&req.messages),
    });

    if !req.system.is_empty() {
        // The stable coach persona prefix: mark it with a cache breakpoint so
        // repeat requests in a session read it from cache (~0.1x billing).
        body["system"] = json!([{
            "type": "text",
            "text": req.system,
            "cache_control": {"type": "ephemeral"},
        }]);
    }

    if !req.tools.is_empty() {
        body["tools"] = Value::Array(
            req.tools
                .iter()
                .map(|t| {
                    json!({
                        "name": t.name,
                        "description": t.description,
                        // NOTE: Anthropic calls the schema field `input_schema`.
                        "input_schema": t.parameters,
                    })
                })
                .collect(),
        );
    }

    body
}

/// Map our transcript to Anthropic messages, collapsing each run of adjacent
/// [`Role::Tool`] messages into one user message of `tool_result` blocks
/// (Anthropic requires all results for one assistant turn in a single user
/// message).
fn wire_messages(messages: &[Message]) -> Vec<Value> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < messages.len() {
        let m = &messages[i];
        match m.role {
            Role::User => {
                out.push(json!({ "role": "user", "content": m.content }));
                i += 1;
            }
            Role::Assistant => {
                if m.tool_calls.is_empty() {
                    out.push(json!({ "role": "assistant", "content": m.content }));
                } else {
                    let mut blocks = Vec::new();
                    if !m.content.is_empty() {
                        blocks.push(json!({ "type": "text", "text": m.content }));
                    }
                    for tc in &m.tool_calls {
                        blocks.push(json!({
                            "type": "tool_use",
                            "id": tc.id,
                            "name": tc.name,
                            "input": tc.arguments,
                        }));
                    }
                    out.push(json!({ "role": "assistant", "content": blocks }));
                }
                i += 1;
            }
            Role::Tool => {
                let mut blocks = Vec::new();
                while i < messages.len() && messages[i].role == Role::Tool {
                    let t = &messages[i];
                    blocks.push(json!({
                        "type": "tool_result",
                        "tool_use_id": t.tool_call_id.clone().unwrap_or_default(),
                        "content": t.content,
                    }));
                    i += 1;
                }
                out.push(json!({ "role": "user", "content": blocks }));
            }
        }
    }
    out
}

/// Parse a Messages API response body into our [`CompletionResponse`].
/// Pure for unit testing.
fn parse_response(v: &Value) -> Result<CompletionResponse, LlmError> {
    let content = v["content"].as_array().ok_or_else(|| {
        LlmError::Malformed("response has no content array".into())
    })?;

    let mut text = String::new();
    let mut tool_calls = Vec::new();
    for block in content {
        match block["type"].as_str() {
            Some("text") => {
                if let Some(t) = block["text"].as_str() {
                    text.push_str(t);
                }
            }
            Some("tool_use") => {
                tool_calls.push(ToolCall {
                    id: block["id"].as_str().unwrap_or_default().to_string(),
                    name: block["name"].as_str().unwrap_or_default().to_string(),
                    // `input` is already a JSON object on this API.
                    arguments: if block["input"].is_null() {
                        json!({})
                    } else {
                        block["input"].clone()
                    },
                });
            }
            _ => {}
        }
    }

    // Total prompt tokens = uncached input + cache reads + cache writes.
    let u = &v["usage"];
    let input_tokens = u["input_tokens"].as_u64().unwrap_or(0)
        + u["cache_read_input_tokens"].as_u64().unwrap_or(0)
        + u["cache_creation_input_tokens"].as_u64().unwrap_or(0);
    let usage = Usage {
        input_tokens,
        output_tokens: u["output_tokens"].as_u64().unwrap_or(0),
    };

    Ok(CompletionResponse {
        text: if text.is_empty() { None } else { Some(text) },
        tool_calls,
        usage,
    })
}

/// Parse an accumulated `input_json_delta` string into the tool input object.
/// Empty means the tool takes no input; malformed payloads repair to `{}`,
/// consistent with the codebase's weak-model tolerance.
fn parse_tool_input(raw: &str) -> Value {
    if raw.trim().is_empty() {
        return json!({});
    }
    serde_json::from_str(raw).unwrap_or_else(|_| json!({}))
}

/// A content block being assembled from streamed events, tracked by the
/// `index` field the API stamps on every block event.
#[derive(Debug)]
enum PartialBlock {
    Text(String),
    ToolUse {
        id: String,
        name: String,
        /// `input_json_delta` fragments, concatenated as they arrive.
        partial_json: String,
        /// Parsed at `content_block_stop`; [`StreamAccumulator::finish`]
        /// falls back to parsing `partial_json` if the stop never arrived.
        input: Option<Value>,
    },
    /// A block type we don't know (e.g. future thinking blocks) — tracked so
    /// its deltas can be ignored without disturbing other indices.
    Unknown,
}

/// Accumulates Anthropic Messages API stream events into a full response.
/// Pure (no I/O) so it can be unit-tested against synthetic SSE fixtures.
///
/// Dispatches on each payload's `"type"`; unknown event types are ignored for
/// forward compatibility. Usage is combined from `message_start` (input side,
/// including cache read/creation tokens — same accounting as
/// [`parse_response`]) and `message_delta` (output side).
#[derive(Debug, Default)]
struct StreamAccumulator {
    /// Blocks by stream `index`, kept in order via a sorted map (indices are
    /// small and sequential in practice, but nothing here relies on that).
    blocks: std::collections::BTreeMap<usize, PartialBlock>,
    usage: Usage,
    done: bool,
}

impl StreamAccumulator {
    fn new() -> Self {
        Self::default()
    }

    /// Process one SSE data payload. Returns the text delta to emit, if any;
    /// an in-stream `error` event surfaces as [`LlmError::Api`].
    fn process(&mut self, payload: &str) -> Result<Option<String>, LlmError> {
        // Tolerate junk payloads — same posture as the OpenAI-compat adapter.
        let Ok(v) = serde_json::from_str::<Value>(payload) else {
            return Ok(None);
        };

        match v["type"].as_str() {
            Some("message_start") => {
                // Total prompt tokens = uncached input + cache reads + cache
                // writes, matching parse_response.
                let u = &v["message"]["usage"];
                self.usage.input_tokens = u["input_tokens"].as_u64().unwrap_or(0)
                    + u["cache_read_input_tokens"].as_u64().unwrap_or(0)
                    + u["cache_creation_input_tokens"].as_u64().unwrap_or(0);
            }
            Some("content_block_start") => {
                let Some(index) = v["index"].as_u64() else {
                    return Ok(None);
                };
                let block = &v["content_block"];
                let partial = match block["type"].as_str() {
                    Some("text") => {
                        PartialBlock::Text(block["text"].as_str().unwrap_or_default().to_string())
                    }
                    Some("tool_use") => PartialBlock::ToolUse {
                        id: block["id"].as_str().unwrap_or_default().to_string(),
                        name: block["name"].as_str().unwrap_or_default().to_string(),
                        partial_json: String::new(),
                        input: None,
                    },
                    _ => PartialBlock::Unknown,
                };
                self.blocks.insert(index as usize, partial);
            }
            Some("content_block_delta") => {
                let Some(index) = v["index"].as_u64() else {
                    return Ok(None);
                };
                let delta = &v["delta"];
                match delta["type"].as_str() {
                    Some("text_delta") => {
                        let fragment = delta["text"].as_str().unwrap_or_default();
                        if fragment.is_empty() {
                            return Ok(None);
                        }
                        // Tolerate a missing content_block_start.
                        let block = self
                            .blocks
                            .entry(index as usize)
                            .or_insert_with(|| PartialBlock::Text(String::new()));
                        if let PartialBlock::Text(text) = block {
                            text.push_str(fragment);
                            return Ok(Some(fragment.to_string()));
                        }
                    }
                    Some("input_json_delta") => {
                        if let Some(PartialBlock::ToolUse { partial_json, .. }) =
                            self.blocks.get_mut(&(index as usize))
                        {
                            partial_json.push_str(delta["partial_json"].as_str().unwrap_or_default());
                        }
                    }
                    _ => {}
                }
            }
            Some("content_block_stop") => {
                if let Some(index) = v["index"].as_u64() {
                    if let Some(PartialBlock::ToolUse {
                        partial_json,
                        input,
                        ..
                    }) = self.blocks.get_mut(&(index as usize))
                    {
                        *input = Some(parse_tool_input(partial_json));
                    }
                }
            }
            Some("message_delta") => {
                if let Some(out) = v["usage"]["output_tokens"].as_u64() {
                    self.usage.output_tokens = out;
                }
            }
            Some("message_stop") => self.done = true,
            Some("error") => {
                // The HTTP exchange succeeded (200) but the stream carried an
                // API error; surface the payload as the error body.
                return Err(LlmError::Api {
                    status: 200,
                    body: payload.to_string(),
                });
            }
            // "ping" and any future event types: ignore.
            _ => {}
        }
        Ok(None)
    }

    fn finish(self) -> CompletionResponse {
        let mut text = String::new();
        let mut tool_calls = Vec::new();
        // BTreeMap iteration is index order, so blocks keep stream order.
        for (_, block) in self.blocks {
            match block {
                PartialBlock::Text(t) => text.push_str(&t),
                PartialBlock::ToolUse {
                    id,
                    name,
                    partial_json,
                    input,
                } => tool_calls.push(ToolCall {
                    id,
                    name,
                    arguments: input.unwrap_or_else(|| parse_tool_input(&partial_json)),
                }),
                PartialBlock::Unknown => {}
            }
        }
        CompletionResponse {
            text: if text.is_empty() { None } else { Some(text) },
            tool_calls,
            usage: self.usage,
        }
    }
}

#[async_trait]
impl CoachModel for AnthropicBackend {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            context_tokens: 200_000,
            ..Capabilities::default()
        }
    }

    async fn complete(&self, req: &CompletionRequest) -> Result<CompletionResponse, LlmError> {
        let body = build_request_body(&self.model, req);
        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));
        let response = self
            .http
            .post(url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        let status = response.status();
        let text = response.text().await?;
        if !status.is_success() {
            return Err(LlmError::Api {
                status: status.as_u16(),
                body: text,
            });
        }

        let v: Value = serde_json::from_str(&text)
            .map_err(|e| LlmError::Malformed(format!("invalid JSON: {e}")))?;
        parse_response(&v)
    }

    async fn complete_stream(
        &self,
        req: &CompletionRequest,
        sink: Sender<StreamEvent>,
    ) -> Result<CompletionResponse, LlmError> {
        let mut body = build_request_body(&self.model, req);
        body["stream"] = Value::Bool(true);
        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));
        let mut response = self
            .http
            .post(url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await?;
            return Err(LlmError::Api {
                status: status.as_u16(),
                body: text,
            });
        }

        let mut lines = SseLineBuffer::new();
        let mut acc = StreamAccumulator::new();
        'read: while let Some(chunk) = response.chunk().await? {
            for line in lines.feed(&chunk) {
                let Some(payload) = sse_data_payload(&line) else {
                    continue;
                };
                if let Some(delta) = acc.process(payload)? {
                    // A dropped receiver just means nobody is listening;
                    // keep accumulating so the caller still gets the response.
                    let _ = sink.send(StreamEvent::TextDelta(delta)).await;
                }
                if acc.done {
                    break 'read;
                }
            }
        }
        Ok(acc.finish())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::ToolDef;

    fn req(system: &str, messages: Vec<Message>, tools: Vec<ToolDef>) -> CompletionRequest {
        CompletionRequest {
            system: system.to_string(),
            messages,
            tools,
        }
    }

    #[test]
    fn system_is_a_text_block_array_with_cache_control() {
        let body = build_request_body("claude-opus-5", &req("You are a chess coach.", vec![], vec![]));
        assert_eq!(body["model"], "claude-opus-5");
        assert_eq!(body["max_tokens"], 1024);
        let system = body["system"].as_array().expect("system must be an array");
        assert_eq!(system.len(), 1);
        assert_eq!(system[0]["type"], "text");
        assert_eq!(system[0]["text"], "You are a chess coach.");
        assert_eq!(system[0]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn empty_system_is_omitted() {
        let body = build_request_body("m", &req("", vec![Message::user("hi")], vec![]));
        assert!(body.get("system").is_none());
    }

    #[test]
    fn tools_use_input_schema_field() {
        let tools = vec![ToolDef {
            name: "best_move".into(),
            description: "Ask the engine for the best move".into(),
            parameters: json!({"type": "object", "properties": {"fen": {"type": "string"}}}),
        }];
        let body = build_request_body("m", &req("s", vec![], tools));
        let t = &body["tools"][0];
        assert_eq!(t["name"], "best_move");
        assert!(t.get("input_schema").is_some(), "schema field must be input_schema");
        assert!(t.get("parameters").is_none(), "must not use OpenAI's `parameters`");
        assert_eq!(t["input_schema"]["type"], "object");
    }

    #[test]
    fn assistant_with_tool_calls_maps_to_tool_use_blocks() {
        let messages = vec![Message::assistant_with_tools(
            Some("Let me check.".into()),
            vec![ToolCall {
                id: "toolu_1".into(),
                name: "best_move".into(),
                arguments: json!({"fen": "start"}),
            }],
        )];
        let body = build_request_body("m", &req("s", messages, vec![]));
        let content = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "Let me check.");
        assert_eq!(content[1]["type"], "tool_use");
        assert_eq!(content[1]["id"], "toolu_1");
        assert_eq!(content[1]["name"], "best_move");
        assert_eq!(content[1]["input"], json!({"fen": "start"}));
    }

    #[test]
    fn empty_assistant_text_block_is_omitted_when_tool_calls_present() {
        let messages = vec![Message::assistant_with_tools(
            None,
            vec![ToolCall {
                id: "toolu_1".into(),
                name: "t".into(),
                arguments: json!({}),
            }],
        )];
        let body = build_request_body("m", &req("s", messages, vec![]));
        let content = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "tool_use");
    }

    #[test]
    fn adjacent_tool_results_collapse_into_one_user_message() {
        let messages = vec![
            Message::user("What's best here?"),
            Message::assistant_with_tools(
                None,
                vec![
                    ToolCall { id: "a".into(), name: "t1".into(), arguments: json!({}) },
                    ToolCall { id: "b".into(), name: "t2".into(), arguments: json!({}) },
                ],
            ),
            Message::tool_result("a", "result A"),
            Message::tool_result("b", "result B"),
            Message::assistant("Both look fine."),
            Message::user("And now?"),
        ];
        let body = build_request_body("m", &req("s", messages, vec![]));
        let wire = body["messages"].as_array().unwrap();
        // user, assistant(tool_use), ONE user(tool_results), assistant, user
        assert_eq!(wire.len(), 5);
        assert_eq!(wire[2]["role"], "user");
        let blocks = wire[2]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["type"], "tool_result");
        assert_eq!(blocks[0]["tool_use_id"], "a");
        assert_eq!(blocks[0]["content"], "result A");
        assert_eq!(blocks[1]["tool_use_id"], "b");
        assert_eq!(blocks[1]["content"], "result B");
        assert_eq!(wire[3]["role"], "assistant");
        assert_eq!(wire[4]["role"], "user");
    }

    #[test]
    fn separate_tool_runs_stay_separate() {
        let messages = vec![
            Message::assistant_with_tools(
                None,
                vec![ToolCall { id: "a".into(), name: "t".into(), arguments: json!({}) }],
            ),
            Message::tool_result("a", "r1"),
            Message::assistant_with_tools(
                None,
                vec![ToolCall { id: "b".into(), name: "t".into(), arguments: json!({}) }],
            ),
            Message::tool_result("b", "r2"),
        ];
        let body = build_request_body("m", &req("s", messages, vec![]));
        let wire = body["messages"].as_array().unwrap();
        assert_eq!(wire.len(), 4);
        assert_eq!(wire[1]["role"], "user");
        assert_eq!(wire[1]["content"].as_array().unwrap().len(), 1);
        assert_eq!(wire[3]["role"], "user");
        assert_eq!(wire[3]["content"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn parses_text_and_tool_use_response() {
        let v = json!({
            "content": [
                {"type": "text", "text": "Checking the engine."},
                {"type": "tool_use", "id": "toolu_9", "name": "best_move",
                 "input": {"fen": "startpos"}},
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 100, "output_tokens": 20},
        });
        let resp = parse_response(&v).unwrap();
        assert_eq!(resp.text.as_deref(), Some("Checking the engine."));
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].id, "toolu_9");
        assert_eq!(resp.tool_calls[0].name, "best_move");
        assert_eq!(resp.tool_calls[0].arguments, json!({"fen": "startpos"}));
        assert_eq!(resp.usage.input_tokens, 100);
        assert_eq!(resp.usage.output_tokens, 20);
    }

    #[test]
    fn cache_tokens_count_toward_input_usage() {
        let v = json!({
            "content": [{"type": "text", "text": "hi"}],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5,
                "cache_read_input_tokens": 900,
                "cache_creation_input_tokens": 40,
            },
        });
        let resp = parse_response(&v).unwrap();
        assert_eq!(resp.usage.input_tokens, 950);
        assert_eq!(resp.usage.output_tokens, 5);
    }

    #[test]
    fn missing_content_is_malformed() {
        assert!(matches!(
            parse_response(&json!({"usage": {}})),
            Err(LlmError::Malformed(_))
        ));
    }

    // --- streaming ---

    /// Run synthetic SSE bytes (split into arbitrary chunks) through the
    /// line buffer + accumulator pipeline, collecting emitted text deltas.
    fn run_stream(
        chunks: &[&[u8]],
    ) -> Result<(Vec<String>, CompletionResponse), LlmError> {
        let mut lines = crate::llm::sse::SseLineBuffer::new();
        let mut acc = StreamAccumulator::new();
        let mut deltas = Vec::new();
        for chunk in chunks {
            for line in lines.feed(chunk) {
                if let Some(payload) = crate::llm::sse::sse_data_payload(&line) {
                    if let Some(delta) = acc.process(payload)? {
                        deltas.push(delta);
                    }
                }
            }
        }
        Ok((deltas, acc.finish()))
    }

    #[test]
    fn streams_text_only_reply() {
        let sse = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":25,\"output_tokens\":1}}}\n",
            "\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n",
            "\n",
            "event: ping\n",
            "data: {\"type\":\"ping\"}\n",
            "\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Nice \"}}\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"move.\"}}\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":7}}\n",
            "data: {\"type\":\"message_stop\"}\n",
        );
        let (deltas, resp) = run_stream(&[sse.as_bytes()]).unwrap();
        assert_eq!(deltas, vec!["Nice ", "move."]);
        assert_eq!(resp.text.as_deref(), Some("Nice move."));
        assert!(resp.tool_calls.is_empty());
        assert_eq!(resp.usage.input_tokens, 25);
        assert_eq!(resp.usage.output_tokens, 7);
    }

    #[test]
    fn assembles_tool_use_input_from_partial_json_fragments() {
        let sse = concat!(
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10}}}\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"best_move\",\"input\":{}}}\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"fen\\\"\"}}\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\": \\\"start\"}}\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"pos\\\"}\"}}\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":12}}\n",
            "data: {\"type\":\"message_stop\"}\n",
        );
        let (deltas, resp) = run_stream(&[sse.as_bytes()]).unwrap();
        assert!(deltas.is_empty(), "tool input fragments must not be emitted as text");
        assert!(resp.text.is_none());
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].id, "toolu_1");
        assert_eq!(resp.tool_calls[0].name, "best_move");
        assert_eq!(resp.tool_calls[0].arguments, json!({"fen": "startpos"}));
        assert_eq!(resp.usage.output_tokens, 12);
    }

    #[test]
    fn interleaves_text_and_tool_blocks_in_order() {
        let sse = concat!(
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":5}}}\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Let me check\"}}\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n",
            "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_a\",\"name\":\"eval_pos\",\"input\":{}}}\n",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"depth\\\": 12}\"}}\n",
            "data: {\"type\":\"content_block_stop\",\"index\":1}\n",
            "data: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n",
            "data: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"text_delta\",\"text\":\" with the engine.\"}}\n",
            "data: {\"type\":\"content_block_stop\",\"index\":2}\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":9}}\n",
            "data: {\"type\":\"message_stop\"}\n",
        );
        let (deltas, resp) = run_stream(&[sse.as_bytes()]).unwrap();
        assert_eq!(deltas, vec!["Let me check", " with the engine."]);
        assert_eq!(resp.text.as_deref(), Some("Let me check with the engine."));
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].id, "toolu_a");
        assert_eq!(resp.tool_calls[0].arguments, json!({"depth": 12}));
    }

    #[test]
    fn empty_tool_input_defaults_to_empty_object() {
        let sse = concat!(
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_e\",\"name\":\"no_args\",\"input\":{}}}\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n",
            "data: {\"type\":\"message_stop\"}\n",
        );
        let (_, resp) = run_stream(&[sse.as_bytes()]).unwrap();
        assert_eq!(resp.tool_calls[0].arguments, json!({}));
    }

    #[test]
    fn malformed_tool_input_repairs_to_empty_object() {
        let sse = concat!(
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_m\",\"name\":\"t\",\"input\":{}}}\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{oops\"}}\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n",
            "data: {\"type\":\"message_stop\"}\n",
        );
        let (_, resp) = run_stream(&[sse.as_bytes()]).unwrap();
        assert_eq!(resp.tool_calls[0].arguments, json!({}));
    }

    #[test]
    fn error_event_surfaces_as_api_error() {
        let sse = concat!(
            "event: error\n",
            "data: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n",
        );
        let err = run_stream(&[sse.as_bytes()]).unwrap_err();
        match err {
            LlmError::Api { body, .. } => assert!(body.contains("overloaded_error")),
            other => panic!("expected Api error, got {other:?}"),
        }
    }

    #[test]
    fn stream_usage_includes_cache_tokens() {
        let sse = concat!(
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10,\"cache_read_input_tokens\":900,\"cache_creation_input_tokens\":40,\"output_tokens\":1}}}\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}\n",
            "data: {\"type\":\"message_stop\"}\n",
        );
        let (_, resp) = run_stream(&[sse.as_bytes()]).unwrap();
        assert_eq!(resp.usage.input_tokens, 950);
        assert_eq!(resp.usage.output_tokens, 5);
    }

    #[test]
    fn payload_split_across_chunks_and_unknown_events_ignored() {
        let (deltas, resp) = run_stream(&[
            b"data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"te",
            b"xt\",\"text\":\"\"}}\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_del",
            b"ta\",\"text\":\"Hello\"}}\ndata: {\"type\":\"some_future_event\"}\ndata: {\"type\":\"message_stop\"}\n",
        ])
        .unwrap();
        assert_eq!(deltas, vec!["Hello"]);
        assert_eq!(resp.text.as_deref(), Some("Hello"));
    }
}
