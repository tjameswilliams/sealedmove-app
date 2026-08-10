//! Thin wrapper over shakmaty: one authoritative game state per session.

use serde::{Deserialize, Serialize};
use shakmaty::{
    fen::Fen, san::San, uci::UciMove, ByColor, ByRole, CastlingMode, Chess, Color, EnPassantMode,
    File, Move, Outcome, Position, Rank, Role, Square,
};

#[derive(Debug, thiserror::Error)]
pub enum GameError {
    #[error("invalid FEN: {0}")]
    Fen(String),
    #[error("invalid move '{0}': {1}")]
    Move(String, String),
}

/// Captured-material picture of the current position, for the app's
/// discarded-pieces tray.
///
/// The captured lists are the STARTING piece set of the game (per color)
/// minus what is on the board now, clamped at zero per role. Promotion can
/// leave a color with MORE of a role than it started with (two queens): the
/// surplus role then simply reports zero captured — the captured lists stay
/// non-negative — while the extra queen still counts at full value in
/// `material_diff`, which is computed from the pieces ON the board (never
/// from the captured lists), so promotions are always valued correctly.
/// The promoted pawn itself is listed as a missing pawn: it did leave the
/// board (over the board it lands in the tray at promotion).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaterialSummary {
    /// Black pieces no longer on the board (White's captures), as lowercase
    /// piece letters sorted most-valuable-first, e.g. `['q','r','n','p']`.
    pub captured_by_white: Vec<char>,
    /// White pieces no longer on the board (Black's captures), as uppercase
    /// piece letters sorted most-valuable-first, e.g. `['Q','P','P']`.
    pub captured_by_black: Vec<char>,
    /// On-board material balance in pawn units (P=1, N=3, B=3, R=5, Q=9);
    /// positive means White is ahead.
    pub material_diff: i32,
}

/// The current game: position plus move history in SAN.
#[derive(Debug, Clone)]
pub struct GameState {
    pos: Chess,
    history_san: Vec<String>,
    /// Piece counts of the STARTING position of this game (standard start
    /// for `new()`, the given position for `from_fen`) — the baseline the
    /// captured-pieces lists are diffed against.
    initial_material: ByColor<ByRole<u8>>,
}

impl Default for GameState {
    fn default() -> Self {
        Self::new()
    }
}

impl GameState {
    /// Standard starting position.
    pub fn new() -> Self {
        let pos = Chess::default();
        let initial_material = pos.board().material();
        Self {
            pos,
            history_san: Vec::new(),
            initial_material,
        }
    }

    pub fn from_fen(fen: &str) -> Result<Self, GameError> {
        let parsed: Fen = fen.parse().map_err(|e| GameError::Fen(format!("{e}")))?;
        let pos: Chess = parsed
            .into_position(CastlingMode::Standard)
            .map_err(|e| GameError::Fen(format!("{e}")))?;
        let initial_material = pos.board().material();
        Ok(Self {
            pos,
            history_san: Vec::new(),
            initial_material,
        })
    }

    pub fn fen(&self) -> String {
        Fen::from_position(self.pos.clone(), EnPassantMode::Legal).to_string()
    }

    pub fn turn(&self) -> Color {
        self.pos.turn()
    }

    pub fn position(&self) -> &Chess {
        &self.pos
    }

    pub fn history_san(&self) -> &[String] {
        &self.history_san
    }

    pub fn is_game_over(&self) -> bool {
        self.pos.is_game_over()
    }

    /// Human-readable result, once the game is over.
    pub fn outcome_text(&self) -> Option<String> {
        match self.pos.outcome()? {
            Outcome::Decisive { winner: Color::White } => Some("1-0 — White wins".into()),
            Outcome::Decisive { winner: Color::Black } => Some("0-1 — Black wins".into()),
            Outcome::Draw => Some("½-½ — draw".into()),
        }
    }

    /// Unicode board diagram for terminal display (rank 8 at the top).
    pub fn board_diagram(&self) -> String {
        let board = self.pos.board();
        let mut out = String::new();
        for rank_idx in (0..8).rev() {
            out.push_str(&format!("{} ", rank_idx + 1));
            for file_idx in 0..8 {
                let sq = Square::from_coords(File::new(file_idx), Rank::new(rank_idx));
                let glyph = board.piece_at(sq).map(unicode_piece).unwrap_or('·');
                out.push(glyph);
                out.push(' ');
            }
            out.push('\n');
        }
        out.push_str("  a b c d e f g h");
        out
    }

