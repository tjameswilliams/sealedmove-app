//! coach-core — the shared, cross-platform heart of the chess coach.
//!
//! Everything platform-agnostic lives here: game state, engine orchestration,
//! the LLM backend abstraction, tool schemas, the coach agent loop, and the
//! student memory/curriculum model. The iOS (SwiftUI) and Android (Compose)
//! apps are thin rendering layers over this crate, connected via UniFFI.
//!
//! Design principle: **the LLM never analyzes the board; it narrates what the
//! engine computed.** Every factual claim the coach makes must be grounded in
//! a tool result (engine analysis, motif detection, opening lookup) produced
//! by deterministic code in this crate.
//!
//! Module map:
//! - [`llm`]     — `CoachModel` trait + backends (OpenAI-compat HTTP now;
//!   Apple Foundation Models via FFI delegate and native Anthropic later)
//! - [`engine`]  — UCI process wrapper (Stockfish analyst, Maia opponent),
//!   MultiPV parsing, move classification
//! - [`game`]    — board state, SAN/UCI handling, verbalization for TTS,
//!   motif detection, opening identification
//! - [`coach`]   — tool schemas, system prompts, the agent loop
//! - [`student`] — persistent student profile: concept graph + rating estimate
//! - [`store`]   — SQLite persistence for games/moves/chat + in-progress resume

pub mod coach;
pub mod engine;
pub mod game;
pub mod llm;
pub mod store;
pub mod student;

/// Which side a player has. Re-exported because it appears in this crate's
/// public API (the whole-game review takes the student's color), so callers
/// need not depend on shakmaty to name it.
pub use shakmaty::Color;
