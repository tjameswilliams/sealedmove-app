//! The foreign-implementable model trait — the key FFI piece.
//!
//! Apple Foundation Models is a Swift-only, on-device API, so the LLM
//! backend must be implementable *on the Swift side* with Rust calling back
//! in. `#[uniffi::export(with_foreign)]` generates exactly that: Swift (or
//! Kotlin, for Gemini Nano) supplies an object conforming to the generated
//! protocol/interface, and [`ForeignModelAdapter`] bridges it into
//! coach-core's `CoachModel`.

use crate::FfiError;
use async_trait::async_trait;
use coach_core::llm::{
    Capabilities, CoachModel, CompletionRequest, CompletionResponse, LlmError, Message,
    ModelTier, ToolCall, ToolDef, Usage,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// A chat model implemented on the foreign (Swift/Kotlin) side.
///
/// Called by Rust from the crate's tokio runtime via `spawn_blocking`, so
/// implementations may block (e.g. wait synchronously on an async Swift
/// API). Implementations must be thread-safe; calls can arrive from any
/// thread. They must NOT call back into `CoachSessionHandle` methods — the
/// session lock is held for the duration of the completion.
#[uniffi::export(with_foreign)]
pub trait ForeignCoachModel: Send + Sync {
    /// Whether the model can do tool calling. If `false` the coach falls
    /// back to engine-verdict-grounded prompting without tools.
    fn supports_tools(&self) -> bool;

    /// Context window size in tokens (used for prompt budgeting).
    fn context_tokens(&self) -> u32;

    /// `true` for small on-device models — selects the compact prompt and
    /// reduced tool set.
    fn compact_tier(&self) -> bool;

    /// One completion turn.
    ///
    /// `request_json` is a JSON object `{system, messages, tools}` matching
    /// coach-core's `CompletionRequest` serialization; the return value is a
    /// JSON object `{text, tool_calls, usage}` matching
    /// `CompletionResponse`. Missing response fields are tolerated — a plain
    /// `{"text": "..."}` is a valid reply.
    fn complete(&self, request_json: String) -> Result<String, FfiError>;
}

/// `CompletionRequest` wire form (`CompletionRequest` itself doesn't derive
/// `Serialize`; its field types all do).
#[derive(Serialize)]
pub(crate) struct WireRequest<'a> {
    pub system: &'a str,
    pub messages: &'a [Message],
    pub tools: &'a [ToolDef],
}

/// `Usage` wire form with per-field defaults so partial objects parse.
#[derive(Default, Deserialize)]
pub(crate) struct WireUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
}

/// `CompletionResponse` wire form. Every field is optional — foreign
/// implementations only report what they have.
#[derive(Default, Deserialize)]
pub(crate) struct WireResponse {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default)]
    pub usage: WireUsage,
}

/// Bridges an `Arc<dyn ForeignCoachModel>` into coach-core's `CoachModel`.
pub(crate) struct ForeignModelAdapter {
    pub inner: Arc<dyn ForeignCoachModel>,
}

#[async_trait]
impl CoachModel for ForeignModelAdapter {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            supports_tools: self.inner.supports_tools(),
            context_tokens: self.inner.context_tokens(),
            tier: if self.inner.compact_tier() {
                ModelTier::Compact
            } else {
                ModelTier::Full
            },
        }
    }

    async fn complete(&self, req: &CompletionRequest) -> Result<CompletionResponse, LlmError> {
        let request_json = serde_json::to_string(&WireRequest {
            system: &req.system,
            messages: &req.messages,
            tools: &req.tools,
        })
        .map_err(|e| LlmError::Malformed(format!("request serialization failed: {e}")))?;

        // Foreign implementations may block; keep them off the async workers.
        let model = Arc::clone(&self.inner);
        let reply_json = tokio::task::spawn_blocking(move || model.complete(request_json))
            .await
            .map_err(|e| LlmError::Malformed(format!("foreign model task failed: {e}")))?
            .map_err(|e| LlmError::Malformed(format!("foreign model error: {e}")))?;

        let wire: WireResponse = serde_json::from_str(&reply_json)
            .map_err(|e| LlmError::Malformed(format!("foreign model returned invalid JSON: {e}")))?;
        Ok(CompletionResponse {
            text: wire.text,
            tool_calls: wire.tool_calls,
            usage: Usage {
                input_tokens: wire.usage.input_tokens,
                output_tokens: wire.usage.output_tokens,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RUNTIME;

    /// A canned foreign model: records nothing, replies with fixed JSON.
    struct CannedModel {
        reply: &'static str,
    }

    impl ForeignCoachModel for CannedModel {
        fn supports_tools(&self) -> bool {
            false
        }
        fn context_tokens(&self) -> u32 {
            4096
        }
        fn compact_tier(&self) -> bool {
            true
        }
        fn complete(&self, request_json: String) -> Result<String, FfiError> {
            // The request must be a valid JSON object with the documented keys.
            let req: serde_json::Value = serde_json::from_str(&request_json)
                .map_err(|e| FfiError::Serialization { msg: e.to_string() })?;
            assert!(req.get("system").is_some());
            assert!(req.get("messages").is_some());
            assert!(req.get("tools").is_some());
            Ok(self.reply.to_string())
        }
    }

    fn request() -> CompletionRequest {
        CompletionRequest {
            system: "You are a chess coach.".into(),
            messages: vec![Message::user("The student played e4.")],
            tools: Vec::new(),
        }
    }

    #[test]
    fn adapter_maps_capabilities() {
        let adapter = ForeignModelAdapter {
            inner: Arc::new(CannedModel { reply: "{}" }),
        };
        let caps = adapter.capabilities();
        assert!(!caps.supports_tools);
        assert_eq!(caps.context_tokens, 4096);
        assert_eq!(caps.tier, ModelTier::Compact);
    }

    #[test]
    fn adapter_round_trips_a_full_response() {
        let adapter = ForeignModelAdapter {
            inner: Arc::new(CannedModel {
                reply: r#"{
                    "text": "Nice opening move!",
                    "tool_calls": [{"id": "c1", "name": "lookup_opening", "arguments": {}}],
                    "usage": {"input_tokens": 10, "output_tokens": 5}
                }"#,
            }),
        };
        let resp = RUNTIME.block_on(adapter.complete(&request())).unwrap();
        assert_eq!(resp.text.as_deref(), Some("Nice opening move!"));
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].name, "lookup_opening");
        assert_eq!(resp.usage.input_tokens, 10);
        assert_eq!(resp.usage.output_tokens, 5);
    }

    #[test]
    fn adapter_tolerates_missing_fields() {
        let adapter = ForeignModelAdapter {
            inner: Arc::new(CannedModel {
                reply: r#"{"text": "just text"}"#,
            }),
        };
        let resp = RUNTIME.block_on(adapter.complete(&request())).unwrap();
        assert_eq!(resp.text.as_deref(), Some("just text"));
        assert!(resp.tool_calls.is_empty());
        assert_eq!(resp.usage.input_tokens, 0);
    }

    #[test]
    fn adapter_reports_invalid_json_as_llm_error() {
        let adapter = ForeignModelAdapter {
            inner: Arc::new(CannedModel { reply: "not json" }),
        };
        let err = RUNTIME.block_on(adapter.complete(&request())).unwrap_err();
        assert!(matches!(err, LlmError::Malformed(_)));
    }
}
