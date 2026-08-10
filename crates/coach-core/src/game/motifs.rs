//! Tactical motif detection — deterministic pattern recognition over the
//! board, so the coach can say "notice your knight is forked" without the
//! LLM ever hallucinating a piece location.
//!
//! These are *static* detectors: they read the current position only, no
//! search. That means occasional over-reporting (an "attacker" that is
//! itself pinned, a "hanging" piece that is tactically defended) — accepted
//! for v1, since the engine's dynamic verdict arrives alongside them. Each
//! detector is a pure function, unit-tested from FEN fixtures.

use serde::{Deserialize, Serialize};
use shakmaty::{attacks, Bitboard, Board, Chess, Color, Position, Role, Square};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MotifKind {
    /// A piece attacked while undefended, or attacked by a cheaper piece.
    HangingPiece,
    /// One piece attacking two or more valuable targets at once.
    Fork,
    /// A piece that cannot move because the king stands behind it.
    Pin,
    /// A valuable piece attacked with a lesser piece behind it on the ray.
    Skewer,
    /// A back rank guarded only by the king, with no escape square.
    BackRankWeakness,
    /// Moving one piece would unleash an attack from the piece behind it.
    DiscoveredAttackAvailable,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MotifInstance {
    pub kind: MotifKind,
    /// Primary square involved, algebraic ("e5").
    pub square: String,
    /// The side under threat ("white" / "black").
    pub against: String,
    /// Human-readable summary the LLM can quote directly.
    pub description: String,
}

/// Detect tactical motifs in the current position, for both colors.
pub fn detect(pos: &Chess) -> Vec<MotifInstance> {
    let board = pos.board();
    let mut out = Vec::new();
    for color in [Color::White, Color::Black] {
        detect_hanging(board, color, &mut out);
        detect_forks(board, color, &mut out);
        detect_pins_and_skewers(board, color, &mut out);
        detect_back_rank(board, color, &mut out);
        detect_discovered(board, color, &mut out);
    }
    out
}

fn value(role: Role) -> i32 {
    match role {
        Role::Pawn => 1,
        Role::Knight | Role::Bishop => 3,
        Role::Rook => 5,
        Role::Queen => 9,
        Role::King => 100,
    }
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::Pawn => "pawn",
        Role::Knight => "knight",
        Role::Bishop => "bishop",
        Role::Rook => "rook",
        Role::Queen => "queen",
        Role::King => "king",
    }
}

fn color_name(c: Color) -> &'static str {
    match c {
        Color::White => "white",
        Color::Black => "black",
    }
}

fn sliders(board: &Board) -> Bitboard {
    board.by_role(Role::Bishop) | board.by_role(Role::Rook) | board.by_role(Role::Queen)
}

/// Pieces of `victim` color that are attacked while undefended, or attacked
/// by something cheaper than they are.
fn detect_hanging(board: &Board, victim: Color, out: &mut Vec<MotifInstance>) {
    let occupied = board.occupied();
    for sq in board.by_color(victim) & !board.by_role(Role::King) {
        let piece = board.piece_at(sq).expect("occupied square");
        let attackers = board.attacks_to(sq, victim.other(), occupied);
        if attackers.is_empty() {
            continue;
        }
        let defenders = board.attacks_to(sq, victim, occupied);
        let cheapest_attacker = attackers
            .into_iter()
            .filter_map(|a| board.piece_at(a))
            .map(|p| value(p.role))
            .min()
            .unwrap_or(i32::MAX);

        if defenders.is_empty() {
            out.push(MotifInstance {
                kind: MotifKind::HangingPiece,
                square: sq.to_string(),
                against: color_name(victim).into(),
                description: format!(
                    "the {} {} on {} is attacked and has no defender",
                    color_name(victim),
                    role_name(piece.role),
                    sq
                ),
            });
        } else if cheapest_attacker < value(piece.role) {
            out.push(MotifInstance {
                kind: MotifKind::HangingPiece,
                square: sq.to_string(),
                against: color_name(victim).into(),
                description: format!(
                    "the {} {} on {} is attacked by a cheaper piece",
                    color_name(victim),
                    role_name(piece.role),
                    sq
                ),
            });
        }
    }
}

