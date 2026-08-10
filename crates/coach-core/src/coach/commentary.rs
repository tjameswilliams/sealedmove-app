//! The commentary engine: a deterministic policy decides WHEN the coach
//! speaks and WHAT to focus on; the LLM decides the words, grounded in a
//! deterministic situation report built from the board and engine facts.
//!
//! Nothing in this module calls the LLM or the engine — it is pure logic
//! over values the session already computed, so every decision is unit
//! testable without I/O. Variety in canned lines rotates by move number,
//! never randomness: the same game replayed produces the same commentary.

use crate::coach::MoveVerdict;
use crate::engine::{Analysis, Judgment};
use crate::game::{motifs, openings::Opening, GameState};
use serde::{Deserialize, Serialize};
use shakmaty::{Chess, Color, Position, Role, Square};

/// How much the coach talks. Serialized in app settings, so the wire names
/// are stable.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommentaryStyle {
    /// Today's classic behavior: the LLM only speaks on notable moves
    /// (inaccuracy/mistake/blunder); everything else gets a canned line; no
    /// opponent or game-arc commentary at all.
    Quiet,
    /// Full LLM reaction on notable moves and milestones; brief canned
    /// encouragement on ordinary good moves with a full "why that was good"
    /// reaction every 3rd good move; opponent commentary only on a real
    /// threat or a big eval swing; a development summary every ~10 full
    /// moves and on phase transitions.
    #[default]
    Balanced,
    /// Full LLM reaction to every student move; opponent commentary whenever
    /// there is anything worth flagging (threat, motif against the student,
    /// phase change); a development summary every ~6 full moves.
    Chatty,
}

/// Game moments worth celebrating or marking, detected by the session from
/// SAN/board state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MilestoneKind {
    Castled,
    FirstCapture,
    Promotion,
    Check,
    /// The last queen(s) just left the board.
    QueensOff,
}

/// What one coach message should be about. A `Full` decision carries one or
/// more of these; each maps to an explicit instruction for the LLM (see
/// [`focus_instruction`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Focus {
    Encourage,
    ExplainWhyGood,
    ExplainMistake,
    ThreatWarning,
    OpeningNote,
    PhaseTransition,
    DevelopmentSummary,
    Milestone(MilestoneKind),
}

/// The policy's answer: say nothing, use a canned line, or invoke the LLM
/// with the given focuses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Silent,
    Brief,
    Full(Vec<Focus>),
}

/// Game phase, from [`detect_phase`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Opening,
    Middlegame,
    Endgame,
}

impl Phase {
    pub fn label(&self) -> &'static str {
        match self {
            Phase::Opening => "opening",
            Phase::Middlegame => "middlegame",
            Phase::Endgame => "endgame",
        }
    }
}

/// Which phase the game is in. Documented heuristic:
///
/// - **Endgame**: total non-pawn, non-king material (both sides, in pawn
///   units: N/B=3, R=5, Q=9) is 12 or less — or the queens are off *and*
///   that material is 18 or less. A bare early queen trade with every other
///   piece still on the board is NOT an endgame.
/// - **Opening**: not an endgame, fullmove number ≤ 10, and at least two
///   minor pieces still sit on their home squares (b1/g1/c1/f1 and the
///   black mirrors) — i.e. somebody is still developing.
/// - **Middlegame**: everything else.
pub fn detect_phase(pos: &Chess) -> Phase {
    let board = pos.board();
    let mut material = 0i32;
    for (role, v) in [
        (Role::Knight, 3),
        (Role::Bishop, 3),
        (Role::Rook, 5),
        (Role::Queen, 9),
    ] {
        material += board.by_role(role).count() as i32 * v;
    }
    let queens_off = board.by_role(Role::Queen).is_empty();
    if material <= 12 || (queens_off && material <= 18) {
        return Phase::Endgame;
    }
    let undeveloped = undeveloped_minors(pos);
    if pos.fullmoves().get() <= 10 && undeveloped >= 2 {
        return Phase::Opening;
    }
    Phase::Middlegame
}

/// Minor pieces (of the right color) still on their starting squares.
fn undeveloped_minors(pos: &Chess) -> u32 {
    let board = pos.board();
    let homes: [(&str, Role, Color); 8] = [
        ("b1", Role::Knight, Color::White),
        ("g1", Role::Knight, Color::White),
        ("c1", Role::Bishop, Color::White),
        ("f1", Role::Bishop, Color::White),
        ("b8", Role::Knight, Color::Black),
        ("g8", Role::Knight, Color::Black),
        ("c8", Role::Bishop, Color::Black),
        ("f8", Role::Bishop, Color::Black),
    ];
    let mut n = 0;
    for (sq, role, color) in homes {
        let sq: Square = sq.parse().expect("static square");
        if board.piece_at(sq) == Some(shakmaty::Piece { color, role }) {
            n += 1;
        }
    }
    n
}