    /// `true` when it is White to move.
    pub fn turn_white(&self) -> bool {
        self.pos.turn() == Color::White
    }

    /// All legal moves in the current position, rendered as SAN.
    pub fn legal_moves_san(&self) -> Vec<String> {
        self.pos
            .legal_moves()
            .iter()
            .map(|m| San::from_move(&self.pos, m).to_string())
            .collect()
    }

    /// Destination squares (algebraic, e.g. "e4") for the piece on `from`.
    ///
    /// Castling is reported as the king's destination square (g1/c1/g8/c8),
    /// matching the standard-mode UCI form. Promotions are deduplicated to a
    /// single destination. Returns an empty vec if `from` is not a valid
    /// square, is empty, or holds a piece with no legal moves.
    pub fn legal_destinations(&self, from: &str) -> Vec<String> {
        let Ok(sq) = from.parse::<Square>() else {
            return Vec::new();
        };
        let mut out: Vec<String> = Vec::new();
        for m in self.pos.legal_moves() {
            let dest = match m {
                Move::Castle { king, rook } if king == sq => {
                    let file = if rook.file() > king.file() {
                        File::G
                    } else {
                        File::C
                    };
                    Square::from_coords(file, king.rank())
                }
                _ if m.from() == Some(sq) => m.to(),
                _ => continue,
            };
            let s = dest.to_string();
            if !out.contains(&s) {
                out.push(s);
            }
        }
        out
    }

    /// Play a move given in SAN ("Nf3", "exd5", "O-O").
    pub fn play_san(&mut self, s: &str) -> Result<(), GameError> {
        let san: San = s
            .parse()
            .map_err(|e| GameError::Move(s.to_string(), format!("{e}")))?;
        let m = san
            .to_move(&self.pos)
            .map_err(|e| GameError::Move(s.to_string(), format!("{e}")))?;
        self.pos = self
            .pos
            .clone()
            .play(&m)
            .map_err(|e| GameError::Move(s.to_string(), format!("{e}")))?;
        self.history_san.push(s.trim_end_matches(['!', '?']).to_string());
        Ok(())
    }

    /// Captured pieces per side plus the on-board material balance — see
    /// [`MaterialSummary`] for the exact semantics (including how
    /// promotions are handled).
    pub fn material_summary(&self) -> MaterialSummary {
        let current = self.pos.board().material();
        // Most-valuable-first; King never leaves the board.
        const ORDER: [(Role, i32); 5] = [
            (Role::Queen, 9),
            (Role::Rook, 5),
            (Role::Bishop, 3),
            (Role::Knight, 3),
            (Role::Pawn, 1),
        ];
        let mut captured_by_white = Vec::new();
        let mut captured_by_black = Vec::new();
        let mut material_diff: i32 = 0;
        for (role, value) in ORDER {
            let now_white = *current.white.get(role);
            let now_black = *current.black.get(role);
            // Clamped at zero: promotion can put MORE queens on the board
            // than the game started with.
            let missing_black = self.initial_material.black.get(role).saturating_sub(now_black);
            let missing_white = self.initial_material.white.get(role).saturating_sub(now_white);
            let letter = role.char(); // lowercase
            captured_by_white.extend(std::iter::repeat(letter).take(missing_black as usize));
            captured_by_black
                .extend(std::iter::repeat(letter.to_ascii_uppercase()).take(missing_white as usize));
            material_diff += value * (i32::from(now_white) - i32::from(now_black));
        }
        MaterialSummary {
            captured_by_white,
            captured_by_black,
            material_diff,
        }
    }

    /// Play a move given in UCI ("e2e4") — the format engines emit.
    /// Returns the SAN form for history/verbalization.
    pub fn play_uci(&mut self, s: &str) -> Result<String, GameError> {
        let uci: UciMove = s
            .parse()
            .map_err(|e| GameError::Move(s.to_string(), format!("{e}")))?;
        let m = uci
            .to_move(&self.pos)
            .map_err(|e| GameError::Move(s.to_string(), format!("{e}")))?;
        let san = San::from_move(&self.pos, &m).to_string();
        self.pos = self
            .pos
            .clone()
            .play(&m)
            .map_err(|e| GameError::Move(s.to_string(), format!("{e}")))?;
        self.history_san.push(san.clone());
        Ok(san)
    }
}

