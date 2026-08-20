//! `BoardHandle` — engine-free chess logic for the app shells.
//!
//! [`crate::session::CoachSessionHandle`] needs a subprocess Stockfish, which
//! iOS cannot spawn (see the crate README). This handle exposes just the
//! deterministic board layer of coach-core — legality, SAN/UCI, history,
//! outcome — so the iOS app can be board-playable today. It is pure
//! computation (no tokio runtime, no I/O): calls are fast and safe from any
//! thread, including the main thread.

use crate::FfiError;
use coach_core::game::GameState;
use std::sync::{Arc, Mutex, MutexGuard};

/// A thread-safe handle to one chess game (no engine, no coach).
#[derive(uniffi::Object)]
pub struct BoardHandle {
    state: Mutex<GameState>,
}

impl BoardHandle {
    fn lock(&self) -> MutexGuard<'_, GameState> {
        // GameState has no invariants that a panicked writer could break
        // mid-update (moves apply atomically), so recover from poisoning.
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }
}

#[uniffi::export]
impl BoardHandle {
    /// Standard starting position.
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(GameState::new()),
        })
    }

    /// Start from an arbitrary FEN.
    #[uniffi::constructor]
    pub fn from_fen(fen: String) -> Result<Arc<Self>, FfiError> {
        Ok(Arc::new(Self {
            state: Mutex::new(GameState::from_fen(&fen)?),
        }))
    }

    /// Current position as FEN.
    pub fn fen(&self) -> String {
        self.lock().fen()
    }

    /// Play a move given in SAN ("Nf3", "exd5", "O-O").
    pub fn play_san(&self, san: String) -> Result<(), FfiError> {
        Ok(self.lock().play_san(&san)?)
    }

    /// Play a move given in UCI ("e2e4", "e7e8q"). Returns the SAN form.
    pub fn play_uci(&self, uci: String) -> Result<String, FfiError> {
        Ok(self.lock().play_uci(&uci)?)
    }

    /// All legal moves in the current position, as SAN.
    pub fn legal_moves_san(&self) -> Vec<String> {
        self.lock().legal_moves_san()
    }

    /// Destination squares (algebraic) for the piece on `from`; castling is
    /// the king's destination (g1/c1/g8/c8). Empty if none or invalid.
    pub fn legal_destinations(&self, from: String) -> Vec<String> {
        self.lock().legal_destinations(&from)
    }

    /// Moves played so far, in SAN.
    pub fn history_san(&self) -> Vec<String> {
        self.lock().history_san().to_vec()
    }

    /// Whether the game has ended (checkmate, stalemate, insufficient
    /// material, ...).
    pub fn is_game_over(&self) -> bool {
        self.lock().is_game_over()
    }

    /// Human-readable result once the game is over, else `None`.
    pub fn outcome_text(&self) -> Option<String> {
        self.lock().outcome_text()
    }

    /// `true` when it is White to move.
    pub fn turn_white(&self) -> bool {
        self.lock().turn_white()
    }

    /// Captured pieces per side plus the on-board material balance, as
    /// JSON (`MaterialSummary`: `captured_by_white` — Black's lost pieces
    /// as lowercase letters, most valuable first; `captured_by_black` —
    /// White's lost pieces, uppercase; `material_diff` — pawn units,
    /// positive = White ahead, computed from the pieces ON the board so
    /// promotions count at full value).
    pub fn material_summary(&self) -> String {
        serde_json::to_string(&self.lock().material_summary())
            .expect("MaterialSummary serializes")
    }

    /// The opening this move history matches, as JSON (`eco`, `name`,
    /// `matched_plies`), or `None` once the game leaves the book.
    ///
    /// It lives on the board mirror rather than the session on purpose: the
    /// UI redraws the opening name after every move, and a hash lookup on
    /// the mirror never waits on the session's mutex behind a running
    /// engine search.
    pub fn current_opening(&self) -> Option<String> {
        let board = self.lock();
        let opening = coach_core::game::openings::lookup(board.history_san())?;
        serde_json::to_string(&opening).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plays_a_game_through_the_handle() {
        let b = BoardHandle::new();
        assert!(b.turn_white());
        b.play_san("e4".into()).unwrap();
        assert!(!b.turn_white());
        assert_eq!(b.play_uci("e7e5".into()).unwrap(), "e5");
        assert_eq!(b.history_san(), vec!["e4".to_string(), "e5".to_string()]);
        assert!(!b.is_game_over());
        assert!(b.outcome_text().is_none());
        assert!(b.fen().contains("4p3/4P3"));
    }

    #[test]
    fn from_fen_and_destinations() {
        let b = BoardHandle::from_fen(
            "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1".into(),
        )
        .unwrap();
        let mut dests = b.legal_destinations("g1".into());
        dests.sort();
        assert_eq!(dests, vec!["f3".to_string(), "h3".to_string()]);
        assert_eq!(b.legal_moves_san().len(), 20);
    }

    #[test]
    fn from_fen_rejects_garbage() {
        assert!(matches!(
            BoardHandle::from_fen("not a fen".into()),
            Err(FfiError::Game { .. })
        ));
    }

    #[test]
    fn rejects_illegal_moves_as_game_error() {
        let b = BoardHandle::new();
        assert!(matches!(
            b.play_san("Ke2".into()),
            Err(FfiError::Game { .. })
        ));
        assert!(matches!(
            b.play_uci("e2e5".into()),
            Err(FfiError::Game { .. })
        ));
    }

    #[test]
    fn material_summary_json_tracks_captures() {
        let b = BoardHandle::new();
        assert_eq!(
            b.material_summary(),
            r#"{"captured_by_white":[],"captured_by_black":[],"material_diff":0}"#
        );
        for m in ["e4", "d5", "exd5"] {
            b.play_san(m.into()).unwrap();
        }
        assert_eq!(
            b.material_summary(),
            r#"{"captured_by_white":["p"],"captured_by_black":[],"material_diff":1}"#
        );
    }

    #[test]
    fn detects_scholars_mate() {
        let b = BoardHandle::new();
        for m in ["e4", "e5", "Bc4", "Nc6", "Qh5", "Nf6", "Qxf7#"] {
            b.play_san(m.into()).unwrap();
        }
        assert!(b.is_game_over());
        assert_eq!(b.outcome_text().as_deref(), Some("1-0 — White wins"));
    }
}