/// Pieces of `attacker` color that currently attack two or more valuable
/// enemy targets (king, higher-valued piece, or an undefended equal one).
fn detect_forks(board: &Board, attacker: Color, out: &mut Vec<MotifInstance>) {
    let victim = attacker.other();
    let occupied = board.occupied();
    for sq in board.by_color(attacker) & !board.by_role(Role::King) {
        let piece = board.piece_at(sq).expect("occupied square");
        let attacker_value = value(piece.role);
        let mut targets: Vec<String> = Vec::new();
        for t in board.attacks_from(sq) & board.by_color(victim) {
            let tp = board.piece_at(t).expect("occupied square");
            let defended = !board.attacks_to(t, victim, occupied).is_empty();
            let valuable = tp.role == Role::King
                || value(tp.role) > attacker_value
                || (!defended && value(tp.role) >= attacker_value);
            if valuable {
                targets.push(format!("the {} on {}", role_name(tp.role), t));
            }
        }
        if targets.len() >= 2 {
            out.push(MotifInstance {
                kind: MotifKind::Fork,
                square: sq.to_string(),
                against: color_name(victim).into(),
                description: format!(
                    "the {} {} on {} forks {}",
                    color_name(attacker),
                    role_name(piece.role),
                    sq,
                    targets.join(" and ")
                ),
            });
        }
    }
}

/// Walk each slider's rays through exactly one enemy piece: king behind =
/// pin; lesser piece behind a more valuable front piece = skewer.
fn detect_pins_and_skewers(board: &Board, slider_color: Color, out: &mut Vec<MotifInstance>) {
    let victim = slider_color.other();
    let occupied = board.occupied();
    for ssq in board.by_color(slider_color) & sliders(board) {
        let slider = board.piece_at(ssq).expect("occupied square");
        let direct = attacks::attacks(ssq, slider, occupied);
        for front_sq in direct & board.by_color(victim) {
            let front = board.piece_at(front_sq).expect("occupied square");
            let without_front = occupied & !Bitboard::from(front_sq);
            let extended = attacks::attacks(ssq, slider, without_front);
            for behind_sq in (extended & !direct) & board.by_color(victim) {
                if !attacks::aligned(ssq, front_sq, behind_sq) {
                    continue;
                }
                let behind = board.piece_at(behind_sq).expect("occupied square");
                if behind.role == Role::King {
                    out.push(MotifInstance {
                        kind: MotifKind::Pin,
                        square: front_sq.to_string(),
                        against: color_name(victim).into(),
                        description: format!(
                            "the {} {} on {} is pinned to its king by the {} on {}",
                            color_name(victim),
                            role_name(front.role),
                            front_sq,
                            role_name(slider.role),
                            ssq
                        ),
                    });
                } else if value(front.role) > value(behind.role)
                    && value(front.role) > value(slider.role)
                {
                    out.push(MotifInstance {
                        kind: MotifKind::Skewer,
                        square: front_sq.to_string(),
                        against: color_name(victim).into(),
                        description: format!(
                            "the {} {} on {} is skewered against the {} on {} by the {} on {}",
                            color_name(victim),
                            role_name(front.role),
                            front_sq,
                            role_name(behind.role),
                            behind_sq,
                            role_name(slider.role),
                            ssq
                        ),
                    });
                }
            }
        }
    }
}

/// King stuck on its home rank behind its own pieces, with enemy heavy
/// pieces on the board.
fn detect_back_rank(board: &Board, victim: Color, out: &mut Vec<MotifInstance>) {
    let Some(ksq) = board.king_of(victim) else {
        return;
    };
    let (home, front) = match victim {
        Color::White => (shakmaty::Rank::First, shakmaty::Rank::Second),
        Color::Black => (shakmaty::Rank::Eighth, shakmaty::Rank::Seventh),
    };
    if ksq.rank() != home {
        return;
    }
    let enemy_heavy = board.by_color(victim.other())
        & (board.by_role(Role::Rook) | board.by_role(Role::Queen));
    if enemy_heavy.is_empty() {
        return;
    }

    // The weakness is only real if a heavy piece can actually see the back
    // rank — otherwise every castled king with a healthy pawn shield (and
    // both kings in the starting position) would be flagged.
    let occupied = board.occupied();
    let home_rank_bb = Bitboard::from_rank(home);
    let rank_reachable = enemy_heavy.into_iter().any(|esq| {
        let piece = board.piece_at(esq).expect("occupied square");
        !(attacks::attacks(esq, piece, occupied) & home_rank_bb).is_empty()
    });
    if !rank_reachable {
        return;
    }

    let king_file = u32::from(ksq.file()) as i32;
    let mut escape_exists = false;
    for df in -1..=1 {
        let f = king_file + df;
        if !(0..8).contains(&f) {
            continue;
        }
        let sq = Square::from_coords(shakmaty::File::new(f as u32), front);
        let blocked_by_own = board.by_color(victim).contains(sq);
        if !blocked_by_own {
            escape_exists = true;
            break;
        }
    }
    if !escape_exists {
        out.push(MotifInstance {
            kind: MotifKind::BackRankWeakness,
            square: ksq.to_string(),
            against: color_name(victim).into(),
            description: format!(
                "the {} king on {} has no escape square off the back rank, and the opponent \
                 has heavy pieces that could reach it",
                color_name(victim),
                ksq
            ),
        });
    }
}