/// Unicode glyph for a piece letter as used in [`MaterialSummary`]'s
/// captured lists: uppercase = White ('Q' → '♕'), lowercase = Black
/// ('q' → '♛'). Unknown letters fall back to the pawn glyph of the cased
/// color. A thin public wrapper over the same mapping the board diagram
/// uses.
pub fn piece_glyph(letter: char) -> char {
    let color = if letter.is_ascii_uppercase() {
        Color::White
    } else {
        Color::Black
    };
    let role = Role::from_char(letter.to_ascii_lowercase()).unwrap_or(Role::Pawn);
    unicode_piece(shakmaty::Piece { color, role })
}

fn unicode_piece(p: shakmaty::Piece) -> char {
    use shakmaty::{Color::*, Role::*};
    match (p.color, p.role) {
        (White, King) => '♔',
        (White, Queen) => '♕',
        (White, Rook) => '♖',
        (White, Bishop) => '♗',
        (White, Knight) => '♘',
        (White, Pawn) => '♙',
        (Black, King) => '♚',
        (Black, Queen) => '♛',
        (Black, Rook) => '♜',
        (Black, Bishop) => '♝',
        (Black, Knight) => '♞',
        (Black, Pawn) => '♟',
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plays_a_short_opening() {
        let mut g = GameState::new();
        for m in ["e4", "e5", "Nf3", "Nc6", "Bb5"] {
            g.play_san(m).unwrap();
        }
        assert_eq!(g.history_san().len(), 5);
        assert!(g.fen().starts_with("r1bqkbnr/pppp1ppp/2n5/1B2p3/4P3/5N2/PPPP1PPP/RNBQK2R"));
    }

    #[test]
    fn uci_roundtrips_to_san() {
        let mut g = GameState::new();
        assert_eq!(g.play_uci("g1f3").unwrap(), "Nf3");
    }

    #[test]
    fn rejects_illegal_moves() {
        let mut g = GameState::new();
        assert!(g.play_san("Ke2").is_err());
    }

    #[test]
    fn twenty_legal_moves_from_the_start() {
        let g = GameState::new();
        let moves = g.legal_moves_san();
        assert_eq!(moves.len(), 20);
        assert!(moves.contains(&"e4".to_string()));
        assert!(moves.contains(&"Nf3".to_string()));
    }

    #[test]
    fn pawn_destinations_from_the_start() {
        let g = GameState::new();
        let mut dests = g.legal_destinations("e2");
        dests.sort();
        assert_eq!(dests, vec!["e3".to_string(), "e4".to_string()]);
    }

    #[test]
    fn destinations_empty_for_invalid_or_empty_squares() {
        let g = GameState::new();
        assert!(g.legal_destinations("z9").is_empty());
        assert!(g.legal_destinations("").is_empty());
        assert!(g.legal_destinations("e5").is_empty());
        // Black pawn: not to move, but still has its piece — shakmaty only
        // lists moves for the side to move, so this is empty too.
        assert!(g.legal_destinations("e7").is_empty());
    }

    #[test]
    fn castling_reported_as_king_destination() {
        let g = GameState::from_fen("r3k2r/8/8/8/8/8/8/R3K2R w KQkq - 0 1").unwrap();
        let dests = g.legal_destinations("e1");
        assert!(dests.contains(&"g1".to_string()));
        assert!(dests.contains(&"c1".to_string()));
        // Never the rook squares (shakmaty's internal castle encoding).
        assert!(!dests.contains(&"h1".to_string()));
        assert!(!dests.contains(&"a1".to_string()));
    }

    #[test]
    fn promotion_destinations_deduplicated() {
        let g = GameState::from_fen("8/P7/8/8/8/8/8/k1K5 w - - 0 1").unwrap();
        assert_eq!(g.legal_destinations("a7"), vec!["a8".to_string()]);
    }

    #[test]
    fn material_summary_fresh_board_is_empty_and_even() {
        let g = GameState::new();
        let m = g.material_summary();
        assert!(m.captured_by_white.is_empty());
        assert!(m.captured_by_black.is_empty());
        assert_eq!(m.material_diff, 0);
    }

    #[test]
    fn material_summary_after_capture_sequence() {
        // 1.e4 d5 2.exd5 Qxd5: exd5 takes the d-pawn, Qxd5 takes the
        // e-pawn-now-on-d5 — one pawn each, balance level.
        let mut g = GameState::new();
        for m in ["e4", "d5", "exd5", "Qxd5"] {
            g.play_san(m).unwrap();
        }
        let m = g.material_summary();
        assert_eq!(m.captured_by_white, vec!['p']);
        assert_eq!(m.captured_by_black, vec!['P']);
        assert_eq!(m.material_diff, 0);

        // Halfway through (after 2.exd5) White is a clean pawn up.
        let mut h = GameState::new();
        for m in ["e4", "d5", "exd5"] {
            h.play_san(m).unwrap();
        }
        let half = h.material_summary();
        assert_eq!(half.captured_by_white, vec!['p']);
        assert!(half.captured_by_black.is_empty());
        assert_eq!(half.material_diff, 1);
    }

    #[test]
    fn material_summary_unequal_position_from_fen_baseline_is_that_fen() {
        // Start ALREADY missing material (K+R vs bare K): the FEN itself is
        // the baseline, so nothing counts as captured yet even though most
        // of both armies are gone — but the diff reflects the board.
        let g = GameState::from_fen("4k3/8/8/8/8/8/8/R3K3 w Q - 0 1").unwrap();
        let m = g.material_summary();
        assert!(m.captured_by_white.is_empty());
        assert!(m.captured_by_black.is_empty());
        assert_eq!(m.material_diff, 5); // lone rook up

        // Captures relative to a from_fen start are still tracked: a rook
        // endgame where White's rook takes the black pawn.
        let mut g =
            GameState::from_fen("4k3/4p3/8/8/8/8/8/4K2R w K - 0 1").unwrap();
        assert_eq!(g.material_summary().material_diff, 4);
        for m in ["Rh7", "Kd8", "Rxe7"] {
            g.play_san(m).unwrap();
        }
        let m = g.material_summary();
        assert_eq!(m.captured_by_white, vec!['p']);
        assert!(m.captured_by_black.is_empty());
        assert_eq!(m.material_diff, 5);
    }

    #[test]
    fn material_summary_ordered_most_valuable_first() {
        // 1.e4 d5 2.exd5 Nf6 3.Bb5+ Bd7 4.Bxd7+ Qxd7 — White has taken a
        // pawn AND a bishop; the bishop must list first.
        let mut g = GameState::new();
        for m in ["e4", "d5", "exd5", "Nf6", "Bb5+", "Bd7", "Bxd7+", "Qxd7"] {
            g.play_san(m).unwrap();
        }
        let m = g.material_summary();
        assert_eq!(m.captured_by_white, vec!['b', 'p']);
        assert_eq!(m.captured_by_black, vec!['B']);
        assert_eq!(m.material_diff, 1); // the extra pawn
    }

    #[test]
    fn material_summary_promotion_surplus_queen_counts_on_board() {
        // White promotes: two white queens on the board, none captured.
        let mut g = GameState::from_fen("8/P6k/8/8/8/8/8/K2Q4 w - - 0 1").unwrap();
        g.play_san("a8=Q").unwrap();
        let m = g.material_summary();
        assert!(m.captured_by_white.is_empty());
        // No SURPLUS queen is ever reported captured (clamped at zero) —
        // but the promoted pawn did leave the board, so it lists as a
        // missing white pawn.
        assert_eq!(m.captured_by_black, vec!['P']);
        // The pawn became a queen: 9 + 9 = 18 on-board for White. The diff
        // comes from the board, so the surplus queen counts at full value.
        assert_eq!(m.material_diff, 18);
    }

    #[test]
    fn material_summary_serializes_as_expected_json() {
        let mut g = GameState::new();
        for m in ["e4", "d5", "exd5"] {
            g.play_san(m).unwrap();
        }
        let json = serde_json::to_string(&g.material_summary()).unwrap();
        assert_eq!(
            json,
            r#"{"captured_by_white":["p"],"captured_by_black":[],"material_diff":1}"#
        );
    }

    #[test]
    fn piece_glyph_maps_case_to_color() {
        assert_eq!(piece_glyph('q'), '♛');
        assert_eq!(piece_glyph('Q'), '♕');
        assert_eq!(piece_glyph('p'), '♟');
        assert_eq!(piece_glyph('P'), '♙');
        assert_eq!(piece_glyph('n'), '♞');
        assert_eq!(piece_glyph('K'), '♔');
    }

    #[test]
    fn turn_white_flips() {
        let mut g = GameState::new();
        assert!(g.turn_white());
        g.play_san("e4").unwrap();
        assert!(!g.turn_white());
    }
}