/// Everything the policy needs to know about the opponent's just-played
/// move. Built by the session from ONE engine analysis of the position
/// after the opponent moved (student to move, so the top line's score is
/// already from the student's perspective).
#[derive(Debug, Clone)]
pub struct OpponentContext {
    /// The opponent's move, SAN.
    pub opponent_san: String,
    /// Fullmove number after the opponent's move.
    pub move_number: u32,
    /// Eval change from the student's perspective since their last judged
    /// move (negative = things got worse for the student).
    pub eval_swing_cp: i32,
    /// The engine sees a forced mate against the student.
    pub threatens_mate: bool,
    /// The eval swung against the student and the top line's early moves
    /// include captures — the opponent's idea wins material.
    pub wins_material: bool,
    /// Motif descriptions currently on the board against the student
    /// (from [`motifs::detect`]).
    pub motifs_against_student: Vec<String>,
    /// Phase after the opponent's move.
    pub phase: Phase,
}

/// Deterministic commentary cadence: WHEN to speak, WHAT to focus on. Pure
/// state machine — no I/O, no randomness. The session owns one per game.
#[derive(Debug, Clone)]
pub struct CommentaryPolicy {
    style: CommentaryStyle,
    /// Consecutive ordinary good-class student moves that only got a brief
    /// line; every 3rd earns a full "why that was good".
    good_streak: u32,
    /// Full moves since the last development summary.
    moves_since_summary: u32,
    /// The opening gets announced at most once per game.
    opening_announced: bool,
    /// Last phase the policy saw; a change triggers PhaseTransition.
    phase: Phase,
}

impl CommentaryPolicy {
    pub fn new(style: CommentaryStyle) -> Self {
        Self {
            style,
            good_streak: 0,
            moves_since_summary: 0,
            opening_announced: false,
            phase: Phase::Opening,
        }
    }

    pub fn style(&self) -> CommentaryStyle {
        self.style
    }

    /// Change the style mid-game; counters are kept so cadence stays smooth.
    pub fn set_style(&mut self, style: CommentaryStyle) {
        self.style = style;
    }

    /// Decide how to react to the student's just-judged move.
    ///
    /// `phase_now` is the phase after the move; `opening_identified` is
    /// whether the move history still matches a named opening.
    pub fn on_student_move(
        &mut self,
        verdict: &MoveVerdict,
        milestones: &[MilestoneKind],
        move_number: u32,
        phase_now: Phase,
        opening_identified: bool,
    ) -> Decision {
        let _ = move_number; // rotation happens at the canned-line call site
        let phase_changed = phase_now != self.phase;
        self.phase = phase_now;
        let notable = verdict.judgment.is_notable();

        if self.style == CommentaryStyle::Quiet {
            // Exactly today's behavior: LLM on notable moves, canned line
            // otherwise; milestones, openings, and phases go unremarked.
            return if notable {
                Decision::Full(vec![Focus::ExplainMistake])
            } else {
                Decision::Brief
            };
        }

        let mut focuses: Vec<Focus> = Vec::new();
        if notable {
            self.good_streak = 0;
            focuses.push(Focus::ExplainMistake);
            if verdict.allows_mate_in.is_some() {
                focuses.push(Focus::ThreatWarning);
            }
        } else {
            match self.style {
                CommentaryStyle::Chatty => {
                    // Every move gets the full treatment.
                    self.good_streak = 0;
                    focuses.push(Focus::Encourage);
                    focuses.push(Focus::ExplainWhyGood);
                }
                CommentaryStyle::Balanced => {
                    self.good_streak += 1;
                    if self.good_streak >= 3 {
                        self.good_streak = 0;
                        focuses.push(Focus::ExplainWhyGood);
                    }
                }
                CommentaryStyle::Quiet => unreachable!("handled above"),
            }
        }

        for m in milestones {
            focuses.push(Focus::Milestone(*m));
        }
        if phase_changed {
            focuses.push(Focus::PhaseTransition);
        }
        if opening_identified && !self.opening_announced {
            self.opening_announced = true;
            focuses.push(Focus::OpeningNote);
        }

        if focuses.is_empty() {
            Decision::Brief
        } else {
            // Any full reaction counts as attention paid: restart the
            // "every 3rd good move" cadence so the coach never speaks twice
            // in a row for streak reasons right after a milestone/phase/
            // opening message.
            self.good_streak = 0;
            Decision::Full(focuses)
        }
    }

