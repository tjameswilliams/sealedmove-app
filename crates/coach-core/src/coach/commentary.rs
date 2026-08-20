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
    /// Shift-triggered: the coach stays silent until the engine decides the
    /// game has significantly shifted (the eval has drifted
    /// [`SHIFT_THRESHOLD_CP`] since the coach last spoke, a mistake/blunder
    /// landed, or a forced mate appeared). It then recaps the stretch of
    /// moves that led to the swing in one message instead of interrupting
    /// on every move.
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

/// The policy's answer: say nothing, use a canned line, invoke the LLM with
/// the given focuses, or recap the stretch that led to a significant shift.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Silent,
    Brief,
    Full(Vec<Focus>),
    /// Balanced only: the game has significantly shifted since the coach
    /// last spoke. `from_ply` is the 0-based half-move index of the last
    /// comment; the recap window is `history[from_ply..]`.
    ShiftRecap { from_ply: usize },
}

/// Eval drift (centipawns, student's perspective) since the coach last
/// spoke that Balanced counts as "the game has significantly shifted".
pub const SHIFT_THRESHOLD_CP: i32 = 120;

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
    /// Absolute eval after the opponent's move, student's perspective
    /// (None when the engine gave no line). Balanced's shift detection
    /// compares this against the eval when the coach last spoke.
    pub eval_for_student_cp: Option<i32>,
    /// Half-moves played after the opponent's move (history length).
    pub ply: usize,
}

/// Deterministic commentary cadence: WHEN to speak, WHAT to focus on. Pure
/// state machine — no I/O, no randomness. The session owns one per game.
#[derive(Debug, Clone)]
pub struct CommentaryPolicy {
    style: CommentaryStyle,
    /// Consecutive ordinary good-class student moves that only got a brief
    /// line; every 3rd earns a full "why that was good" (Chatty only).
    good_streak: u32,
    /// Full moves since the last development summary.
    moves_since_summary: u32,
    /// The opening gets announced at most once per game.
    opening_announced: bool,
    /// Last phase the policy saw; a change triggers PhaseTransition.
    phase: Phase,
    /// Half-moves played when the coach last spoke: the start of the
    /// window a Balanced shift recap covers.
    last_spoken_ply: usize,
    /// Eval (student's perspective) when the coach last spoke; Balanced
    /// measures shift as drift from this anchor. Starts at 0 (a level
    /// game).
    anchor_eval_cp: i32,
}

impl CommentaryPolicy {
    pub fn new(style: CommentaryStyle) -> Self {
        Self {
            style,
            good_streak: 0,
            moves_since_summary: 0,
            opening_announced: false,
            phase: Phase::Opening,
            last_spoken_ply: 0,
            anchor_eval_cp: 0,
        }
    }

