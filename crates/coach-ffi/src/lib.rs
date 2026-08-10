//! coach-ffi — the UniFFI surface of the chess coach.
//!
//! This crate is the boundary the iOS (SwiftUI) and Android (Compose) apps
//! talk to. It exposes [`session::CoachSessionHandle`] (a blocking wrapper
//! around `coach_core::coach::CoachSession`) and the foreign-implementable
//! [`foreign::ForeignCoachModel`] trait, which is how Swift plugs Apple
//! Foundation Models (an on-device, Swift-only API) in as the LLM backend:
//! Swift implements the trait, Rust calls back across the FFI boundary.
//!
//! # Design decisions (v1)
//!
//! **Blocking surface.** Every exported method is synchronous: the crate owns
//! a lazily-created global tokio runtime and each call does
//! `RUNTIME.block_on(...)`. Platform apps must call from background
//! threads/queues (a `DispatchQueue` on iOS, `Dispatchers.IO` on Android),
//! never from the main thread. Exporting async fns can come later; blocking
//! is simpler and portable across binding targets.
//!
//! **JSON payloads.** Complex values cross the boundary as JSON strings
//! (verdicts, summaries, profiles, and the request/response of
//! `ForeignCoachModel::complete`). Tradeoff: we lose per-field type safety at
//! the boundary and pay a small serialize/parse cost, but the surface stays
//! tiny and stable while the core types are still evolving — the platform
//! apps already need JSON codecs for persistence anyway. Typed
//! `uniffi::Record` mirrors of these payloads are the planned v2 upgrade.
//!
//! **Errors.** Everything maps into one flat [`FfiError`] enum with a
//! message string per domain (engine / game / llm / serialization).

uniffi::setup_scaffolding!();

pub mod board;
pub mod foreign;
pub mod session;

use std::sync::LazyLock;
use tokio::runtime::Runtime;

/// The crate-global runtime every blocking export runs on.
pub(crate) static RUNTIME: LazyLock<Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("coach-ffi")
        .enable_all()
        .build()
        .expect("failed to build coach-ffi tokio runtime")
});

/// One error type for the whole FFI surface, mapped from coach-core errors.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum FfiError {
    #[error("engine error: {msg}")]
    Engine { msg: String },
    #[error("game error: {msg}")]
    Game { msg: String },
    #[error("llm error: {msg}")]
    Llm { msg: String },
    #[error("serialization error: {msg}")]
    Serialization { msg: String },
    #[error("store error: {msg}")]
    Store { msg: String },
}

impl From<coach_core::coach::CoachError> for FfiError {
    fn from(e: coach_core::coach::CoachError) -> Self {
        use coach_core::coach::CoachError;
        match e {
            CoachError::Llm(e) => FfiError::Llm { msg: e.to_string() },
            CoachError::Engine(e) => FfiError::Engine { msg: e.to_string() },
            CoachError::Game(e) => FfiError::Game { msg: e.to_string() },
            CoachError::Store(e) => FfiError::Store { msg: e.to_string() },
            e @ CoachError::ToolLoopExceeded(_) => FfiError::Llm { msg: e.to_string() },
        }
    }
}

impl From<coach_core::store::StoreError> for FfiError {
    fn from(e: coach_core::store::StoreError) -> Self {
        FfiError::Store { msg: e.to_string() }
    }
}

impl From<coach_core::game::GameError> for FfiError {
    fn from(e: coach_core::game::GameError) -> Self {
        FfiError::Game { msg: e.to_string() }
    }
}

impl From<std::io::Error> for FfiError {
    fn from(e: std::io::Error) -> Self {
        FfiError::Engine { msg: e.to_string() }
    }
}

impl From<serde_json::Error> for FfiError {
    fn from(e: serde_json::Error) -> Self {
        FfiError::Serialization { msg: e.to_string() }
    }
}

/// Required so a foreign `ForeignCoachModel` implementation that panics or
/// throws something unexpected surfaces as a regular error instead of
/// aborting.
impl From<uniffi::UnexpectedUniFFICallbackError> for FfiError {
    fn from(e: uniffi::UnexpectedUniFFICallbackError) -> Self {
        FfiError::Llm {
            msg: format!("foreign model callback failed: {e}"),
        }
    }
}

/// Render a SAN move as speakable English for the TTS pipeline
/// ("Nf3" → "knight to f3").
#[uniffi::export]
pub fn speak_san(san: String) -> String {
    coach_core::game::speak_san(&san)
}

/// The coach-ffi crate version.
#[uniffi::export]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}