    /// Decide how to react to the opponent's just-played move.
    pub fn on_opponent_move(&mut self, ctx: &OpponentContext) -> Decision {
        if self.style == CommentaryStyle::Quiet {
            return Decision::Silent;
        }
        let phase_changed = ctx.phase != self.phase;
        self.phase = ctx.phase;
        self.moves_since_summary += 1;

        let (swing_threshold, summary_every, flag_motifs) = match self.style {
            CommentaryStyle::Balanced => (150, 10, false),
            CommentaryStyle::Chatty => (90, 6, true),
            CommentaryStyle::Quiet => unreachable!("handled above"),
        };

        let mut focuses: Vec<Focus> = Vec::new();
        let threatening = ctx.threatens_mate
            || ctx.wins_material
            || ctx.eval_swing_cp.abs() >= swing_threshold
            || (flag_motifs && !ctx.motifs_against_student.is_empty());
        if threatening {
            focuses.push(Focus::ThreatWarning);
        }
        if phase_changed {
            focuses.push(Focus::PhaseTransition);
        }
        if self.moves_since_summary >= summary_every {
            self.moves_since_summary = 0;
            focuses.push(Focus::DevelopmentSummary);
        }

        if focuses.is_empty() {
            Decision::Silent
        } else {
            Decision::Full(focuses)
        }
    }
}

/// The explicit instruction the LLM receives for one focus. These keep the
/// model on a single point per message instead of a generic reaction.
pub fn focus_instruction(focus: &Focus) -> String {
    match focus {
        Focus::Encourage => {
            "Acknowledge what the move accomplishes before anything else — name the \
             specific thing it does, not generic praise."
                .into()
        }
        Focus::ExplainWhyGood => {
            "Say WHY the move is strong in one concrete sentence, grounded in the \
             engine facts and the situation report."
                .into()
        }
        Focus::ExplainMistake => {
            "Acknowledge what the move was trying to do, then explain the single most \
             important thing it missed."
                .into()
        }
        Focus::ThreatWarning => {
            "Name the single most important thing to watch right now — one threat or \
             opportunity, nothing else."
                .into()
        }
        Focus::OpeningNote => {
            "Mention the opening by name (from the situation report) and one idea \
             behind it."
                .into()
        }
        Focus::PhaseTransition => {
            "Point out that the game is entering a new phase and what that changes \
             about what to look for."
                .into()
        }
        Focus::DevelopmentSummary => {
            "Narrate in one or two sentences how the game has developed so far — the \
             story, not a move list."
                .into()
        }
        Focus::Milestone(kind) => match kind {
            MilestoneKind::Castled => {
                "The student just castled — note what that does for king safety.".into()
            }
            MilestoneKind::FirstCapture => {
                "That was the game's first capture — note that the position is opening \
                 up and trades have begun."
                    .into()
            }
            MilestoneKind::Promotion => {
                "A pawn just promoted — celebrate it and note what the new piece changes."
                    .into()
            }
            MilestoneKind::Check => {
                "The move gives check — note what the check forces.".into()
            }
            MilestoneKind::QueensOff => {
                "The queens just left the board — note how the game changes without them."
                    .into()
            }
        },
    }
}

/// Expanded canned commentary pool: at least 4 deterministic variants per
/// judgment class, rotated by move number (never randomness). Warmer and
/// more specific than the old one-liners — each references the move played.
/// Mate-related lines take priority regardless of rotation.
pub fn brief_line(verdict: &MoveVerdict, move_number: u32) -> String {
    let san = &verdict.played_san;
    if verdict.allows_mate_in.is_some() {
        return format!(
            "{san} is dangerous — it gives your opponent a forced checkmate. Look at \
             the squares around your king."
        );
    }
    if verdict.missed_mate_in.is_some() {
        return format!(
            "{san} lets a big chance slip — you had a forced win here. Look for the \
             most forcing moves."
        );
    }
    let i = move_number as usize;
    let pick = |lines: [String; 4]| -> String { lines[i % 4].clone() };
    match verdict.judgment {
        Judgment::Best | Judgment::Excellent => pick([
            format!("{san} — nice, that's right in line with the engine's choice."),
            format!("{san} is exactly the move. Well spotted."),
            format!("Great find — {san} was the strongest idea in the position."),
            format!("{san} — that's precisely what the position called for."),
        ]),
        Judgment::Good => pick([
            format!("{san} is a solid move."),
            format!("{san} keeps your position healthy — good, steady chess."),
            format!("Nothing wrong with {san}. You're playing sensibly."),
            format!("{san} does the job — keep building on it."),
        ]),
        Judgment::Inaccuracy => pick([
            format!("{san} is playable, but there was something a bit stronger here."),
            format!("{san} is okay — not the sharpest choice, though. Worth a second look later."),
            format!("You can get away with {san}, but the engine liked another idea better."),
            format!("{san} gives back a little of your edge — small thing, keep going."),
        ]),
        Judgment::Mistake | Judgment::Blunder => pick([
            format!("{san} runs into trouble — take another look at this position."),
            format!("{san} has a problem — slow down here and check what your opponent can do."),
            format!("Careful — {san} gives your opponent a real chance. See if you can spot it."),
            format!("{san} loses some ground. Before your next move, ask what changed."),
        ]),
    }
}

