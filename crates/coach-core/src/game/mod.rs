//! Board state and everything deterministic we derive from it: SAN/UCI
//! handling, spoken-form verbalization for TTS, tactical motif detection,
//! and opening identification.

pub mod motifs;
pub mod openings;
pub mod state;
pub mod verbalize;

pub use state::{GameError, GameState};
pub use verbalize::speak_san;
