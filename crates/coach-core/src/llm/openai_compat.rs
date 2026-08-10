//! The BYOLLM backend: any provider or local server speaking the OpenAI
//! chat-completions wire format. Configured with just `{base_url, api_key,
//! model}` — covers OpenAI, OpenRouter, Groq, Ollama, LM Studio, Mistral,
//! DeepSeek, and the Gemini/Anthropic compat endpoints.
//!
//! "OpenAI-compatible" is a spectrum, not a spec: tool-call fidelity varies
//! across providers and especially across small local models. This adapter
//! defends accordingly — malformed tool arguments are repaired to `{}` rather
//! than erroring, and the coach loop upstream tolerates responses that never
//! call tools at all.

use super::sse::{sse_data_payload, SseLineBuffer};
use super::{
    Capabilities, CoachModel, CompletionRequest, CompletionResponse, LlmError, Message, Role,
    StreamEvent, ToolCall, Usage,
};
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::mpsc::Sender;

pub struct OpenAiCompatBackend {
    http: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
    model: String,
    caps: Capabilities,
}

impl OpenAiCompatBackend {
    pub fn new(base_url: String, api_key: Option<String>, model: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url,
            api_key,
            model,
            caps: Capabilities::default(),
        }
    }

    pub fn with_capabilities(mut self, caps: Capabilities) -> Self {
        self.caps = caps;
        self
    }

    fn to_wire_message(m: &Message) -> Value {
        match m.role {
            Role::User => json!({ "role": "user", "content": m.content }),
            Role::Assistant => {
                let mut msg = json!({ "role": "assistant" });
                // OpenAI wire format wants `content: null` (not "") when the
                // turn is tool-calls-only.
                msg["content"] = if m.content.is_empty() {
                    Value::Null
                } else {
                    Value::String(m.content.clone())
                };
                if !m.tool_calls.is_empty() {
                    msg["tool_calls"] = Value::Array(
                        m.tool_calls
                            .iter()
                            .map(|tc| {
                                json!({
                                    "id": tc.id,
                                    "type": "function",
                                    "function": {
                                        "name": tc.name,
                                        "arguments": tc.arguments.to_string(),
                                    }
                                })
                            })
                            .collect(),
                    );
                }
                msg
            }
            Role::Tool => json!({
                "role": "tool",
                "tool_call_id": m.tool_call_id.clone().unwrap_or_default(),
                "content": m.content,
            }),
        }
    }

    fn build_body(&self, req: &CompletionRequest, stream: bool) -> Value {
        let mut messages = vec![json!({ "role": "system", "content": req.system })];
        messages.extend(req.messages.iter().map(Self::to_wire_message));

        let mut body = json!({
            "model": self.model,
            "messages": messages,
        });
        if stream {
            body["stream"] = Value::Bool(true);
        }
        if self.caps.supports_tools && !req.tools.is_empty() {
            body["tools"] = Value::Array(
                req.tools
                    .iter()
                    .map(|t| {
                        json!({
                            "type": "function",
                            "function": {
                                "name": t.name,
                                "description": t.description,
                                "parameters": t.parameters,
                            }
                        })
                    })
                    .collect(),
            );
        }
        body
    }

    fn request(&self, body: &Value) -> reqwest::RequestBuilder {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let mut request = self.http.post(url).json(body);
        if let Some(key) = &self.api_key {
            request = request.bearer_auth(key);
        }
        request
    }
}

/// Parse a tool-call `arguments` JSON string, repairing malformed payloads to
/// `{}` — BYOLLM means weak local models will be plugged in.
fn parse_arguments(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|_| json!({}))
}

/// A tool call being assembled from streamed deltas: `id`/`name` arrive on the
/// first delta for an index, `arguments` accumulates as string fragments.
#[derive(Debug, Default)]
struct PartialToolCall {
    id: String,
    name: String,
    arguments: String,
}