    /// The coach just spoke about the position at `ply` half-moves with the
    /// game at `eval_cp`: later shift detection measures from here.
    fn mark_spoken(&mut self, ply: usize, eval_cp: i32) {
        self.last_spoken_ply = ply;
        self.anchor_eval_cp = eval_cp;
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
    /// `ply_now` is the history length after the move; `phase_now` is the
    /// phase after the move; `opening_identified` is whether the move
    /// history still matches a named opening.
    pub fn on_student_move(
        &mut self,
        verdict: &MoveVerdict,
        milestones: &[MilestoneKind],
        move_number: u32,
        ply_now: usize,
        phase_now: Phase,
        opening_identified: bool,
    ) -> Decision {
        let _ = move_number; // rotation happens at the canned-line call site
        let phase_changed = phase_now != self.phase;
        self.phase = phase_now;
        let notable = verdict.judgment.is_notable();
        let eval_now = verdict.eval_after_cp;

        let decision = match self.style {
            CommentaryStyle::Quiet => {
                // Exactly today's behavior: LLM on notable moves, canned
                // line otherwise; milestones, openings, and phases go
                // unremarked.
                if notable {
                    Decision::Full(vec![Focus::ExplainMistake])
                } else {
                    Decision::Brief
                }
            }
            CommentaryStyle::Balanced => {
                // Shift-triggered: nothing until the game meaningfully
                // swings, then ONE recap of the stretch that led here. A
                // mistake/blunder or a mate appearing IS a shift, whatever
                // the drift; milestones, openings, and phases never
                // interrupt on their own.
                let shifted = matches!(verdict.judgment, Judgment::Mistake | Judgment::Blunder)
                    || verdict.allows_mate_in.is_some()
                    || verdict.missed_mate_in.is_some()
                    || (eval_now - self.anchor_eval_cp).abs() >= SHIFT_THRESHOLD_CP;
                if shifted {
                    Decision::ShiftRecap {
                        from_ply: self.last_spoken_ply,
                    }
                } else {
                    Decision::Silent
                }
            }
            CommentaryStyle::Chatty => {
                let mut focuses: Vec<Focus> = Vec::new();
                if notable {
                    focuses.push(Focus::ExplainMistake);
                    if verdict.allows_mate_in.is_some() {
                        focuses.push(Focus::ThreatWarning);
                    }
                } else {
                    // Every move gets the full treatment.
                    focuses.push(Focus::Encourage);
                    focuses.push(Focus::ExplainWhyGood);
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
                Decision::Full(focuses)
            }
        };

        if !matches!(decision, Decision::Silent) {
            self.good_streak = 0;
            self.mark_spoken(ply_now, eval_now);
        }
        decision
    }

    /// Decide how to react to the opponent's just-played move.
    pub fn on_opponent_move(&mut self, ctx: &OpponentContext) -> Decision {
        if self.style == CommentaryStyle::Quiet {
            return Decision::Silent;
        }
        let phase_changed = ctx.phase != self.phase;
        self.phase = ctx.phase;

        if self.style == CommentaryStyle::Balanced {
            // Same shift rule as student moves: a forced mate against the
            // student, or eval drift past the threshold since the coach
            // last spoke. No motif nagging, no scheduled summaries.
            let drifted = ctx
                .eval_for_student_cp
                .map(|e| (e - self.anchor_eval_cp).abs() >= SHIFT_THRESHOLD_CP)
                .unwrap_or(false);
            if ctx.threatens_mate || drifted {
                let from_ply = self.last_spoken_ply;
                self.mark_spoken(
                    ctx.ply,
                    ctx.eval_for_student_cp.unwrap_or(self.anchor_eval_cp),
                );
                return Decision::ShiftRecap { from_ply };
            }
            return Decision::Silent;
        }

        // Chatty.
        self.moves_since_summary += 1;
        let (swing_threshold, summary_every) = (90, 6);

        let mut focuses: Vec<Focus> = Vec::new();
        let threatening = ctx.threatens_mate
            || ctx.wins_material
            || ctx.eval_swing_cp.abs() >= swing_threshold
            || !ctx.motifs_against_student.is_empty();
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
            if let Some(e) = ctx.eval_for_student_cp {
                self.mark_spoken(ctx.ply, e);
            }
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

    // Motifs, both sides, capped so the report stays compact. Tactical and
    // positional findings get separate lines: a passed pawn is worth
    // mentioning, but it must never displace a hanging queen. `detect`
    // returns them priority-ordered, so each `take` keeps the sharpest.
    let found = motifs::detect(pos);
    let tactical = |against: Color| -> Vec<&str> {
        found
            .iter()
            .filter(|m| m.kind.is_tactical() && m.against == color_name(against))
            .map(|m| m.description.as_str())
            .take(2)
            .collect()
    };
    let against_student = tactical(student);
    let for_student = tactical(student.other());
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
    let positional: Vec<&str> = found
        .iter()
        .filter(|m| !m.kind.is_tactical())
        .map(|m| m.description.as_str())
        .take(2)
        .collect();
    if !positional.is_empty() {
        lines.push(format!("Position notes: {}", positional.join("; ")));
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

// MARK: - Pause recap

/// One student move inside a recap window: what they played and what the
/// engine thought of it while the coach was keeping quiet.
#[derive(Debug, Clone)]
pub struct RecapMove {
    /// 0-based ply in the game history (matches the store's `moves.ply`).
    pub ply: usize,
    pub san: String,
    pub judgment: Option<Judgment>,
    pub cp_loss: Option<i32>,
}

/// "12. Nf3" / "12… Nc6" for a 0-based ply.
pub fn move_label(ply: usize, san: &str) -> String {
    let number = ply / 2 + 1;
    if ply.is_multiple_of(2) {
        format!("{number}. {san}")
    } else {
        format!("{number}… {san}")
    }
}

/// The moves of a recap window as a readable line: "12. Nf3 Nc6, 13. Bb5".
fn moves_line(history: &[String], from: usize) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut i = from;
    while i < history.len() {
        let number = i / 2 + 1;
        if i.is_multiple_of(2) {
            let mut chunk = format!("{number}. {}", history[i]);
            if i + 1 < history.len() {
                chunk.push(' ');
                chunk.push_str(&history[i + 1]);
                i += 1;
            }
            out.push(chunk);
        } else {
            out.push(format!("{number}… {}", history[i]));
        }
        i += 1;
    }
    out.join(", ")
}

/// The judged student moves of the window, worst first so the interesting
/// ones survive a truncation: "13. Bb5: inaccuracy, 60 centipawns lost".
fn judged_lines(judged: &[RecapMove], max: usize) -> Vec<String> {
    let mut ranked: Vec<&RecapMove> = judged.iter().collect();
    ranked.sort_by_key(|m| -m.cp_loss.unwrap_or(0));
    ranked
        .into_iter()
        .filter(|m| m.judgment.is_some())
        .take(max)
        .map(|m| {
            let label = m.judgment.map(|j| j.label()).unwrap_or("unjudged");
            match m.cp_loss {
                Some(loss) if loss > 0 => format!(
                    "{}: {label}, {loss} centipawns lost",
                    move_label(m.ply, &m.san)
                ),
                _ => format!("{}: {label}", move_label(m.ply, &m.san)),
            }
        })
        .collect()
}

/// Deterministic facts block for a pause recap, handed to the LLM the same
/// way [`situation_report`] is: every claim in it came from the engine or
/// the board, so the model narrates rather than analyzes.
///
/// `from` is the history length when the coach last spoke; the window is
/// `history[from..]`. `eval_span` is (before the window, now) in
/// centipawns from the student's perspective.
pub fn recap_report(
    history: &[String],
    from: usize,
    judged: &[RecapMove],
    eval_span: Option<(i32, i32)>,
    situation: &str,
) -> String {
    let mut lines = vec![format!(
        "Half-moves played while you were paused: {}",
        history.len().saturating_sub(from)
    )];
    lines.push(format!("Moves: {}", moves_line(history, from)));
    let judged_out = judged_lines(judged, 4);
    if judged_out.is_empty() {
        lines.push("Engine verdicts on the student's moves: none recorded".into());
    } else {
        lines.push(format!(
            "Engine verdicts on the student's moves (worst first): {}",
            judged_out.join("; ")
        ));
    }
    if let Some((before, now)) = eval_span {
        lines.push(format!(
            "Eval across the stretch: {:+.2} to {:+.2} for the student",
            before as f64 / 100.0,
            now as f64 / 100.0
        ));
    }
    lines.push(String::new());
    lines.push("Situation now:".into());
    lines.push(situation.to_string());
    lines.join("\n")
}

/// Deterministic facts block for a Balanced shift recap: the stretch of
/// moves since the coach last spoke, the engine's verdicts on them, and the
/// eval swing. Handed to the LLM exactly like [`recap_report`], so the
/// model narrates the shift rather than inventing one.
pub fn shift_report(
    history: &[String],
    from: usize,
    judged: &[RecapMove],
    eval_span: Option<(i32, i32)>,
    situation: &str,
) -> String {
    let mut lines = vec![format!(
        "Half-moves in the stretch leading to the shift: {}",
        history.len().saturating_sub(from)
    )];
    lines.push(format!("Moves: {}", moves_line(history, from)));
    let judged_out = judged_lines(judged, 4);
    if judged_out.is_empty() {
        lines.push("Engine verdicts on the student's moves: none recorded".into());
    } else {
        lines.push(format!(
            "Engine verdicts on the student's moves (worst first): {}",
            judged_out.join("; ")
        ));
    }
    if let Some((before, now)) = eval_span {
        lines.push(format!(
            "Eval across the stretch: {:+.2} to {:+.2} for the student",
            before as f64 / 100.0,
            now as f64 / 100.0
        ));
    }
    lines.push(String::new());
    lines.push("Situation now:".into());
    lines.push(situation.to_string());
    lines.join("\n")
}

/// The shift-recap line for when the LLM is unavailable: same facts, plain
/// prose, no model involved.
pub fn shift_fallback(
    history: &[String],
    from: usize,
    judged: &[RecapMove],
    eval_span: Option<(i32, i32)>,
    phase: Phase,
) -> String {
    let notable: Vec<&RecapMove> = judged
        .iter()
        .filter(|m| m.judgment.is_some_and(|j| j.is_notable()))
        .collect();
    let worst = notable.iter().max_by_key(|m| m.cp_loss.unwrap_or(0));
    // Which way it swung: the eval span when we have one; otherwise a
    // student slip in the window reads as "against you"; otherwise don't
    // claim a direction at all.
    let direction = match eval_span.map(|(before, now)| now - before) {
        Some(d) if d > 0 => Some(true),
        Some(d) if d < 0 => Some(false),
        _ => worst.map(|_| false),
    };
    let mut text = match direction {
        Some(true) => "The game just swung your way.".to_string(),
        Some(false) => "The game just shifted against you.".to_string(),
        None => "The game just took a turn.".to_string(),
    };
    text.push_str(&format!(
        " Over the last stretch ({}),",
        moves_line(history, from)
    ));
    if let Some(worst) = worst {
        text.push_str(&format!(
            " the engine flagged {} as {}.",
            move_label(worst.ply, &worst.san),
            worst.judgment.map(|j| j.label()).unwrap_or("notable")
        ));
    } else {
        text.push_str(" the position changed hands without a clear slip from you.");
    }
    if let Some((before, now)) = eval_span {
        text.push_str(&format!(
            " The evaluation went from {:+.2} to {:+.2}.",
            before as f64 / 100.0,
            now as f64 / 100.0
        ));
    }
    text.push_str(&format!(
        " We're in the {}. Take a moment here.",
        phase.label()
    ));
    text
}

/// The catch-up line for when the LLM is unavailable: same facts, plain
/// prose, no model involved.
pub fn recap_fallback(
    history: &[String],
    from: usize,
    judged: &[RecapMove],
    eval_span: Option<(i32, i32)>,
    phase: Phase,
) -> String {
    let count = history.len().saturating_sub(from);
    let mut text = format!(
        "Catching you up: {count} half-move{} were played while I was paused ({}).",
        if count == 1 { "" } else { "s" },
        moves_line(history, from)
    );
    let notable: Vec<&RecapMove> = judged
        .iter()
        .filter(|m| m.judgment.is_some_and(|j| j.is_notable()))
        .collect();
    if let Some(worst) = notable.iter().max_by_key(|m| m.cp_loss.unwrap_or(0)) {
        text.push_str(&format!(
            " The engine flagged {} as {}.",
            move_label(worst.ply, &worst.san),
            worst.judgment.map(|j| j.label()).unwrap_or("notable")
        ));
    } else if !judged.is_empty() {
        text.push_str(" Nothing in there worried the engine.");
    }
    if let Some((before, now)) = eval_span {
        let delta = now - before;
        let drift = if delta > 50 {
            "you gained ground over that stretch"
        } else if delta < -50 {
            "you gave up some ground over that stretch"
        } else {
            "the evaluation held roughly steady"
        };
        text.push_str(&format!(
            " Overall {drift}, and you stand at {:+.2} now.",
            now as f64 / 100.0
        ));
    }
    text.push_str(&format!(" We're in the {}.", phase.label()));
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recap_history() -> Vec<String> {
        ["e4", "e5", "Nf3", "Nc6", "Bb5", "a6", "Bxc6", "dxc6"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn move_label_numbers_both_colors() {
        assert_eq!(move_label(0, "e4"), "1. e4");
        assert_eq!(move_label(1, "e5"), "1… e5");
        assert_eq!(move_label(6, "Bxc6"), "4. Bxc6");
    }

    #[test]
    fn recap_report_carries_the_window_not_the_whole_game() {
        let judged = vec![RecapMove {
            ply: 6,
            san: "Bxc6".into(),
            judgment: Some(Judgment::Inaccuracy),
            cp_loss: Some(80),
        }];
        let report = recap_report(
            &recap_history(),
            4,
            &judged,
            Some((30, -50)),
            "Move: 5 (opening)",
        );
        assert!(report.contains("Half-moves played while you were paused: 4"));
        assert!(report.contains("3. Bb5 a6, 4. Bxc6 dxc6"));
        // Moves from before the window must not leak in.
        assert!(!report.contains("1. e4"));
        assert!(report.contains("4. Bxc6: inaccuracy, 80 centipawns lost"));
        assert!(report.contains("+0.30 to -0.50"));
    }

    #[test]
    fn recap_fallback_leads_with_the_worst_move() {
        let judged = vec![
            RecapMove {
                ply: 4,
                san: "Bb5".into(),
                judgment: Some(Judgment::Good),
                cp_loss: Some(30),
            },
            RecapMove {
                ply: 6,
                san: "Bxc6".into(),
                judgment: Some(Judgment::Blunder),
                cp_loss: Some(400),
            },
        ];
        let text = recap_fallback(&recap_history(), 4, &judged, Some((30, -50)), Phase::Opening);
        assert!(text.contains("4 half-moves"));
        assert!(text.contains("flagged 4. Bxc6 as blunder"));
        assert!(text.contains("gave up some ground"));
    }

    #[test]
    fn recap_fallback_says_so_when_nothing_slipped() {
        let judged = vec![RecapMove {
            ply: 4,
            san: "Bb5".into(),
            judgment: Some(Judgment::Best),
            cp_loss: Some(0),
        }];
        let text = recap_fallback(&recap_history(), 4, &judged, None, Phase::Opening);
        assert!(text.contains("Nothing in there worried the engine"));
    }

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
            eval_for_student_cp: Some(0),
            ply: 4,
        }
    }

    /// A verdict whose post-move eval the test controls (shift detection
    /// compares it against the policy's anchor).
    fn verdict_at(judgment: Judgment, eval_after_cp: i32) -> MoveVerdict {
        let mut v = verdict(judgment);
        v.eval_after_cp = eval_after_cp;
        v
    }

    // ---------- style × student-move matrix ----------

    #[test]
    fn quiet_notable_is_full_explain_mistake() {
        let mut p = CommentaryPolicy::new(CommentaryStyle::Quiet);
        for j in [Judgment::Inaccuracy, Judgment::Mistake, Judgment::Blunder] {
            let d = p.on_student_move(&verdict(j), &[], 5, 9, Phase::Opening, false);
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
            9,
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
    fn balanced_ordinary_moves_stay_silent() {
        // Good moves, milestones, a named opening, even a phase change:
        // none of it interrupts while the eval holds steady.
        let mut p = CommentaryPolicy::new(CommentaryStyle::Balanced);
        let v = verdict(Judgment::Good); // eval_after_cp = 30, within threshold
        assert_eq!(p.on_student_move(&v, &[], 1, 1, Phase::Opening, true), Decision::Silent);
        assert_eq!(
            p.on_student_move(&v, &[MilestoneKind::Castled], 2, 3, Phase::Opening, true),
            Decision::Silent
        );
        assert_eq!(
            p.on_student_move(&v, &[], 3, 5, Phase::Middlegame, true),
            Decision::Silent
        );
    }

    #[test]
    fn balanced_mistake_or_blunder_triggers_shift_recap() {
        let mut p = CommentaryPolicy::new(CommentaryStyle::Balanced);
        let good = verdict(Judgment::Good);
        assert_eq!(p.on_student_move(&good, &[], 1, 1, Phase::Opening, false), Decision::Silent);
        assert_eq!(p.on_student_move(&good, &[], 2, 3, Phase::Opening, false), Decision::Silent);
        // The window opens at the last spoken ply (0, never spoke).
        let d = p.on_student_move(&verdict(Judgment::Mistake), &[], 3, 5, Phase::Opening, false);
        assert_eq!(d, Decision::ShiftRecap { from_ply: 0 });
        // Having spoken at ply 5, the next shift's window starts there.
        let d = p.on_student_move(&verdict_at(Judgment::Blunder, -400), &[], 4, 7, Phase::Opening, false);
        assert_eq!(d, Decision::ShiftRecap { from_ply: 5 });
    }

    #[test]
    fn balanced_inaccuracy_alone_does_not_trigger() {
        let mut p = CommentaryPolicy::new(CommentaryStyle::Balanced);
        // Inaccuracy with the eval still near the anchor: no interruption.
        let d = p.on_student_move(&verdict_at(Judgment::Inaccuracy, -60), &[], 1, 1, Phase::Opening, false);
        assert_eq!(d, Decision::Silent);
    }

    #[test]
    fn balanced_eval_drift_triggers_shift_recap() {
        let mut p = CommentaryPolicy::new(CommentaryStyle::Balanced);
        // Slow drift: each move fine on its own, but the game has swung
        // 130cp from the anchor by the third, and that IS the shift.
        assert_eq!(
            p.on_student_move(&verdict_at(Judgment::Good, -40), &[], 1, 1, Phase::Opening, false),
            Decision::Silent
        );
        assert_eq!(
            p.on_student_move(&verdict_at(Judgment::Good, -90), &[], 2, 3, Phase::Opening, false),
            Decision::Silent
        );
        assert_eq!(
            p.on_student_move(&verdict_at(Judgment::Good, -130), &[], 3, 5, Phase::Opening, false),
            Decision::ShiftRecap { from_ply: 0 }
        );
        // Anchor re-based at -130: holding steady is silent again.
        assert_eq!(
            p.on_student_move(&verdict_at(Judgment::Good, -150), &[], 4, 7, Phase::Opening, false),
            Decision::Silent
        );
        // Recovering back past the threshold is ALSO a shift (their way).
        assert_eq!(
            p.on_student_move(&verdict_at(Judgment::Good, 10), &[], 5, 9, Phase::Opening, false),
            Decision::ShiftRecap { from_ply: 5 }
        );
    }

    #[test]
    fn balanced_mate_flags_always_trigger() {
        let mut p = CommentaryPolicy::new(CommentaryStyle::Balanced);
        let mut v = verdict(Judgment::Good);
        v.allows_mate_in = Some(3);
        assert!(matches!(
            p.on_student_move(&v, &[], 1, 1, Phase::Opening, false),
            Decision::ShiftRecap { .. }
        ));
        let mut v = verdict(Judgment::Good);
        v.missed_mate_in = Some(2);
        assert!(matches!(
            p.on_student_move(&v, &[], 2, 3, Phase::Opening, false),
            Decision::ShiftRecap { .. }
        ));
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
            let d = p.on_student_move(&verdict(j), &[], n, n as usize * 2 - 1, Phase::Opening, false);
            assert!(matches!(d, Decision::Full(_)), "move {n} {j:?} → {d:?}");
        }
    }

    #[test]
    fn chatty_good_move_focuses_encourage_then_why() {
        let mut p = CommentaryPolicy::new(CommentaryStyle::Chatty);
        let d = p.on_student_move(&verdict(Judgment::Good), &[], 1, 1, Phase::Opening, false);
        assert_eq!(d, Decision::Full(vec![Focus::Encourage, Focus::ExplainWhyGood]));
    }

    #[test]
    fn chatty_milestone_phase_and_opening_still_ride_along() {
        let mut p = CommentaryPolicy::new(CommentaryStyle::Chatty);
        let d = p.on_student_move(
            &verdict(Judgment::Good),
            &[MilestoneKind::Castled],
            5,
            9,
            Phase::Middlegame,
            true,
        );
        assert_eq!(
            d,
            Decision::Full(vec![
                Focus::Encourage,
                Focus::ExplainWhyGood,
                Focus::Milestone(MilestoneKind::Castled),
                Focus::PhaseTransition,
                Focus::OpeningNote,
            ])
        );
    }

    // ---------- style × opponent-move matrix ----------

    #[test]
    fn balanced_opponent_quiet_move_is_silent() {
        let mut p = CommentaryPolicy::new(CommentaryStyle::Balanced);
        assert_eq!(p.on_opponent_move(&ctx()), Decision::Silent);
    }

    #[test]
    fn balanced_opponent_shift_triggers_recap() {
        let mut p = CommentaryPolicy::new(CommentaryStyle::Balanced);
        // Drift past the threshold since the coach last spoke.
        let mut c = ctx();
        c.eval_for_student_cp = Some(-150);
        assert_eq!(p.on_opponent_move(&c), Decision::ShiftRecap { from_ply: 0 });
        // Anchor re-based: the same eval again is silent.
        c.ply = 6;
        assert_eq!(p.on_opponent_move(&c), Decision::Silent);

        // A forced mate against the student always triggers.
        let mut c = ctx();
        c.ply = 8;
        c.eval_for_student_cp = Some(-150);
        c.threatens_mate = true;
        assert_eq!(p.on_opponent_move(&c), Decision::ShiftRecap { from_ply: 4 });
    }

    #[test]
    fn balanced_opponent_ignores_motifs_summaries_and_phases() {
        let mut p = CommentaryPolicy::new(CommentaryStyle::Balanced);
        let mut c = ctx();
        c.motifs_against_student = vec!["the white knight on f3 is attacked".into()];
        c.phase = Phase::Middlegame;
        for i in 0..12 {
            c.ply = 2 * i + 2;
            assert_eq!(p.on_opponent_move(&c), Decision::Silent, "move {i}");
        }
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
    fn chatty_combined_focuses_accumulate() {
        let mut p = CommentaryPolicy::new(CommentaryStyle::Chatty);
        let quiet = ctx();
        for _ in 0..5 {
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
