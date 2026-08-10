//! The LLM backend abstraction.
//!
//! `CoachModel` is *our* interface — the OpenAI wire format is one adapter
//! behind it, not the interface itself. Planned backends:
//!
//! | Backend | Status | Notes |
//! |---|---|---|
//! | [`openai_compat::OpenAiCompatBackend`] | implemented | BYOLLM: `{base_url, api_key, model}` covers OpenAI, OpenRouter, Groq, Ollama, LM Studio, Mistral, DeepSeek, Gemini/Anthropic compat endpoints |
//! | [`anthropic::AnthropicBackend`] | implemented | proxied paid tier — prompt caching on the stable system prefix (cache reads bill ~0.1x on repeat requests); thinking/effort control still TODO |
//! | Apple Foundation Models | planned | offline tier; Swift implements this trait via UniFFI foreign-trait support, Rust calls back across the FFI boundary |
//! | Gemini Nano (AICore) | planned | Android offline tier, same delegate pattern |

pub mod anthropic;
pub mod openai_compat;
pub(crate) mod sse;
pub mod stream;
mod types;

pub use types::*;

use async_trait::async_trait;
use std::sync::Arc;

/// A chat model capable of driving the coach: text in/out plus tool calling.
///
/// Implementations must be `Send + Sync` — the session holds one behind an
/// `Arc` and the FFI layer will call in from platform threads.
///
/// Streaming note: `complete` returns the full response. `complete_stream`
/// delivers text incrementally (pair it with [`stream::SentenceChunker`] to
/// feed TTS sentence-by-sentence); its default implementation falls back to
/// `complete`, so backends without streaming support keep working.
#[async_trait]
pub trait CoachModel: Send + Sync {
    /// Static description of what this backend can do. The coach uses this to
    /// pick the tool set and prompt tier (full vs. compact).
    fn capabilities(&self) -> Capabilities;

    /// One model turn: given the conversation so far, return text and/or tool
    /// calls. The caller (the coach loop) executes tools and calls again.
    async fn complete(&self, req: &CompletionRequest) -> Result<CompletionResponse, LlmError>;

    /// Streaming variant of [`CoachModel::complete`], feeding the voice/TTS
    /// pipeline: text is delivered incrementally on `sink` as
    /// [`StreamEvent::TextDelta`]s while the full assembled response is
    /// still returned at the end.
    ///
    /// The default implementation falls back to [`CoachModel::complete`] and
    /// sends the whole text as a single delta, so non-streaming backends keep
    /// working unchanged. An `mpsc` sender (rather than returning a `Stream`)
    /// keeps the trait object-safe for `Arc<dyn CoachModel>`.
    ///
    /// A dropped receiver is not an error: delivery is best-effort and the
    /// completed response is still returned.
    async fn complete_stream(
        &self,
        req: &CompletionRequest,
        sink: tokio::sync::mpsc::Sender<StreamEvent>,
    ) -> Result<CompletionResponse, LlmError> {
        let resp = self.complete(req).await?;
        if let Some(text) = &resp.text {
            if !text.is_empty() {
                // Receiver may have gone away; that only means nobody is
                // listening to the stream, not that the completion failed.
                let _ = sink.send(StreamEvent::TextDelta(text.clone())).await;
            }
        }
        Ok(resp)
    }
}

/// Serializable backend configuration — this is what the apps persist
/// (API keys themselves go in the platform keychain, referenced by id).
#[derive(Debug, Clone)]
pub enum BackendConfig {
    OpenAiCompat {
        base_url: String,
        api_key: Option<String>,
        model: String,
        capabilities: Option<Capabilities>,
    },
    /// Native Anthropic backend (paid tier): prompt caching on the stable
    /// system prefix, `input_schema` tool wire format.
    Anthropic {
        api_key: String,
        model: String,
        /// Defaults to `https://api.anthropic.com` when `None`.
        base_url: Option<String>,
    },
    // AppleFoundationModels   — provided by the Swift side as a foreign trait impl
}

/// A backend that never says anything — used for engine-only play, where
/// [`crate::coach::CoachSession`] falls back to templated engine-verdict
/// commentary instead of invoking a model.
pub struct NullBackend;

#[async_trait]
impl CoachModel for NullBackend {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            supports_tools: false,
            context_tokens: 0,
            tier: ModelTier::Compact,
        }
    }

    async fn complete(&self, _req: &CompletionRequest) -> Result<CompletionResponse, LlmError> {
        Ok(CompletionResponse::default())
    }
}

/// Construct a backend from configuration.
pub fn build_backend(cfg: BackendConfig) -> Arc<dyn CoachModel> {
    match cfg {
        BackendConfig::OpenAiCompat {
            base_url,
            api_key,
            model,
            capabilities,
        } => {
            let mut backend = openai_compat::OpenAiCompatBackend::new(base_url, api_key, model);
            if let Some(caps) = capabilities {
                backend = backend.with_capabilities(caps);
            }
            Arc::new(backend)
        }
        BackendConfig::Anthropic {
            api_key,
            model,
            base_url,
        } => {
            let mut backend = anthropic::AnthropicBackend::new(api_key, model);
            if let Some(url) = base_url {
                backend = backend.with_base_url(url);
            }
            Arc::new(backend)
        }
    }
}