/// Canned opponent-move note for when the LLM is unavailable (engine-only
/// play with `NullBackend`): a "keep an eye on…" line built purely from the
/// deterministic context. Same rotation-by-move-number determinism.
pub fn engine_only_note(
    focuses: &[Focus],
    ctx: &OpponentContext,
    eval_for_student_cp: Option<i32>,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if focuses.contains(&Focus::ThreatWarning) {
        if ctx.threatens_mate {
            parts.push(
                "Careful — there are mating ideas against your king now. Check every \
                 check before you move."
                    .into(),
            );
        } else if let Some(desc) = ctx.motifs_against_student.first() {
            parts.push(format!("Keep an eye on this: {desc}."));
        } else if ctx.eval_swing_cp < 0 {
            parts.push(format!(
                "{} changed things — look at what it attacks before you reply.",
                ctx.opponent_san
            ));
        } else {
            parts.push(
                "Your opponent's last move may have given you something — look for \
                 undefended pieces."
                    .into(),
            );
        }
    }
    if focuses.contains(&Focus::PhaseTransition) {
        parts.push(format!(
            "The game is moving into the {} now.",
            ctx.phase.label()
        ));
    }
    if focuses.contains(&Focus::DevelopmentSummary) {
        let eval_text = match eval_for_student_cp {
            Some(cp) if cp >= 60 => "you're ahead",
            Some(cp) if cp <= -60 => "you're under some pressure",
            Some(_) => "the game is balanced",
            None => "the game goes on",
        };
        parts.push(format!(
            "Move {}: we're in the {} and {}.",
            ctx.move_number,
            ctx.phase.label(),
            eval_text
        ));
    }
    if parts.is_empty() {
        parts.push(format!("Your opponent played {}.", ctx.opponent_san));
    }
    parts.join(" ")
}

/// Render the first `max` moves of a UCI principal variation as SAN, by
/// replaying them on a clone of the game. Stops early (silently) at the
/// first move that fails to replay — a stale PV must never panic.
pub fn pv_to_san(game: &GameState, pv: &[String], max: usize) -> Vec<String> {
    let mut clone = game.clone();
    let mut out = Vec::new();
    for uci in pv.iter().take(max) {
        match clone.play_uci(uci) {
            Ok(san) => out.push(san),
            Err(_) => break,
        }
    }
    out
}

/// Material balance in pawn units, positive = `student` is ahead.
fn material_balance(pos: &Chess, student: Color) -> i32 {
    let board = pos.board();
    let mut diff = 0i32;
    for (role, v) in [
        (Role::Pawn, 1),
        (Role::Knight, 3),
        (Role::Bishop, 3),
        (Role::Rook, 5),
        (Role::Queen, 9),
    ] {
        let bb = board.by_role(role);
        diff += (bb & board.by_color(student)).count() as i32 * v;
        diff -= (bb & board.by_color(student.other())).count() as i32 * v;
    }
    diff
}

fn color_name(c: Color) -> &'static str {
    match c {
        Color::White => "white",
        Color::Black => "black",
    }
}