/// Accumulates OpenAI chat-completions stream chunks into a full response.
/// Pure (no I/O) so it can be unit-tested against synthetic SSE fixtures.
#[derive(Debug, Default)]
struct StreamAccumulator {
    text: String,
    tool_calls: Vec<PartialToolCall>,
    usage: Usage,
    done: bool,
}

impl StreamAccumulator {
    fn new() -> Self {
        Self::default()
    }

    /// Process one SSE data payload. Returns the text delta to emit, if any.
    fn process(&mut self, payload: &str) -> Option<String> {
        if payload.trim() == "[DONE]" {
            self.done = true;
            return None;
        }
        // Tolerate junk payloads from loosely-compatible providers.
        let v: Value = serde_json::from_str(payload).ok()?;

        // Some providers emit a final usage chunk (empty choices).
        if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
            self.usage = Usage {
                input_tokens: u["prompt_tokens"].as_u64().unwrap_or(0),
                output_tokens: u["completion_tokens"].as_u64().unwrap_or(0),
            };
        }

        let delta = &v["choices"][0]["delta"];
        if let Some(calls) = delta["tool_calls"].as_array() {
            for (pos, call) in calls.iter().enumerate() {
                let index = call["index"].as_u64().unwrap_or(pos as u64) as usize;
                while self.tool_calls.len() <= index {
                    self.tool_calls.push(PartialToolCall::default());
                }
                let partial = &mut self.tool_calls[index];
                if let Some(id) = call["id"].as_str() {
                    partial.id = id.to_string();
                }
                if let Some(name) = call["function"]["name"].as_str() {
                    partial.name = name.to_string();
                }
                if let Some(args) = call["function"]["arguments"].as_str() {
                    partial.arguments.push_str(args);
                }
            }
        }

        let content = delta["content"].as_str()?;
        if content.is_empty() {
            return None;
        }
        self.text.push_str(content);
        Some(content.to_string())
    }

    fn finish(self) -> CompletionResponse {
        let text = if self.text.is_empty() {
            None
        } else {
            Some(self.text)
        };
        let tool_calls = self
            .tool_calls
            .into_iter()
            .map(|p| ToolCall {
                id: p.id,
                name: p.name,
                arguments: parse_arguments(&p.arguments),
            })
            .collect();
        CompletionResponse {
            text,
            tool_calls,
            usage: self.usage,
        }
    }
}

#[async_trait]
impl CoachModel for OpenAiCompatBackend {
    fn capabilities(&self) -> Capabilities {
        self.caps
    }

