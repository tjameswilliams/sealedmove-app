use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Message roles in the coach conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
    /// A tool result being returned to the model.
    Tool,
}

/// One conversation message. The system prompt is carried separately on
/// [`CompletionRequest`] so backends can place it however their API expects.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    /// Tool calls the assistant made in this turn (assistant messages only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// Which tool call this message answers (tool messages only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    pub fn assistant_with_tools(content: Option<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.unwrap_or_default(),
            tool_calls,
            tool_call_id: None,
        }
    }

    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call_id.into()),
        }
    }
}

/// A tool the model may call. `parameters` is a JSON Schema object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

/// A tool invocation requested by the model. `arguments` is already parsed
/// from the provider's JSON string — backends are responsible for repairing
/// or defaulting malformed argument payloads (BYOLLM means weak local models
/// will be plugged in; the core tolerates them).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

/// One request to the model.
#[derive(Debug, Clone)]
pub struct CompletionRequest {
    /// System prompt. Kept stable across a session where possible — the
    /// native Anthropic backend will put a cache breakpoint after it.
    pub system: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDef>,
}

/// The model's reply: text, tool calls, or both.
#[derive(Debug, Clone, Default)]
pub struct CompletionResponse {
    pub text: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Usage,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// An incremental event emitted while a completion streams.
///
/// Sent over an `mpsc` channel rather than returned as a `Stream` so that
/// [`crate::llm::CoachModel`] stays object-safe behind `Arc<dyn CoachModel>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent {
    /// A fragment of assistant text, in order. Concatenating all deltas
    /// yields the full response text.
    TextDelta(String),
}

/// Prompt/tooling tier. Compact backends (on-device models, small local
/// models) get a shorter system prompt and a reduced tool set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelTier {
    Full,
    Compact,
}

/// What a backend can do. Checked by the coach loop, never by UI code.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Capabilities {
    pub supports_tools: bool,
    pub context_tokens: u32,
    pub tier: ModelTier,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            supports_tools: true,
            context_tokens: 128_000,
            tier: ModelTier::Full,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("api error (status {status}): {body}")]
    Api { status: u16, body: String },
    #[error("malformed response: {0}")]
    Malformed(String),
}