/// Build the compact deterministic situation report the LLM receives with a
/// `Full` decision. Plain text, clearly labeled lines, under ~120 words.
///
/// `eval_history_cp` is the student's last few post-move evals (oldest
/// first, student's perspective). `analysis` is a fresh engine analysis of
/// the *current* position, if the session has one cached; its top line is
/// verbalized as the current best idea (the opponent's idea when it is the
/// opponent's turn, a suggested plan when it is the student's).
pub fn situation_report(
    game: &GameState,
    student: Color,
    phase: Phase,
    eval_history_cp: &[i32],
    analysis: Option<&Analysis>,
    opening: Option<&Opening>,
) -> String {
    let pos = game.position();
    let mut lines: Vec<String> = Vec::new();

    lines.push(format!(
        "Move: {} ({})",
        pos.fullmoves().get(),
        phase.label()
    ));
    if let Some(op) = opening {
        lines.push(format!("Opening: {} {}", op.eco, op.name));
    }

    let mat = material_balance(pos, student);
    let mat_text = match mat {
        0 => "even".to_string(),
        d if d > 0 => format!("student is up {d} point{}", if d == 1 { "" } else { "s" }),
        d => format!("student is down {} point{}", -d, if d == -1 { "" } else { "s" }),
    };
    lines.push(format!("Material: {mat_text}"));

    // Current eval, student's perspective: prefer the fresh analysis (whose
    // top-line score is from the side to move), fall back to history.
    let eval_now = analysis
        .and_then(|a| a.lines.first())
        .map(|l| {
            let cp = l.score.as_cp();
            if pos.turn() == student {
                cp
            } else {
                -cp
            }
        })
        .or_else(|| eval_history_cp.last().copied());
    if let Some(cp) = eval_now {
        let trend = if eval_history_cp.len() >= 3 {
            let window = &eval_history_cp[eval_history_cp.len() - 3..];
            let delta = window[2] - window[0];
            if delta > 40 {
                ", trending up"
            } else if delta < -40 {
                ", trending down"
            } else {
                ", steady"
            }
        } else {
            ""
        };
        lines.push(format!(
            "Eval: {:+.2} for the student{trend}",
            cp as f64 / 100.0
        ));
    }

    // Motifs, both sides, capped so the report stays compact.
    let found = motifs::detect(pos);
    let against_student: Vec<&str> = found
        .iter()
        .filter(|m| m.against == color_name(student))
        .map(|m| m.description.as_str())
        .take(2)
        .collect();
    let for_student: Vec<&str> = found
        .iter()
        .filter(|m| m.against == color_name(student.other()))
        .map(|m| m.description.as_str())
        .take(2)
        .collect();
    lines.push(format!(
        "Threats: {}",
        if against_student.is_empty() {
            "none detected against the student".to_string()
        } else {
            against_student.join("; ")
        }
    ));
    if !for_student.is_empty() {
        lines.push(format!("Tactics for the student: {}", for_student.join("; ")));
    }

    if let Some(a) = analysis {
        if let Some(top) = a.lines.first() {
            let sans = pv_to_san(game, &top.pv, 3);
            if !sans.is_empty() {
                let label = if pos.turn() == student {
                    "Suggested plan"
                } else {
                    "Opponent's idea"
                };
                lines.push(format!("{label}: {}", sans.join(", then ")));
            }
        }
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verdict(judgment: Judgment) -> MoveVerdict {
        MoveVerdict {
            played_san: "Nf3".into(),
            cp_loss: match judgment {
                Judgment::Best => 0,
                Judgment::Excellent => 15,
                Judgment::Good => 40,
                Judgment::Inaccuracy => 80,
                Judgment::Mistake => 180,
                Judgment::Blunder => 400,
            },
            judgment,
            best_move_uci: "g1f3".into(),
            best_line: vec!["g1f3".into()],
            eval_before_cp: 30,
            eval_after_cp: 30,
            allows_mate_in: None,
            missed_mate_in: None,
        }
    }

    fn ctx() -> OpponentContext {
        OpponentContext {
            opponent_san: "e5".into(),
            move_number: 2,
            eval_swing_cp: 0,
            threatens_mate: false,
            wins_material: false,
            motifs_against_student: vec![],
            phase: Phase::Opening,
        }
    }

    // ---------- style × student-move matrix ----------

    #[test]
    fn quiet_notable_is_full_explain_mistake() {
        let mut p = CommentaryPolicy::new(CommentaryStyle::Quiet);
        for j in [Judgment::Inaccuracy, Judgment::Mistake, Judgment::Blunder] {
            let d = p.on_student_move(&verdict(j), &[], 5, Phase::Opening, false);
            assert_eq!(d, Decision::Full(vec![Focus::ExplainMistake]), "{j:?}");
        }
    }

    #[test]
    fn quiet_good_is_brief_even_with_milestones_and_opening() {
        let mut p = CommentaryPolicy::new(CommentaryStyle::Quiet);
        let d = p.on_student_move(
            &verdict(Judgment::Good),
            &[MilestoneKind::Castled],
            5,
            Phase::Middlegame,
            true,
        );
        assert_eq!(d, Decision::Brief);
    }

    #[test]
    fn quiet_opponent_always_silent() {
        let mut p = CommentaryPolicy::new(CommentaryStyle::Quiet);
        let mut c = ctx();
        c.threatens_mate = true;
        c.eval_swing_cp = -900;
        for _ in 0..20 {
            assert_eq!(p.on_opponent_move(&c), Decision::Silent);
        }
    }

    #[test]
    fn balanced_notable_is_full_and_adds_threat_on_allowed_mate() {
        let mut p = CommentaryPolicy::new(CommentaryStyle::Balanced);
        let d = p.on_student_move(&verdict(Judgment::Mistake), &[], 5, Phase::Opening, false);
        assert_eq!(d, Decision::Full(vec![Focus::ExplainMistake]));

        let mut v = verdict(Judgment::Blunder);
        v.allows_mate_in = Some(2);
        let d = p.on_student_move(&v, &[], 6, Phase::Opening, false);
        assert_eq!(
            d,
            Decision::Full(vec![Focus::ExplainMistake, Focus::ThreatWarning])
        );
    }

    #[test]
    fn balanced_every_third_good_move_gets_why_good() {
        let mut p = CommentaryPolicy::new(CommentaryStyle::Balanced);
        let v = verdict(Judgment::Good);
        assert_eq!(p.on_student_move(&v, &[], 1, Phase::Opening, false), Decision::Brief);
        assert_eq!(p.on_student_move(&v, &[], 2, Phase::Opening, false), Decision::Brief);
        assert_eq!(
            p.on_student_move(&v, &[], 3, Phase::Opening, false),
            Decision::Full(vec![Focus::ExplainWhyGood])
        );
        // Streak reset: two more briefs before the next full.
        assert_eq!(p.on_student_move(&v, &[], 4, Phase::Opening, false), Decision::Brief);
        assert_eq!(p.on_student_move(&v, &[], 5, Phase::Opening, false), Decision::Brief);
        assert_eq!(
            p.on_student_move(&v, &[], 6, Phase::Opening, false),
            Decision::Full(vec![Focus::ExplainWhyGood])
        );
    }

    #[test]
    fn balanced_notable_resets_good_streak() {
        let mut p = CommentaryPolicy::new(CommentaryStyle::Balanced);
        let good = verdict(Judgment::Good);
        p.on_student_move(&good, &[], 1, Phase::Opening, false);
        p.on_student_move(&good, &[], 2, Phase::Opening, false);
        p.on_student_move(&verdict(Judgment::Mistake), &[], 3, Phase::Opening, false);
        // Streak restarted: needs three more goods.
        assert_eq!(p.on_student_move(&good, &[], 4, Phase::Opening, false), Decision::Brief);
        assert_eq!(p.on_student_move(&good, &[], 5, Phase::Opening, false), Decision::Brief);
        assert!(matches!(
            p.on_student_move(&good, &[], 6, Phase::Opening, false),
            Decision::Full(_)
        ));
    }

    #[test]
    fn balanced_milestone_forces_full() {
        let mut p = CommentaryPolicy::new(CommentaryStyle::Balanced);
        let d = p.on_student_move(
            &verdict(Judgment::Good),
            &[MilestoneKind::Castled],
            5,
            Phase::Opening,
            false,
        );
        assert_eq!(d, Decision::Full(vec![Focus::Milestone(MilestoneKind::Castled)]));
    }

    #[test]
    fn balanced_opening_announced_exactly_once() {
        let mut p = CommentaryPolicy::new(CommentaryStyle::Balanced);
        let v = verdict(Judgment::Good);
        let d = p.on_student_move(&v, &[], 1, Phase::Opening, true);
        assert_eq!(d, Decision::Full(vec![Focus::OpeningNote]));
        // Still identified on later moves, never announced again — and the
        // good-move streak restarted after that full reaction.
        let d = p.on_student_move(&v, &[], 2, Phase::Opening, true);
        assert_eq!(d, Decision::Brief);
        let d = p.on_student_move(&v, &[], 3, Phase::Opening, true);
        assert_eq!(d, Decision::Brief);
        let d = p.on_student_move(&v, &[], 4, Phase::Opening, true);
        assert_eq!(d, Decision::Full(vec![Focus::ExplainWhyGood]));
    }

    #[test]
    fn balanced_phase_transition_on_student_move() {
        let mut p = CommentaryPolicy::new(CommentaryStyle::Balanced);
        let v = verdict(Judgment::Good);
        p.on_student_move(&v, &[], 5, Phase::Opening, false);
        let d = p.on_student_move(&v, &[], 6, Phase::Middlegame, false);
        assert_eq!(d, Decision::Full(vec![Focus::PhaseTransition]));
        // No re-announcement while the phase holds.
        assert_eq!(p.on_student_move(&v, &[], 7, Phase::Middlegame, false), Decision::Brief);
    }

    #[test]
    fn chatty_every_student_move_is_full() {
        let mut p = CommentaryPolicy::new(CommentaryStyle::Chatty);
        for (n, j) in [
            (1, Judgment::Best),
            (2, Judgment::Good),
            (3, Judgment::Excellent),
            (4, Judgment::Inaccuracy),
            (5, Judgment::Blunder),
        ] {
            let d = p.on_student_move(&verdict(j), &[], n, Phase::Opening, false);
            assert!(matches!(d, Decision::Full(_)), "move {n} {j:?} → {d:?}");
        }
    }

    #[test]
    fn chatty_good_move_focuses_encourage_then_why() {
        let mut p = CommentaryPolicy::new(CommentaryStyle::Chatty);
        let d = p.on_student_move(&verdict(Judgment::Good), &[], 1, Phase::Opening, false);
        assert_eq!(d, Decision::Full(vec![Focus::Encourage, Focus::ExplainWhyGood]));
    }

    // ---------- style × opponent-move matrix ----------

    #[test]
    fn balanced_opponent_quiet_move_is_silent() {
        let mut p = CommentaryPolicy::new(CommentaryStyle::Balanced);
        assert_eq!(p.on_opponent_move(&ctx()), Decision::Silent);
    }

    #[test]
    fn balanced_opponent_threat_or_big_swing_warns() {
        let mut p = CommentaryPolicy::new(CommentaryStyle::Balanced);
        let mut c = ctx();
        c.threatens_mate = true;
        assert_eq!(p.on_opponent_move(&c), Decision::Full(vec![Focus::ThreatWarning]));

        let mut c = ctx();
        c.wins_material = true;
        assert_eq!(p.on_opponent_move(&c), Decision::Full(vec![Focus::ThreatWarning]));

        let mut c = ctx();
        c.eval_swing_cp = -200;
        assert_eq!(p.on_opponent_move(&c), Decision::Full(vec![Focus::ThreatWarning]));

        // Balanced ignores bare motifs (static detectors over-report).
        let mut c = ctx();
        c.motifs_against_student = vec!["the white knight on f3 is attacked".into()];
        assert_eq!(p.on_opponent_move(&c), Decision::Silent);

        // A sub-threshold swing stays silent.
        let mut c = ctx();
        c.eval_swing_cp = -100;
        assert_eq!(p.on_opponent_move(&c), Decision::Silent);
    }

    #[test]
    fn balanced_summary_every_ten_full_moves() {
        let mut p = CommentaryPolicy::new(CommentaryStyle::Balanced);
        let c = ctx();
        for i in 1..10 {
            assert_eq!(p.on_opponent_move(&c), Decision::Silent, "move {i}");
        }
        assert_eq!(
            p.on_opponent_move(&c),
            Decision::Full(vec![Focus::DevelopmentSummary])
        );
        // Counter resets.
        for i in 1..10 {
            assert_eq!(p.on_opponent_move(&c), Decision::Silent, "second lap {i}");
        }
        assert_eq!(
            p.on_opponent_move(&c),
            Decision::Full(vec![Focus::DevelopmentSummary])
        );
    }

    #[test]
    fn balanced_phase_transition_on_opponent_move() {
        let mut p = CommentaryPolicy::new(CommentaryStyle::Balanced);
        let mut c = ctx();
        c.phase = Phase::Middlegame;
        assert_eq!(p.on_opponent_move(&c), Decision::Full(vec![Focus::PhaseTransition]));
        assert_eq!(p.on_opponent_move(&c), Decision::Silent);
    }

    #[test]
    fn chatty_opponent_flags_motifs_and_summarizes_every_six() {
        let mut p = CommentaryPolicy::new(CommentaryStyle::Chatty);
        let mut c = ctx();
        c.motifs_against_student = vec!["the white queen on d1 is attacked".into()];
        assert_eq!(p.on_opponent_move(&c), Decision::Full(vec![Focus::ThreatWarning]));

        let quiet = ctx();
        for i in 2..6 {
            assert_eq!(p.on_opponent_move(&quiet), Decision::Silent, "move {i}");
        }
        assert_eq!(
            p.on_opponent_move(&quiet),
            Decision::Full(vec![Focus::DevelopmentSummary])
        );
    }

    #[test]
    fn chatty_lower_swing_threshold() {
        let mut p = CommentaryPolicy::new(CommentaryStyle::Chatty);
        let mut c = ctx();
        c.eval_swing_cp = -100; // silent in Balanced, flagged in Chatty
        assert_eq!(p.on_opponent_move(&c), Decision::Full(vec![Focus::ThreatWarning]));
    }

    #[test]
    fn combined_focuses_accumulate() {
        let mut p = CommentaryPolicy::new(CommentaryStyle::Balanced);
        let quiet = ctx();
        for _ in 0..9 {
            p.on_opponent_move(&quiet);
        }
        let mut c = ctx();
        c.threatens_mate = true;
        c.phase = Phase::Middlegame;
        assert_eq!(
            p.on_opponent_move(&c),
            Decision::Full(vec![
                Focus::ThreatWarning,
                Focus::PhaseTransition,
                Focus::DevelopmentSummary
            ])
        );
    }

    // ---------- phase detection ----------

    fn phase_of(fen: &str) -> Phase {
        detect_phase(GameState::from_fen(fen).unwrap().position())
    }

    #[test]
    fn starting_position_is_opening() {
        assert_eq!(detect_phase(GameState::new().position()), Phase::Opening);
    }

    #[test]
    fn developed_position_past_move_ten_is_middlegame() {
        // Full material, queens on, fullmove 15.
        assert_eq!(
            phase_of("r4rk1/ppq2ppp/2n1bn2/2b1p3/2B1P3/2N1BN2/PPQ2PPP/R4RK1 w - - 4 15"),
            Phase::Middlegame
        );
    }

    #[test]
    fn developed_position_before_move_ten_is_middlegame() {
        // Fullmove 8 but only one minor piece still at home (< 2): developed.
        assert_eq!(
            phase_of("r1bq1rk1/pppp1ppp/2n2n2/4p1B1/2B1P3/2N2N2/PPPP1PPP/R2Q1RK1 w - - 6 8"),
            Phase::Middlegame
        );
    }

    #[test]
    fn queens_off_with_low_material_is_endgame() {
        // Two rooks each (total 20) with queens off: still a middlegame.
        assert_eq!(
            phase_of("2rr2k1/5ppp/8/8/8/8/5PPP/2RR2K1 b - - 0 30"),
            Phase::Middlegame
        );
        // One rook each (total 10): endgame.
        assert_eq!(phase_of("3r2k1/5ppp/8/8/8/8/5PPP/3R2K1 b - - 0 30"), Phase::Endgame);
    }

    #[test]
    fn early_queen_trade_is_not_endgame() {
        // Queens traded on move 5 but every other piece still on the board.
        assert_eq!(
            phase_of("rnb1kbnr/pppp1ppp/8/4p3/4P3/8/PPPP1PPP/RNB1KBNR w KQkq - 0 5"),
            Phase::Opening
        );
    }

    #[test]
    fn bare_kings_and_pawns_is_endgame() {
        assert_eq!(phase_of("8/5pk1/8/8/8/6K1/5P2/8 w - - 0 40"), Phase::Endgame);
    }

    // ---------- situation report ----------

    #[test]
    fn situation_report_structure() {
        use crate::engine::{Score, ScoredLine};
        let mut game = GameState::new();
        game.play_san("e4").unwrap();
        game.play_san("e5").unwrap();
        game.play_san("Nf3").unwrap();
        // Black to move; student is White. Mock analysis from Black's view.
        let analysis = Analysis {
            best_move: "b8c6".into(),
            lines: vec![ScoredLine {
                multipv: 1,
                depth: 12,
                score: Score::Cp(-25),
                pv: vec!["b8c6".into(), "f1b5".into(), "g8f6".into()],
            }],
        };
        let opening = crate::game::openings::lookup(game.history_san());
        let report = situation_report(
            &game,
            Color::White,
            Phase::Opening,
            &[10, 20, 70],
            Some(&analysis),
            opening.as_ref(),
        );

        assert!(report.contains("Move: 2 (opening)"), "report:\n{report}");
        assert!(report.contains("Opening: "), "report:\n{report}");
        assert!(report.contains("Material: even"), "report:\n{report}");
        // Score -25 from Black's view → +0.25 for the student, rising trend.
        assert!(report.contains("Eval: +0.25 for the student, trending up"), "report:\n{report}");
        assert!(report.contains("Threats: "), "report:\n{report}");
        assert!(
            report.contains("Opponent's idea: Nc6, then Bb5, then Nf6"),
            "report:\n{report}"
        );
        assert!(
            report.split_whitespace().count() < 120,
            "report too long:\n{report}"
        );
    }

    #[test]
    fn situation_report_material_imbalance_and_no_analysis() {
        // White is up a queen; no analysis, short eval history.
        let game = GameState::from_fen("4k3/8/8/8/8/8/8/Q3K3 w - - 0 20").unwrap();
        let report = situation_report(&game, Color::White, Phase::Endgame, &[500], None, None);
        assert!(report.contains("Material: student is up 9 points"), "report:\n{report}");
        assert!(report.contains("Eval: +5.00 for the student"), "report:\n{report}");
        assert!(!report.contains("trending"), "no trend with short history:\n{report}");
        assert!(!report.contains("idea"), "no plan line without analysis:\n{report}");
    }

    // ---------- canned pools ----------

    #[test]
    fn brief_lines_rotate_by_move_number_and_cite_the_san() {
        for j in [
            Judgment::Best,
            Judgment::Good,
            Judgment::Inaccuracy,
            Judgment::Blunder,
        ] {
            let v = verdict(j);
            let lines: Vec<String> = (1..=4).map(|n| brief_line(&v, n)).collect();
            let distinct: std::collections::HashSet<&String> = lines.iter().collect();
            assert_eq!(distinct.len(), 4, "{j:?} pool should have 4 rotating variants");
            for l in &lines {
                assert!(l.contains("Nf3"), "{j:?} line must cite the move: {l}");
            }
            // Deterministic: same move number, same line.
            assert_eq!(brief_line(&v, 7), brief_line(&v, 7));
            assert_eq!(brief_line(&v, 3), brief_line(&v, 7));
        }
    }

    #[test]
    fn brief_line_mate_cases_take_priority() {
        let mut v = verdict(Judgment::Blunder);
        v.allows_mate_in = Some(3);
        for n in 1..=4 {
            assert!(brief_line(&v, n).contains("forced checkmate"));
        }
        let mut v = verdict(Judgment::Mistake);
        v.missed_mate_in = Some(2);
        assert!(brief_line(&v, 1).contains("forced win"));
    }

    #[test]
    fn engine_only_notes_cover_focuses() {
        let mut c = ctx();
        c.motifs_against_student = vec!["the white rook on a1 is attacked and has no defender".into()];
        c.eval_swing_cp = -180;
        let note = engine_only_note(&[Focus::ThreatWarning], &c, Some(-120));
        assert!(note.contains("Keep an eye on this: the white rook on a1"), "{note}");

        let mut c = ctx();
        c.threatens_mate = true;
        let note = engine_only_note(&[Focus::ThreatWarning], &c, None);
        assert!(note.contains("mating ideas"), "{note}");

        let c = ctx();
        let note = engine_only_note(
            &[Focus::PhaseTransition, Focus::DevelopmentSummary],
            &c,
            Some(20),
        );
        assert!(note.contains("moving into the opening"), "{note}");
        assert!(note.contains("Move 2:"), "{note}");
        assert!(note.contains("balanced"), "{note}");
    }

    #[test]
    fn pv_to_san_stops_at_illegal_moves() {
        let game = GameState::new();
        let sans = pv_to_san(
            &game,
            &["e2e4".into(), "e7e5".into(), "z9z9".into(), "g1f3".into()],
            4,
        );
        assert_eq!(sans, vec!["e4".to_string(), "e5".to_string()]);
    }
}