    async fn complete(&self, req: &CompletionRequest) -> Result<CompletionResponse, LlmError> {
        let body = self.build_body(req, false);
        let response = self.request(&body).send().await?;
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
        let message = &v["choices"][0]["message"];
        if message.is_null() {
            return Err(LlmError::Malformed(
                "response has no choices[0].message".into(),
            ));
        }

        let content = message["content"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        let mut tool_calls = Vec::new();
        if let Some(calls) = message["tool_calls"].as_array() {
            for call in calls {
                let arguments = call["function"]["arguments"]
                    .as_str()
                    .map(parse_arguments)
                    .unwrap_or_else(|| json!({}));
                tool_calls.push(ToolCall {
                    id: call["id"].as_str().unwrap_or_default().to_string(),
                    name: call["function"]["name"].as_str().unwrap_or_default().to_string(),
                    arguments,
                });
            }
        }

        let usage = Usage {
            input_tokens: v["usage"]["prompt_tokens"].as_u64().unwrap_or(0),
            output_tokens: v["usage"]["completion_tokens"].as_u64().unwrap_or(0),
        };

        Ok(CompletionResponse {
            text: content,
            tool_calls,
            usage,
        })
    }

    async fn complete_stream(
        &self,
        req: &CompletionRequest,
        sink: Sender<StreamEvent>,
    ) -> Result<CompletionResponse, LlmError> {
        let body = self.build_body(req, true);
        let mut response = self.request(&body).send().await?;
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
                if let Some(delta) = acc.process(payload) {
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

    /// Run synthetic SSE bytes (split into arbitrary chunks) through the
    /// line buffer + accumulator pipeline, collecting emitted text deltas.
    fn run_stream(chunks: &[&[u8]]) -> (Vec<String>, CompletionResponse) {
        let mut lines = SseLineBuffer::new();
        let mut acc = StreamAccumulator::new();
        let mut deltas = Vec::new();
        for chunk in chunks {
            for line in lines.feed(chunk) {
                if let Some(payload) = sse_data_payload(&line) {
                    if let Some(delta) = acc.process(payload) {
                        deltas.push(delta);
                    }
                }
            }
        }
        (deltas, acc.finish())
    }

    #[test]
    fn accumulates_text_deltas() {
        let sse = b"data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"\"}}]}\n\
                    data: {\"choices\":[{\"delta\":{\"content\":\"Nice \"}}]}\n\
                    data: {\"choices\":[{\"delta\":{\"content\":\"move.\"}}]}\n\
                    data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\
                    data: [DONE]\n";
        let (deltas, resp) = run_stream(&[sse]);
        assert_eq!(deltas, vec!["Nice ", "move."]);
        assert_eq!(resp.text.as_deref(), Some("Nice move."));
        assert!(resp.tool_calls.is_empty());
    }

    #[test]
    fn payload_split_across_chunks_mid_json() {
        let (deltas, resp) = run_stream(&[
            b"data: {\"choices\":[{\"delta\":{\"con",
            b"tent\":\"Hello\"}}]}\ndata: [DONE]\n",
        ]);
        assert_eq!(deltas, vec!["Hello"]);
        assert_eq!(resp.text.as_deref(), Some("Hello"));
    }

    #[test]
    fn assembles_streamed_tool_calls_by_index() {
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_a\",\"function\":{\"name\":\"best_move\",\"arguments\":\"\"}}]}}]}\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"fen\\\":\"}}]}}]}\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"call_b\",\"function\":{\"name\":\"eval_pos\",\"arguments\":\"{}\"}}]}}]}\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"start\\\"}\"}}]}}]}\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n",
            "data: [DONE]\n",
        );
        let (deltas, resp) = run_stream(&[sse.as_bytes()]);
        assert!(deltas.is_empty());
        assert_eq!(resp.tool_calls.len(), 2);
        assert_eq!(resp.tool_calls[0].id, "call_a");
        assert_eq!(resp.tool_calls[0].name, "best_move");
        assert_eq!(resp.tool_calls[0].arguments, json!({"fen": "start"}));
        assert_eq!(resp.tool_calls[1].id, "call_b");
        assert_eq!(resp.tool_calls[1].name, "eval_pos");
        assert_eq!(resp.tool_calls[1].arguments, json!({}));
    }

    #[test]
    fn malformed_streamed_arguments_repair_to_empty_object() {
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c\",\"function\":{\"name\":\"t\",\"arguments\":\"{oops\"}}]}}]}\n",
            "data: [DONE]\n",
        );
        let (_, resp) = run_stream(&[sse.as_bytes()]);
        assert_eq!(resp.tool_calls[0].arguments, json!({}));
    }

    #[test]
    fn stops_at_done_and_captures_usage() {
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":3}}\n",
            "data: [DONE]\n",
        );
        let (_, resp) = run_stream(&[sse.as_bytes()]);
        assert_eq!(resp.usage.input_tokens, 12);
        assert_eq!(resp.usage.output_tokens, 3);
    }

    #[test]
    fn junk_payloads_are_tolerated() {
        let (deltas, resp) = run_stream(&[
            b"data: not json at all\ndata: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\ndata: [DONE]\n",
        ]);
        assert_eq!(deltas, vec!["ok"]);
        assert_eq!(resp.text.as_deref(), Some("ok"));
    }
}