/// A piece whose departure would reveal a slider attack on the enemy king,
/// queen, or rook.
fn detect_discovered(board: &Board, mover: Color, out: &mut Vec<MotifInstance>) {
    let victim = mover.other();
    let occupied = board.occupied();
    for msq in board.by_color(mover) & !board.by_role(Role::King) {
        let mover_piece = board.piece_at(msq).expect("occupied square");
        let without_mover = occupied & !Bitboard::from(msq);
        for ssq in (board.by_color(mover) & sliders(board)) & !Bitboard::from(msq) {
            let slider = board.piece_at(ssq).expect("occupied square");
            let before = attacks::attacks(ssq, slider, occupied);
            let after = attacks::attacks(ssq, slider, without_mover);
            for t in (after & !before) & board.by_color(victim) {
                let tp = board.piece_at(t).expect("occupied square");
                if matches!(tp.role, Role::King | Role::Queen | Role::Rook)
                    && attacks::aligned(ssq, msq, t)
                {
                    out.push(MotifInstance {
                        kind: MotifKind::DiscoveredAttackAvailable,
                        square: msq.to_string(),
                        against: color_name(victim).into(),
                        description: format!(
                            "moving the {} {} on {} would reveal an attack from the {} on {} \
                             against the {} on {}",
                            color_name(mover),
                            role_name(mover_piece.role),
                            msq,
                            role_name(slider.role),
                            ssq,
                            role_name(tp.role),
                            t
                        ),
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::GameState;

    fn motifs_for(fen: &str) -> Vec<MotifInstance> {
        let game = GameState::from_fen(fen).unwrap();
        detect(game.position())
    }

    #[test]
    fn detects_hanging_piece() {
        // White knight e3 attacks the undefended black queen on d5.
        let found = motifs_for("8/8/8/3q4/8/4N3/8/K6k w - - 0 1");
        assert!(found
            .iter()
            .any(|m| m.kind == MotifKind::HangingPiece && m.square == "d5" && m.against == "black"));
    }

    #[test]
    fn detects_fork() {
        // White knight on c7 forks the black king (a8) and rook (e8).
        let found = motifs_for("k3r3/2N5/8/8/8/8/8/6K1 b - - 0 1");
        assert!(found
            .iter()
            .any(|m| m.kind == MotifKind::Fork && m.square == "c7" && m.against == "black"));
    }

    #[test]
    fn detects_absolute_pin() {
        // White rook e2 pins the black knight e7 to the king on e8.
        let found = motifs_for("4k3/4n3/8/8/8/8/4R3/4K3 b - - 0 1");
        assert!(found
            .iter()
            .any(|m| m.kind == MotifKind::Pin && m.square == "e7" && m.against == "black"));
    }

    #[test]
    fn detects_back_rank_weakness() {
        // Black king g8 boxed in by its own pawns; white rook on the board.
        // White's king is equally boxed in, but black has no heavy pieces,
        // so only black is flagged.
        let found = motifs_for("6k1/5ppp/8/8/8/8/5PPP/3R2K1 w - - 0 1");
        let back_rank: Vec<_> = found
            .iter()
            .filter(|m| m.kind == MotifKind::BackRankWeakness)
            .collect();
        assert_eq!(back_rank.len(), 1);
        assert_eq!(back_rank[0].against, "black");
    }

    #[test]
    fn detects_discovered_attack() {
        // Moving the white knight on d4 reveals the b2 bishop's attack on
        // the black queen (g7).
        let found = motifs_for("4k3/6q1/8/8/3N4/8/1B6/4K3 w - - 0 1");
        assert!(found
            .iter()
            .any(|m| m.kind == MotifKind::DiscoveredAttackAvailable && m.square == "d4"));
    }

    #[test]
    fn quiet_position_is_quiet() {
        // Starting position: nothing to report.
        let game = GameState::new();
        assert!(detect(game.position()).is_empty());
    }
}
