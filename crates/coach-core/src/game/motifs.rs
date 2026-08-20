//! Tactical motif detection — deterministic pattern recognition over the
//! board, so the coach can say "notice your knight is forked" without the
//! LLM ever hallucinating a piece location.
//!
//! These are *static* detectors: they read the current position only, no
//! search. That means occasional over-reporting (an "attacker" that is
//! itself pinned, a "hanging" piece that is tactically defended) — accepted
//! for v1, since the engine's dynamic verdict arrives alongside them. Each
//! detector is a pure function, unit-tested from FEN fixtures.
//!
//! Detectors fall into two families, separated by [`MotifKind::is_tactical`]:
//!
//! - **Tactical** — something is hanging *right now* (forks, pins, trapped
//!   pieces, mating patterns). Urgent; the coach leads with these.
//! - **Positional** — structure and endgame shape (passed pawns, isolated
//!   pawns, outposts, the opposition, Lucena/Philidor). Instructive rather
//!   than urgent, and true of most positions, so they must never crowd out
//!   a tactic. [`detect`] sorts every result by [`MotifKind::priority`] so
//!   callers that take the first N always get the sharpest findings.

use serde::{Deserialize, Serialize};
use shakmaty::{attacks, Bitboard, Board, Chess, Color, File, Position, Rank, Role, Square};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MotifKind {
    /// The side to move is in check from two pieces at once — only a king
    /// move can answer it.
    DoubleCheck,
    /// A piece attacked while undefended, or attacked by a cheaper piece.
    HangingPiece,
    /// One piece attacking two or more valuable targets at once.
    Fork,
    /// A piece that cannot move because the king stands behind it.
    Pin,
    /// A valuable piece attacked with a lesser piece behind it on the ray.
    Skewer,
    /// An attacked piece with no square to run to.
    TrappedPiece,
    /// The only defender of an attacked piece is itself capturable.
    RemovingTheDefender,
    /// One piece is the sole defender of two attacked units — it cannot
    /// hold both.
    OverloadedDefender,
    /// A back rank guarded only by the king, with no escape square.
    BackRankWeakness,
    /// The king's flight squares are all blocked by its own pieces, with an
    /// enemy knight on the board.
    SmotheredKing,
    /// Moving one piece would unleash an attack from the piece behind it.
    DiscoveredAttackAvailable,
    /// Two friendly sliders stacked on one line, doubling their power.
    Battery,
    /// A pawn with no enemy pawn able to stop it on its file or either
    /// neighbour.
    PassedPawn,
    /// A passed pawn far from the enemy pawn mass — the classic winning
    /// endgame asset.
    OutsidePassedPawn,
    /// A minor piece on a protected square no enemy pawn can ever attack.
    Outpost,
    /// One side holds both bishops while the other does not.
    BishopPair,
    /// A pawn with no friendly pawn on either adjacent file.
    IsolatedPawn,
    /// Two or more friendly pawns stacked on one file.
    DoubledPawns,
    /// A pawn left behind its neighbours, its advance square covered by an
    /// enemy pawn.
    BackwardPawn,
    /// Kings facing each other with one square between, in a pawn endgame.
    Opposition,
    /// Rook endgame, pawn on the seventh, king shepherding it home.
    LucenaPosition,
    /// Rook endgame, defender's rook holding the third rank.
    PhilidorPosition,
}

impl MotifKind {
    /// Sort weight — lower speaks first. Callers that show only the top few
    /// findings get the sharpest ones: a fork outranks a doubled pawn.
    pub fn priority(self) -> u8 {
        match self {
            Self::DoubleCheck => 0,
            Self::SmotheredKing => 1,
            Self::BackRankWeakness => 2,
            Self::Fork => 3,
            Self::HangingPiece => 4,
            Self::TrappedPiece => 5,
            Self::Pin => 6,
            Self::Skewer => 7,
            Self::RemovingTheDefender => 8,
            Self::OverloadedDefender => 9,
            Self::DiscoveredAttackAvailable => 10,
            Self::Battery => 11,
            // Positional findings from here down.
            Self::LucenaPosition => 20,
            Self::PhilidorPosition => 21,
            Self::OutsidePassedPawn => 22,
            Self::PassedPawn => 23,
            Self::Opposition => 24,
            Self::Outpost => 25,
            Self::BishopPair => 26,
            Self::BackwardPawn => 27,
            Self::IsolatedPawn => 28,
            Self::DoubledPawns => 29,
        }
    }

    /// Tactical motifs demand an answer this move; positional ones describe
    /// the shape of the position. Consumers word the two differently.
    pub fn is_tactical(self) -> bool {
        self.priority() < 20
    }

