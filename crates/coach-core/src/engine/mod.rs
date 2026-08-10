//! Engine orchestration.
//!
//! Two engines run side by side in the product:
//! - **Analyst** — Stockfish in MultiPV mode. Source of truth for every
//!   factual claim the coach makes: best lines, evals, move classification.
//! - **Opponent** — Maia (lc0 weights trained on human games at specific
//!   rating bands). Plays like a human at the student's level, including the
//!   instructive mistakes. Both speak UCI, so [`uci::UciEngine`] covers both;
//!   Maia just needs `spawn` pointed at an lc0 binary with the right weights.

pub mod accuracy;
pub mod analysis;
pub mod uci;

pub use accuracy::{estimated_rating_from_acl, GameAccuracy};
pub use analysis::{judge_move, Judgment};
pub use uci::{
    Analysis, ChannelTransport, ProcessTransport, Score, ScoredLine, UciEngine, UciTransport,
};