    /// Lexicon slug the app links this motif to, when it has an entry.
    pub fn slug(self) -> &'static str {
        match self {
            Self::DoubleCheck => "double-check",
            Self::HangingPiece => "hanging-piece",
            Self::Fork => "fork",
            Self::Pin => "pin",
            Self::Skewer => "skewer",
            Self::TrappedPiece => "trapped-piece",
            Self::RemovingTheDefender => "removing-the-defender",
            Self::OverloadedDefender => "overloading",
            Self::BackRankWeakness => "back-rank-mate",
            Self::SmotheredKing => "smothered-mate",
            Self::DiscoveredAttackAvailable => "discovered-attack",
            Self::Battery => "battery",
            Self::PassedPawn => "passed-pawn",
            Self::OutsidePassedPawn => "outside-passed-pawn",
            Self::Outpost => "outpost",
            Self::BishopPair => "bishop-pair",
            Self::IsolatedPawn => "isolated-pawn",
            Self::DoubledPawns => "doubled-pawns",
            Self::BackwardPawn => "backward-pawn",
            Self::Opposition => "opposition",
            Self::LucenaPosition => "lucena-position",
            Self::PhilidorPosition => "philidor-position",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MotifInstance {
    pub kind: MotifKind,
    /// Primary square involved, algebraic ("e5").
    pub square: String,
    /// The side the motif is bad news for ("white" / "black").
    pub against: String,
    /// Human-readable summary the LLM can quote directly.
    pub description: String,
    /// Other squares in the pattern (fork targets, the battery's second
    /// piece, the defender being removed) — the app highlights these
    /// alongside `square`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_squares: Vec<String>,
}

impl MotifInstance {
    fn new(
        kind: MotifKind,
        square: Square,
        against: Color,
        description: String,
    ) -> Self {
        Self {
            kind,
            square: square.to_string(),
            against: color_name(against).into(),
            description,
            extra_squares: Vec::new(),
        }
    }

    fn with_extra(mut self, squares: impl IntoIterator<Item = Square>) -> Self {
        self.extra_squares = squares.into_iter().map(|s| s.to_string()).collect();
        self
    }
}

/// Detect tactical and positional motifs in the current position, for both
/// colors, sorted most urgent first (see [`MotifKind::priority`]).
pub fn detect(pos: &Chess) -> Vec<MotifInstance> {
    let board = pos.board();
    let mut out = Vec::new();

    detect_double_check(pos, &mut out);
    for color in [Color::White, Color::Black] {
        detect_hanging(board, color, &mut out);
        detect_forks(board, color, &mut out);
        detect_pins_and_skewers(board, color, &mut out);
        detect_back_rank(board, color, &mut out);
        detect_discovered(board, color, &mut out);
        detect_trapped(board, color, &mut out);
        detect_removing_the_defender(board, color, &mut out);
        detect_overloaded(board, color, &mut out);
        detect_smothered(board, color, &mut out);
        detect_battery(board, color, &mut out);
        detect_passed_pawns(board, color, &mut out);
        detect_outposts(board, color, &mut out);
        detect_pawn_structure(board, color, &mut out);
        detect_bishop_pair(board, color, &mut out);
    }
    detect_opposition(pos, &mut out);
    detect_rook_endgames(board, &mut out);

    // Stable sort: within one priority class the per-detector order (and
    // so White before Black) is preserved, keeping output deterministic.
    out.sort_by_key(|m| m.kind.priority());
    out
}

/// The `max` most urgent motifs — what callers with a token or screen
/// budget should show. Positional findings only appear once the tactical
/// ones run out.
pub fn detect_top(pos: &Chess, max: usize) -> Vec<MotifInstance> {
    let mut found = detect(pos);
    found.truncate(max);
    found
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

fn pawns_of(board: &Board, color: Color) -> Bitboard {
    board.by_color(color) & board.by_role(Role::Pawn)
}

/// Material other than kings and pawns, in pawn units, both sides together.
/// The endgame detectors gate on this.
fn non_pawn_material(board: &Board) -> i32 {
    let mut total = 0;
    for role in [Role::Knight, Role::Bishop, Role::Rook, Role::Queen] {
        total += value(role) * board.by_role(role).count() as i32;
    }
    total
}

/// Rank index 1..=8 counted from `color`'s own back rank — so a white pawn
/// on a7 and a black pawn on a2 both read as 7 (one step from promoting).
fn relative_rank(sq: Square, color: Color) -> u32 {
    let rank = u32::from(sq.rank());
    match color {
        Color::White => rank + 1,
        Color::Black => 8 - rank,
    }
}

/// Every square strictly ahead of `sq` on its own file, from `color`'s view.
fn front_file(sq: Square, color: Color) -> Bitboard {
    let mut bb = Bitboard::EMPTY;
    let file = sq.file();
    for r in 0..8u32 {
        let rank = Rank::new(r);
        let ahead = match color {
            Color::White => r > u32::from(sq.rank()),
            Color::Black => r < u32::from(sq.rank()),
        };
        if ahead {
            bb |= Bitboard::from(Square::from_coords(file, rank));
        }
    }
    bb
}

/// The three-file corridor ahead of `sq` (its own file plus both
/// neighbours) — the squares an enemy pawn would need to occupy to stop or
/// challenge a passer.
fn front_span(sq: Square, color: Color) -> Bitboard {
    let mut bb = Bitboard::EMPTY;
    let file = u32::from(sq.file()) as i32;
    for df in -1..=1 {
        let f = file + df;
        if !(0..8).contains(&f) {
            continue;
        }
        let neighbour = Square::from_coords(File::new(f as u32), sq.rank());
        bb |= front_file(neighbour, color);
    }
    bb
}

/// Every square on the files either side of `sq`.
fn adjacent_files(sq: Square) -> Bitboard {
    let mut bb = Bitboard::EMPTY;
    let file = u32::from(sq.file()) as i32;
    for df in [-1, 1] {
        let f = file + df;
        if (0..8).contains(&f) {
            bb |= Bitboard::from_file(File::new(f as u32));
        }
    }
    bb
}

/// Cheapest enemy piece bearing on `sq`, in pawn units. `i32::MAX` when the
/// square is not attacked at all.
fn cheapest_attacker(board: &Board, sq: Square, by: Color, occupied: Bitboard) -> i32 {
    board
        .attacks_to(sq, by, occupied)
        .into_iter()
        .filter_map(|a| board.piece_at(a))
        .map(|p| value(p.role))
        .min()
        .unwrap_or(i32::MAX)
}

// MARK: - Tactical detectors

/// Two attackers on the king at once: no block, no capture, the king must
/// move. Worth its own callout because it breaks the usual "just take the
/// checker" reflex.
fn detect_double_check(pos: &Chess, out: &mut Vec<MotifInstance>) {
    let checkers = pos.checkers();
    if checkers.count() < 2 {
        return;
    }
    let victim = pos.turn();
    let Some(ksq) = pos.board().king_of(victim) else {
        return;
    };
    out.push(
        MotifInstance::new(
            MotifKind::DoubleCheck,
            ksq,
            victim,
            format!(
                "the {} king on {} is in double check — only a king move can answer it",
                color_name(victim),
                ksq
            ),
        )
        .with_extra(checkers),
    );
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
        let cheapest = cheapest_attacker(board, sq, victim.other(), occupied);

        if defenders.is_empty() {
            out.push(
                MotifInstance::new(
                    MotifKind::HangingPiece,
                    sq,
                    victim,
                    format!(
                        "the {} {} on {} is attacked and has no defender",
                        color_name(victim),
                        role_name(piece.role),
                        sq
                    ),
                )
                .with_extra(attackers),
            );
        } else if cheapest < value(piece.role) {
            out.push(
                MotifInstance::new(
                    MotifKind::HangingPiece,
                    sq,
                    victim,
                    format!(
                        "the {} {} on {} is attacked by a cheaper piece",
                        color_name(victim),
                        role_name(piece.role),
                        sq
                    ),
                )
                .with_extra(attackers),
            );
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
        let mut squares: Vec<Square> = Vec::new();
        for t in board.attacks_from(sq) & board.by_color(victim) {
            let tp = board.piece_at(t).expect("occupied square");
            let defended = !board.attacks_to(t, victim, occupied).is_empty();
            let valuable = tp.role == Role::King
                || value(tp.role) > attacker_value
                || (!defended && value(tp.role) >= attacker_value);
            if valuable {
                targets.push(format!("the {} on {}", role_name(tp.role), t));
                squares.push(t);
            }
        }
        if targets.len() >= 2 {
            out.push(
                MotifInstance::new(
                    MotifKind::Fork,
                    sq,
                    victim,
                    format!(
                        "the {} {} on {} forks {}",
                        color_name(attacker),
                        role_name(piece.role),
                        sq,
                        targets.join(" and ")
                    ),
                )
                .with_extra(squares),
            );
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
                    out.push(
                        MotifInstance::new(
                            MotifKind::Pin,
                            front_sq,
                            victim,
                            format!(
                                "the {} {} on {} is pinned to its king by the {} on {}",
                                color_name(victim),
                                role_name(front.role),
                                front_sq,
                                role_name(slider.role),
                                ssq
                            ),
                        )
                        .with_extra([ssq, behind_sq]),
                    );
                } else if value(front.role) > value(behind.role)
                    && value(front.role) > value(slider.role)
                {
                    out.push(
                        MotifInstance::new(
                            MotifKind::Skewer,
                            front_sq,
                            victim,
                            format!(
                                "the {} {} on {} is skewered against the {} on {} by the {} on {}",
                                color_name(victim),
                                role_name(front.role),
                                front_sq,
                                role_name(behind.role),
                                behind_sq,
                                role_name(slider.role),
                                ssq
                            ),
                        )
                        .with_extra([ssq, behind_sq]),
                    );
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
        Color::White => (Rank::First, Rank::Second),
        Color::Black => (Rank::Eighth, Rank::Seventh),
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
        let sq = Square::from_coords(File::new(f as u32), front);
        let blocked_by_own = board.by_color(victim).contains(sq);
        if !blocked_by_own {
            escape_exists = true;
            break;
        }
    }
    if !escape_exists {
        out.push(
            MotifInstance::new(
                MotifKind::BackRankWeakness,
                ksq,
                victim,
                format!(
                    "the {} king on {} has no escape square off the back rank, and the opponent \
                     has heavy pieces that could reach it",
                    color_name(victim),
                    ksq
                ),
            )
            .with_extra(enemy_heavy),
        );
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
                    out.push(
                        MotifInstance::new(
                            MotifKind::DiscoveredAttackAvailable,
                            msq,
                            victim,
                            format!(
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
                        )
                        .with_extra([ssq, t]),
                    );
                }
            }
        }
    }
}

/// An attacked piece with nowhere safe to go. Static approximation: the
/// piece is attacked by something cheaper (or is undefended), and every
/// square it could step to is covered by a cheaper enemy piece. Pawns and
/// kings are excluded — "trapped" is a piece-loss idea.
fn detect_trapped(board: &Board, victim: Color, out: &mut Vec<MotifInstance>) {
    let occupied = board.occupied();
    let enemy = victim.other();
    for sq in board.by_color(victim) & !board.by_role(Role::King) & !board.by_role(Role::Pawn) {
        let piece = board.piece_at(sq).expect("occupied square");
        let attackers = board.attacks_to(sq, enemy, occupied);
        if attackers.is_empty() {
            continue;
        }
        // Only a piece that actually stands to be lost is "trapped": either
        // nothing defends it, or the cheapest attacker wins material.
        let defended = !board.attacks_to(sq, victim, occupied).is_empty();
        let cheapest_here = cheapest_attacker(board, sq, enemy, occupied);
        if defended && cheapest_here >= value(piece.role) {
            continue;
        }

        // Squares it can step to, judged with the piece already gone from
        // its origin — a fleeing piece no longer blocks the ray it left.
        let without = occupied & !Bitboard::from(sq);
        let escapes = board.attacks_from(sq) & !board.by_color(victim);
        let has_escape = escapes.into_iter().any(|dest| {
            let after = without | Bitboard::from(dest);
            let cheapest_there = cheapest_attacker(board, dest, enemy, after);
            if cheapest_there == i32::MAX {
                return true; // Nothing covers the square — a clean escape.
            }
            // Landing on an attacked square still saves the piece if the
            // move grabs at least as much as it risks.
            let gain = board.piece_at(dest).map(|p| value(p.role)).unwrap_or(0);
            if gain >= value(piece.role) {
                return true;
            }
            // Otherwise the square must be defended by somebody OTHER than
            // the fleeing piece itself, and only by pieces expensive enough
            // that the trade is not a loss. An undefended square covered by
            // anything at all loses the piece just the same.
            let defended_there =
                !(board.attacks_to(dest, victim, after) & !Bitboard::from(sq)).is_empty();
            defended_there && cheapest_there >= value(piece.role)
        });
        if has_escape {
            continue;
        }
        out.push(
            MotifInstance::new(
                MotifKind::TrappedPiece,
                sq,
                victim,
                format!(
                    "the {} {} on {} is attacked and has no safe square to run to",
                    color_name(victim),
                    role_name(piece.role),
                    sq
                ),
            )
            .with_extra(attackers),
        );
    }
}

/// The sole defender of an attacked piece is itself capturable — take the
/// defender first and the piece behind it falls.
fn detect_removing_the_defender(board: &Board, attacker: Color, out: &mut Vec<MotifInstance>) {
    let victim = attacker.other();
    let occupied = board.occupied();
    for target in board.by_color(victim) & !board.by_role(Role::King) {
        let target_piece = board.piece_at(target).expect("occupied square");
        // Only interesting when we are already hitting the target and the
        // exchange is currently held by exactly one defender.
        if board.attacks_to(target, attacker, occupied).is_empty() {
            continue;
        }
        let defenders = board.attacks_to(target, victim, occupied);
        let Some(defender) = defenders.single_square() else {
            continue;
        };
        let defender_piece = board.piece_at(defender).expect("occupied square");
        if defender_piece.role == Role::King {
            continue; // A king defender cannot be captured.
        }
        // Is the lone defender itself takeable on favourable terms?
        let defender_attackers = board.attacks_to(defender, attacker, occupied);
        if defender_attackers.is_empty() {
            continue;
        }
        let defended = !board
            .attacks_to(defender, victim, occupied & !Bitboard::from(defender))
            .is_empty();
        let cheapest = cheapest_attacker(board, defender, attacker, occupied);
        if defended && cheapest > value(defender_piece.role) {
            continue;
        }
        out.push(
            MotifInstance::new(
                MotifKind::RemovingTheDefender,
                defender,
                victim,
                format!(
                    "the {} {} on {} is the only thing defending the {} on {}, and it can be \
                     captured or driven off first",
                    color_name(victim),
                    role_name(defender_piece.role),
                    defender,
                    role_name(target_piece.role),
                    target
                ),
            )
            .with_extra([target]),
        );
    }
}

/// One piece asked to hold two attacked units at once — whichever it saves,
/// the other drops.
fn detect_overloaded(board: &Board, victim: Color, out: &mut Vec<MotifInstance>) {
    let occupied = board.occupied();
    let enemy = victim.other();
    for defender in board.by_color(victim) {
        let defender_piece = board.piece_at(defender).expect("occupied square");
        let mut duties: Vec<Square> = Vec::new();
        for guarded in board.attacks_from(defender) & board.by_color(victim) {
            if board.attacks_to(guarded, enemy, occupied).is_empty() {
                continue; // Nothing is attacking it; no duty to discharge.
            }
            let held_alone = board
                .attacks_to(guarded, victim, occupied)
                .single_square()
                .is_some_and(|only| only == defender);
            if held_alone {
                duties.push(guarded);
            }
        }
        if duties.len() >= 2 {
            let listed = duties
                .iter()
                .map(|s| {
                    let p = board.piece_at(*s).expect("occupied square");
                    format!("the {} on {}", role_name(p.role), s)
                })
                .collect::<Vec<_>>()
                .join(" and ");
            out.push(
                MotifInstance::new(
                    MotifKind::OverloadedDefender,
                    defender,
                    victim,
                    format!(
                        "the {} {} on {} is overloaded — it is the only defender of both {}",
                        color_name(victim),
                        role_name(defender_piece.role),
                        defender,
                        listed
                    ),
                )
                .with_extra(duties),
            );
        }
    }
}

/// The king's own pieces have sealed every flight square while an enemy
/// knight is on the board — the smothered-mate shape.
fn detect_smothered(board: &Board, victim: Color, out: &mut Vec<MotifInstance>) {
    let Some(ksq) = board.king_of(victim) else {
        return;
    };
    let enemy_knights = board.by_color(victim.other()) & board.by_role(Role::Knight);
    if enemy_knights.is_empty() {
        return;
    }
    let flight = attacks::king_attacks(ksq) & !board.by_color(victim);
    if !flight.is_empty() {
        return;
    }
    // The pattern only bites if a knight can actually reach a checking
    // square next to that king.
    let checking_squares = attacks::knight_attacks(ksq);
    let knight_can_come = enemy_knights
        .into_iter()
        .any(|nsq| !(attacks::knight_attacks(nsq) & checking_squares).is_empty());
    if !knight_can_come {
        return;
    }
    out.push(MotifInstance::new(
        MotifKind::SmotheredKing,
        ksq,
        victim,
        format!(
            "the {} king on {} is boxed in by its own pieces with an enemy knight nearby — the \
             smothered-mate pattern",
            color_name(victim),
            ksq
        ),
    ));
}

/// Two friendly sliders stacked on one clear line: queen behind rook on a
/// file, rooks doubled, queen and bishop on a diagonal.
fn detect_battery(board: &Board, color: Color, out: &mut Vec<MotifInstance>) {
    let occupied = board.occupied();
    let own_sliders = board.by_color(color) & sliders(board);
    for first in own_sliders {
        let piece = board.piece_at(first).expect("occupied square");
        for second in attacks::attacks(first, piece, occupied) & own_sliders {
            // Each pair once: only report from the lower square.
            if second <= first {
                continue;
            }
            let other = board.piece_at(second).expect("occupied square");
            let straight = first.file() == second.file() || first.rank() == second.rank();
            let compatible = if straight {
                matches!(piece.role, Role::Rook | Role::Queen)
                    && matches!(other.role, Role::Rook | Role::Queen)
            } else {
                matches!(piece.role, Role::Bishop | Role::Queen)
                    && matches!(other.role, Role::Bishop | Role::Queen)
            };
            if !compatible {
                continue;
            }
            out.push(
                MotifInstance::new(
                    MotifKind::Battery,
                    first,
                    color.other(),
                    format!(
                        "the {} {} on {} and {} on {} form a battery on the same line",
                        color_name(color),
                        role_name(piece.role),
                        first,
                        role_name(other.role),
                        second
                    ),
                )
                .with_extra([second]),
            );
        }
    }
}

// MARK: - Positional detectors

/// Pawns no enemy pawn can stop on their file or either neighbour. A passer
/// with every enemy pawn at least two files away is flagged as an outside
/// passer — the endgame's most valuable structural asset.
fn detect_passed_pawns(board: &Board, color: Color, out: &mut Vec<MotifInstance>) {
    let enemy_pawns = pawns_of(board, color.other());
    let own_pawns = pawns_of(board, color);
    for sq in own_pawns {
        if !(front_span(sq, color) & enemy_pawns).is_empty() {
            continue;
        }
        // A pawn behind a friendly pawn on the same file is not the passer;
        // the front one is.
        if !(front_file(sq, color) & own_pawns).is_empty() {
            continue;
        }
        let file = u32::from(sq.file()) as i32;
        let outside = enemy_pawns
            .into_iter()
            .all(|e| (u32::from(e.file()) as i32 - file).abs() >= 2)
            && !enemy_pawns.is_empty();
        let protected = !(attacks::pawn_attacks(color.other(), sq) & own_pawns).is_empty();
        let kind = if outside {
            MotifKind::OutsidePassedPawn
        } else {
            MotifKind::PassedPawn
        };
        let shade = if outside {
            " far from the enemy pawns"
        } else if protected {
            ", and it is protected by a friendly pawn"
        } else {
            ""
        };
        out.push(MotifInstance::new(
            kind,
            sq,
            color.other(),
            format!(
                "the {} pawn on {} is passed{shade} — {} steps from promoting",
                color_name(color),
                sq,
                8 - relative_rank(sq, color)
            ),
        ));
    }
}

/// A minor piece parked on a square no enemy pawn can ever challenge,
/// protected by a pawn of its own, inside the enemy half.
fn detect_outposts(board: &Board, color: Color, out: &mut Vec<MotifInstance>) {
    let own_pawns = pawns_of(board, color);
    let enemy_pawns = pawns_of(board, color.other());
    let minors = board.by_color(color) & (board.by_role(Role::Knight) | board.by_role(Role::Bishop));
    for sq in minors {
        let rank = relative_rank(sq, color);
        if !(4..=6).contains(&rank) {
            continue;
        }
        // Protected by one of our pawns: our pawn sits on a square that
        // attacks this one, i.e. this square is hit by our pawn attacks.
        if (attacks::pawn_attacks(color.other(), sq) & own_pawns).is_empty() {
            continue;
        }
        // No enemy pawn is left on a neighbouring file behind the outpost,
        // so none can ever come forward to kick it.
        let can_be_kicked = enemy_pawns.into_iter().any(|e| {
            let same_side = (u32::from(e.file()) as i32 - u32::from(sq.file()) as i32).abs() == 1;
            same_side && relative_rank(e, color.other()) < relative_rank(sq, color.other())
        });
        if can_be_kicked {
            continue;
        }
        let piece = board.piece_at(sq).expect("occupied square");
        out.push(MotifInstance::new(
            MotifKind::Outpost,
            sq,
            color.other(),
            format!(
                "the {} {} on {} sits on an outpost — pawn-protected, and no enemy pawn can drive \
                 it away",
                color_name(color),
                role_name(piece.role),
                sq
            ),
        ));
    }
}

/// Isolated, doubled, and backward pawns — the three structural weaknesses
/// a student can act on.
fn detect_pawn_structure(board: &Board, color: Color, out: &mut Vec<MotifInstance>) {
    let own_pawns = pawns_of(board, color);
    let enemy_pawns = pawns_of(board, color.other());

    // Doubled: report the file once, from its rearmost pawn.
    for file_index in 0..8u32 {
        let file = File::new(file_index);
        let on_file = own_pawns & Bitboard::from_file(file);
        if on_file.count() < 2 {
            continue;
        }
        let rear = on_file
            .into_iter()
            .min_by_key(|sq| relative_rank(*sq, color))
            .expect("non-empty");
        out.push(
            MotifInstance::new(
                MotifKind::DoubledPawns,
                rear,
                color,
                format!(
                    "{} has doubled pawns on the {}-file — they cannot defend each other",
                    color_name(color),
                    (b'a' + file_index as u8) as char
                ),
            )
            .with_extra(on_file.into_iter().filter(|s| *s != rear)),
        );
    }

    for sq in own_pawns {
        let neighbours = own_pawns & adjacent_files(sq);
        if neighbours.is_empty() {
            out.push(MotifInstance::new(
                MotifKind::IsolatedPawn,
                sq,
                color,
                format!(
                    "the {} pawn on {} is isolated — no friendly pawn can ever defend it",
                    color_name(color),
                    sq
                ),
            ));
            continue;
        }
        // Backward: every neighbour is further advanced, and the square in
        // front is covered by an enemy pawn, so it cannot catch up.
        let rank = relative_rank(sq, color);
        let all_ahead = neighbours
            .into_iter()
            .all(|n| relative_rank(n, color) > rank);
        if !all_ahead {
            continue;
        }
        let ahead_rank = match color {
            Color::White => u32::from(sq.rank()) + 1,
            Color::Black => u32::from(sq.rank()).wrapping_sub(1),
        };
        if ahead_rank > 7 {
            continue;
        }
        let ahead = Square::from_coords(sq.file(), Rank::new(ahead_rank));
        let covered_by_enemy_pawn =
            !(attacks::pawn_attacks(color, ahead) & enemy_pawns).is_empty();
        if covered_by_enemy_pawn {
            out.push(MotifInstance::new(
                MotifKind::BackwardPawn,
                sq,
                color,
                format!(
                    "the {} pawn on {} is backward — its neighbours have advanced past it and an \
                     enemy pawn covers the square in front",
                    color_name(color),
                    sq
                ),
            ));
        }
    }
}

/// Two bishops against one (or none) — the long-term middlegame edge.
fn detect_bishop_pair(board: &Board, color: Color, out: &mut Vec<MotifInstance>) {
    let own = board.by_color(color) & board.by_role(Role::Bishop);
    let theirs = board.by_color(color.other()) & board.by_role(Role::Bishop);
    if own.count() < 2 || theirs.count() >= 2 {
        return;
    }
    // Only a real pair if they cover both square colors.
    let light = Bitboard::LIGHT_SQUARES;
    if (own & light).is_empty() || (own & !light).is_empty() {
        return;
    }
    let anchor = own.into_iter().next().expect("non-empty");
    out.push(
        MotifInstance::new(
            MotifKind::BishopPair,
            anchor,
            color.other(),
            format!(
                "{} holds the bishop pair — the two bishops cover both square colors as the \
                 position opens up",
                color_name(color)
            ),
        )
        .with_extra(own.into_iter().skip(1)),
    );
}

/// Kings a square apart on one line in a pawn endgame: the side NOT to move
/// holds the opposition.
fn detect_opposition(pos: &Chess, out: &mut Vec<MotifInstance>) {
    let board = pos.board();
    if non_pawn_material(board) > 0 {
        return; // Pure king-and-pawn endings only.
    }
    let (Some(white), Some(black)) = (board.king_of(Color::White), board.king_of(Color::Black))
    else {
        return;
    };
    let file_gap = (u32::from(white.file()) as i32 - u32::from(black.file()) as i32).abs();
    let rank_gap = (u32::from(white.rank()) as i32 - u32::from(black.rank()) as i32).abs();
    let direct = (file_gap == 0 && rank_gap == 2)
        || (rank_gap == 0 && file_gap == 2)
        || (file_gap == 2 && rank_gap == 2);
    if !direct {
        return;
    }
    // Whoever must move first has to give ground.
    let holder = pos.turn().other();
    let (holder_sq, loser_sq) = if holder == Color::White {
        (white, black)
    } else {
        (black, white)
    };
    out.push(
        MotifInstance::new(
            MotifKind::Opposition,
            holder_sq,
            holder.other(),
            format!(
                "the kings stand in opposition and {} has it — {} must move first and give ground",
                color_name(holder),
                color_name(holder.other())
            ),
        )
        .with_extra([loser_sq]),
    );
}

/// The two rook endgames every student needs: Lucena (win) and Philidor
/// (draw). Both are narrow shape checks over a rook-and-pawn-versus-rook
/// material split.
fn detect_rook_endgames(board: &Board, out: &mut Vec<MotifInstance>) {
    for attacker in [Color::White, Color::Black] {
        let defender = attacker.other();
        let a_pawns = pawns_of(board, attacker);
        let a_rooks = board.by_color(attacker) & board.by_role(Role::Rook);
        let d_rooks = board.by_color(defender) & board.by_role(Role::Rook);
        // Exactly R+P vs R, nothing else on the board but kings.
        let a_others = board.by_color(attacker)
            & !board.by_role(Role::King)
            & !board.by_role(Role::Rook)
            & !board.by_role(Role::Pawn);
        let d_others = board.by_color(defender) & !board.by_role(Role::King) & !d_rooks;
        if a_pawns.count() != 1
            || a_rooks.count() != 1
            || d_rooks.count() != 1
            || !a_others.is_empty()
            || !d_others.is_empty()
        {
            continue;
        }
        let (Some(pawn), Some(d_rook)) = (a_pawns.single_square(), d_rooks.single_square()) else {
            continue;
        };
        let (Some(a_king), Some(d_king)) = (board.king_of(attacker), board.king_of(defender))
        else {
            continue;
        };
        let pawn_rank = relative_rank(pawn, attacker);
        let promo_rank = match attacker {
            Color::White => Rank::Eighth,
            Color::Black => Rank::First,
        };
        let promo_square = Square::from_coords(pawn.file(), promo_rank);

        // Lucena: pawn one step from promoting, our king already on the
        // queening square, their king cut off by at least two files.
        let king_cut_off =
            (u32::from(d_king.file()) as i32 - u32::from(pawn.file()) as i32).abs() >= 2;
        if pawn_rank == 7 && a_king == promo_square && king_cut_off {
            out.push(
                MotifInstance::new(
                    MotifKind::LucenaPosition,
                    pawn,
                    defender,
                    format!(
                        "a Lucena-type position for {}: the pawn is on the seventh with the king \
                         on the queening square — build a bridge with the rook and promote",
                        color_name(attacker)
                    ),
                )
                .with_extra([a_king, d_king]),
            );
            continue;
        }

        // Philidor: pawn not yet past the sixth, their king in front of it,
        // their rook parked on their own third rank.
        let d_third = match defender {
            Color::White => Rank::Third,
            Color::Black => Rank::Sixth,
        };
        let king_blockading =
            (u32::from(d_king.file()) as i32 - u32::from(pawn.file()) as i32).abs() <= 1
                && relative_rank(d_king, attacker) > pawn_rank;
        if pawn_rank <= 5 && king_blockading && d_rook.rank() == d_third {
            out.push(
                MotifInstance::new(
                    MotifKind::PhilidorPosition,
                    d_rook,
                    attacker,
                    format!(
                        "a Philidor-type position for {}: the king blockades the pawn and the rook \
                         holds the third rank — the drawing setup",
                        color_name(defender)
                    ),
                )
                .with_extra([d_king, pawn]),
            );
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

    fn has(found: &[MotifInstance], kind: MotifKind, square: &str) -> bool {
        found.iter().any(|m| m.kind == kind && m.square == square)
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

    #[test]
    fn detects_double_check() {
        // Black king e8; white knight d6 checks, and the rook on e1 checks
        // down the open e-file behind it.
        let found = motifs_for("4k3/8/3N4/8/8/8/8/4R1K1 b - - 0 1");
        assert!(has(&found, MotifKind::DoubleCheck, "e8"));
    }

    #[test]
    fn double_check_leads_the_sort() {
        let found = motifs_for("4k3/8/3N4/8/8/8/8/4R1K1 b - - 0 1");
        assert_eq!(found[0].kind, MotifKind::DoubleCheck);
    }

    #[test]
    fn detects_trapped_piece() {
        // The white bishop on a7 is attacked down the file by the a1 rook.
        // Its only two squares are b6 (a pawn, but the c7 pawn recaptures)
        // and b8 (covered by the d8 rook, undefended) — nowhere to run.
        let found = motifs_for("3r4/B1p5/1p5k/8/8/7K/8/r7 w - - 0 1");
        assert!(has(&found, MotifKind::TrappedPiece, "a7"));
    }

    #[test]
    fn escaping_to_a_defended_square_is_not_trapped() {
        // Same shape, but a white knight on d7 guards b8: the bishop can
        // step there and only the rook (worth more) can take it.
        let found = motifs_for("3r4/B1pN4/1p5k/8/8/7K/8/r7 w - - 0 1");
        assert!(!has(&found, MotifKind::TrappedPiece, "a7"));
    }

    #[test]
    fn detects_removing_the_defender() {
        // The e1 rook hits the black rook on e6, whose only defender is the
        // c5 knight — and the b4 pawn can take that knight first.
        let found = motifs_for("4k3/8/4r3/2n5/1P6/8/8/4R1K1 b - - 0 1");
        assert!(has(&found, MotifKind::RemovingTheDefender, "c5"));
    }

    #[test]
    fn defender_that_cannot_be_taken_profitably_is_not_flagged() {
        // Same idea, but the lone defender is a pawn guarded by its own
        // bishop: Bxc6 Bxc6 loses material, so it is no tactic.
        let found = motifs_for("4k3/8/2p5/3b4/B7/8/8/3R2K1 b - - 0 1");
        assert!(!found
            .iter()
            .any(|m| m.kind == MotifKind::RemovingTheDefender));
    }

    #[test]
    fn detects_overloaded_defender() {
        // The black rook on d7 is the only defender of both the b7 and f7
        // pawns, each attacked by a white rook.
        let found = motifs_for("7k/1p1r1p2/8/8/8/8/8/1R3RK1 b - - 0 1");
        assert!(has(&found, MotifKind::OverloadedDefender, "d7"));
    }

    #[test]
    fn detects_passed_pawn() {
        // The white a-pawn has no black pawn on a, b, or c ahead of it.
        let found = motifs_for("4k3/5p2/8/P7/8/8/5P2/4K3 w - - 0 1");
        assert!(found.iter().any(|m| {
            matches!(m.kind, MotifKind::PassedPawn | MotifKind::OutsidePassedPawn)
                && m.square == "a5"
        }));
    }

    #[test]
    fn far_flung_passer_is_an_outside_passer() {
        let found = motifs_for("4k3/5p2/8/P7/8/8/5P2/4K3 w - - 0 1");
        assert!(has(&found, MotifKind::OutsidePassedPawn, "a5"));
    }

    #[test]
    fn detects_isolated_and_doubled_pawns() {
        // White has doubled, isolated pawns on the c-file.
        let found = motifs_for("4k3/8/8/8/2P5/2P5/8/4K3 w - - 0 1");
        assert!(found.iter().any(|m| m.kind == MotifKind::DoubledPawns));
        assert!(found.iter().any(|m| m.kind == MotifKind::IsolatedPawn));
    }

    #[test]
    fn detects_backward_pawn() {
        // The white d3 pawn trails its c4/e4 neighbours, and black's c5/e5
        // pawns cover d4 — it can never catch up.
        let found = motifs_for("4k3/8/8/2p1p3/2P1P3/3P4/8/4K3 w - - 0 1");
        assert!(has(&found, MotifKind::BackwardPawn, "d3"));
    }

    #[test]
    fn detects_bishop_pair() {
        let found = motifs_for("4k1n1/8/8/8/8/8/2B2B2/4K3 w - - 0 1");
        assert!(found.iter().any(|m| m.kind == MotifKind::BishopPair));
    }

    #[test]
    fn detects_opposition() {
        // Kings on e4 and e6 with white to move: black holds the
        // opposition.
        let found = motifs_for("8/8/4k3/8/4K3/8/4P3/8 w - - 0 1");
        let op = found
            .iter()
            .find(|m| m.kind == MotifKind::Opposition)
            .expect("opposition detected");
        assert_eq!(op.against, "white");
    }

    #[test]
    fn opposition_only_in_pawn_endings() {
        // Same king shape, but a rook is still on: not an opposition call.
        let found = motifs_for("8/8/4k3/8/4K3/8/4P3/R7 w - - 0 1");
        assert!(!found.iter().any(|m| m.kind == MotifKind::Opposition));
    }

    #[test]
    fn detects_lucena_position() {
        // White pawn c7, king on the queening square c8, black king cut off
        // on the e-file by the rook.
        let found = motifs_for("2K5/2P5/8/8/8/8/4k3/2R3r1 w - - 0 1");
        assert!(found.iter().any(|m| m.kind == MotifKind::LucenaPosition));
    }

    #[test]
    fn detects_philidor_position() {
        // Black king blockades the e5 pawn from e6 with the rook on the
        // third rank (a6 from black's side).
        let found = motifs_for("8/8/r3k3/4P3/4K3/8/8/4R3 w - - 0 1");
        assert!(found.iter().any(|m| m.kind == MotifKind::PhilidorPosition));
    }

    #[test]
    fn detects_outpost() {
        // White knight on d5, protected by the c4 pawn, with no black pawn
        // left on c or e to challenge it.
        let found = motifs_for("4k3/pp4pp/8/3N4/2P5/8/PP4PP/4K3 w - - 0 1");
        assert!(has(&found, MotifKind::Outpost, "d5"));
    }

    #[test]
    fn detects_battery() {
        // White queen d1 behind the rook d4 on the open d-file.
        let found = motifs_for("4k3/8/8/8/3R4/8/8/3QK3 w - - 0 1");
        assert!(found.iter().any(|m| m.kind == MotifKind::Battery));
    }

    #[test]
    fn detect_top_prefers_tactics_over_structure() {
        // A fork alongside a pile of structural noise: the fork comes
        // first, so a caller taking only one finding gets the tactic.
        const FEN: &str = "k3r3/2N5/8/8/2P5/2P5/8/6K1 b - - 0 1";
        let game = GameState::from_fen(FEN).unwrap();
        let all = detect(game.position());
        assert!(all.len() > 1, "fixture should also produce structural motifs");
        let top = detect_top(game.position(), 1);
        assert_eq!(top.len(), 1);
        assert!(top[0].kind.is_tactical());
    }
}
