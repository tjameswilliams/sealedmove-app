//! Whole-game review: the post-mortem a coach gives once the clocks stop.
//!
//! The pipeline is deliberately hybrid, because the two obvious designs are
//! both wrong. Re-searching every position at full depth is accurate but
//! takes minutes on a phone; reusing only the per-move verdicts already in
//! the store is instant but blind to everything the *opponent* did, since
//! only student moves are ever judged live.
//!
//! So: a cheap [`SCAN_DEPTH`] sweep evaluates every position in the game and
//! turns it into an eval curve, the curve's biggest swings identify a
//! handful of hotspots, and only those get a full-depth re-search. Cost
//! scales with game length, not with game length times depth.
//!
//! Anchoring follows the same rule as the rest of the coach: the engine
//! picks the moments, the LLM only writes prose into slots it was handed.
//! A model cannot invent a turning point that never happened, and a reply
//! that fails to parse degrades to deterministic notes rather than nothing.

use serde::{Deserialize, Serialize};
use shakmaty::{Color, Position};

use super::{CoachError, CoachSession};
use crate::coach::commentary;
use crate::engine::{estimated_rating_from_acl, judge_move, Analysis, GameAccuracy, Judgment};
use crate::game::{motifs, openings, GameState};
use crate::llm::{CompletionRequest, Message};

/// Depth of the sweep over every position. Low enough that a 60-ply game
/// scans in seconds, high enough that a real blunder shows up as a swing.
pub const SCAN_DEPTH: u32 = 10;
/// How many moments a review anchors by default. More than a handful stops
/// being a lesson and starts being a move list.
pub const DEFAULT_MAX_MOMENTS: usize = 5;
/// What a move that ends the game in checkmate scores for the mover, in
/// centipawns. Matches `Score::as_cp`'s mate range: the engine cannot
/// search a terminal position, so the score is assigned, not searched.
const MATE_SCORE_CP: i32 = 9_999;
/// Centipawn swing a student move must lose to count as a slip.
const SLIP_CP: i32 = 90;
/// Centipawn swing an opponent move must hand over to count as a gift.
const GIFT_CP: i32 = 120;
/// How much better the best move must be than the runner-up for a position
/// to count as sharp — the setup for "you found the only move".
const SHARP_GAP_CP: i32 = 100;
/// Ceiling on a student move's loss for it to still read as a strength.
const STRENGTH_MAX_LOSS_CP: i32 = 25;

/// Why a moment earned its place in the review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MomentKind {
    /// The single largest evaluation swing of the game.
    TurningPoint,
    /// A student move that gave up significant ground.
    Slip,
    /// A sharp position where the student found the move.
    Strength,
    /// An opponent move that handed the student something.
    Gift,
}

impl MomentKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::TurningPoint => "turning point",
            Self::Slip => "slip",
            Self::Strength => "strength",
            Self::Gift => "gift",
        }
    }
}

/// One anchored note in a review. Everything here is engine output; `note`
/// is the only field the LLM writes, and it is optional.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewMoment {
    /// 0-based ply of the move this note is about — the anchor the app
    /// scrubs the board to.
    pub ply: usize,
    pub san: String,
    pub by_student: bool,
    pub kind: MomentKind,
    /// Position BEFORE the move, so the app can replay into it.
    pub fen_before: String,
    /// Position after the move: where the board lands.
    pub fen_after: String,
    /// Evals in centipawns, student's perspective.
    pub eval_before_cp: i32,
    pub eval_after_cp: i32,
    /// Ground lost by the mover, relative to the engine's best.
    pub cp_loss: i32,
    pub judgment: Option<Judgment>,
    /// What the engine wanted instead, in SAN.
    pub best_san: Option<String>,
    /// The first few moves of the engine's line from `fen_before`.
    pub best_line_san: Vec<String>,
    /// Motifs standing in the position after the move.
    pub motifs: Vec<String>,
    /// Deterministic one-line title, always present.
    pub headline: String,
    /// The coach's prose for this moment. Falls back to a deterministic
    /// sentence when no model is available or its reply did not parse.
    pub note: String,
}

/// A finished review: the report card plus its anchored moments.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameReview {
    /// One-line verdict on the game.
    pub headline: String,
    /// A short paragraph of overall assessment.
    pub summary: String,
    pub strengths: Vec<String>,
    pub weaknesses: Vec<String>,
    /// Accuracy over the student's moves, recomputed across the whole game
    /// at [`SCAN_DEPTH`] so reviewing a past game gives the same answer as
    /// reviewing a live one.
    pub accuracy: f64,
    pub acl: f64,
    pub est_rating: u32,
    pub moves_judged: u32,
    /// Deepest book line the game followed, if any.
    pub opening: Option<openings::Opening>,
    /// How many plies the game stayed in book.
    pub book_plies: usize,
    pub moments: Vec<ReviewMoment>,
    /// True when the prose came from the model; false when every line is
    /// the deterministic fallback. The app labels the difference.
    pub narrated: bool,
}

/// Per-ply facts from the scan, before hotspot selection.
struct ScanPoint {
    /// Eval of the position after this ply, student's perspective.
    eval_cp: i32,
    /// Gap between the best and second-best move in the position BEFORE
    /// this ply — how sharp the choice was.
    sharpness_cp: i32,
    fen_before: String,
    fen_after: String,
    by_student: bool,
}

impl CoachSession {
    /// Review the game currently on the board.
    pub async fn review_current_game(&mut self) -> Result<GameReview, CoachError> {
        let moves = self.game.history_san().to_vec();
        let start = self.game_start_fen.clone();
        let student = self.student_color;
        self.review_moves(&moves, start.as_deref(), student, DEFAULT_MAX_MOMENTS)
            .await
    }

    /// Review an arbitrary move list — the same engine path serves the game
    /// just finished and any game pulled back out of history. The live
    /// game state is never touched: the scan replays onto a scratch board.
    pub async fn review_moves(
        &mut self,
        moves: &[String],
        starting_fen: Option<&str>,
        student: Color,
        max_moments: usize,
    ) -> Result<GameReview, CoachError> {
        let scan = self.scan_game(moves, starting_fen, student).await?;

        // Accuracy over the student's moves, from the scan's eval curve.
        let mut accuracy = GameAccuracy::new();
        for (i, point) in scan.iter().enumerate() {
            if !point.by_student {
                continue;
            }
            accuracy.record(cp_loss_at(&scan, i));
        }
        let acl = accuracy.avg_centipawn_loss();

        let mut moments = self.build_moments(&scan, moves, max_moments).await?;
        let opening = openings::lookup(moves);
        let book_plies = opening.as_ref().map(|o| o.matched_plies).unwrap_or(0);

        // Deterministic prose first: it is the answer if the model is
        // absent, fails, or replies with something unparseable.
        for moment in &mut moments {
            moment.note = deterministic_note(moment);
        }
        let mut review = GameReview {
            headline: deterministic_headline(accuracy.accuracy_percent(), &moments),
            summary: deterministic_summary(&moments, opening.as_ref(), acl),
            strengths: deterministic_strengths(&moments),
            weaknesses: deterministic_weaknesses(&moments),
            accuracy: accuracy.accuracy_percent(),
            acl,
            est_rating: estimated_rating_from_acl(acl),
            moves_judged: accuracy.moves(),
            opening,
            book_plies,
            moments,
            narrated: false,
        };

        self.narrate_review(&mut review, student).await;
        Ok(review)
    }

    /// Sweep the whole game at [`SCAN_DEPTH`], producing the eval curve
    /// every later step reads.
    async fn scan_game(
        &mut self,
        moves: &[String],
        starting_fen: Option<&str>,
        student: Color,
    ) -> Result<Vec<ScanPoint>, CoachError> {
        let mut game = match starting_fen {
            Some(fen) => GameState::from_fen(fen)?,
            None => GameState::new(),
        };
        let mut scan = Vec::with_capacity(moves.len());
        for san in moves {
            let fen_before = game.fen();
            let mover = game.position().turn();
            let before = self.analyst.analyze(&fen_before, SCAN_DEPTH, 2).await?;
            // Sharpness only means something for the side actually
            // choosing: how much worse the runner-up was.
            let sharpness_cp = match (before.lines.first(), before.lines.get(1)) {
                (Some(best), Some(second)) => best.score.as_cp() - second.score.as_cp(),
                _ => 0,
            };
            game.play_san(san)?;
            let fen_after = game.fen();
            // A terminal position gives the engine nothing to search (no
            // lines), which would read a delivered mate as 0.00 (level).
            // Score checkmate directly; other endings really are level.
            let eval_cp = if game.position().is_checkmate() {
                if mover == student { MATE_SCORE_CP } else { -MATE_SCORE_CP }
            } else {
                let after = self.analyst.analyze(&fen_after, SCAN_DEPTH, 1).await?;
                perspective(&after, game.position().turn(), student)
            };
            scan.push(ScanPoint {
                eval_cp,
                sharpness_cp,
                fen_before,
                fen_after,
                by_student: mover == student,
            });
        }
        Ok(scan)
    }

    /// Pick the hotspots off the eval curve and re-search each at full
    /// depth, turning them into anchored moments.
    async fn build_moments(
        &mut self,
        scan: &[ScanPoint],
        moves: &[String],
        max_moments: usize,
    ) -> Result<Vec<ReviewMoment>, CoachError> {
        let mut chosen = select_moments(scan, max_moments);
        chosen.sort_by_key(|(ply, _)| *ply);

        let mut out = Vec::with_capacity(chosen.len());
        for (ply, kind) in chosen {
            let point = &scan[ply];
            // Full-depth re-search of the position they were choosing in:
            // the scan told us WHERE to look, this tells us what was true.
            let deep = self
                .analyst
                .analyze(&point.fen_before, self.analysis_depth, 2)
                .await?;
            let mut before_game = replay_to(moves, ply, &point.fen_before)?;
            let best_line_san = deep
                .lines
                .first()
                .map(|l| commentary::pv_to_san(&before_game, &l.pv, 4))
                .unwrap_or_default();
            let best_san = best_line_san.first().cloned();

            // Both evals from the mover's perspective, so cp_loss reads the
            // same way judge_move does live.
            let eval_before_mover = deep
                .lines
                .first()
                .map(|l| l.score.as_cp())
                .unwrap_or(point.eval_cp);
            before_game.play_san(&moves[ply])?;
            // Same terminal-position rule as the scan: a delivered mate is
            // a win for the mover, not the 0.00 an empty search reports.
            let eval_after_mover = if before_game.position().is_checkmate() {
                MATE_SCORE_CP
            } else {
                let after = self
                    .analyst
                    .analyze(&before_game.fen(), self.analysis_depth, 1)
                    .await?;
                -after.lines.first().map(|l| l.score.as_cp()).unwrap_or(0)
            };
            let (cp_loss, judgment) = judge_move(eval_before_mover, eval_after_mover);

            // Stored evals stay in the student's frame, matching the curve.
            let sign = if point.by_student { 1 } else { -1 };
            let motifs = motifs::detect_top(before_game.position(), 3)
                .into_iter()
                .map(|m| m.description)
                .collect();

            let moment = ReviewMoment {
                ply,
                san: moves[ply].clone(),
                by_student: point.by_student,
                kind,
                fen_before: point.fen_before.clone(),
                fen_after: point.fen_after.clone(),
                eval_before_cp: eval_before_mover * sign,
                eval_after_cp: eval_after_mover * sign,
                cp_loss,
                judgment: Some(judgment),
                best_san,
                best_line_san,
                motifs,
                headline: String::new(),
                note: String::new(),
            };
            out.push(ReviewMoment {
                headline: deterministic_headline_for(&moment),
                ..moment
            });
        }
        Ok(out)
    }

    /// Hand the deterministic report to the model for prose. Failure of any
    /// kind leaves the deterministic text in place — the review is already
    /// complete and useful before this runs.
    async fn narrate_review(&mut self, review: &mut GameReview, student: Color) {
        if !self.narration_enabled() {
            // A stub backend cannot write about a game it was told about;
            // the deterministic notes already say the true thing, and
            // `narrated` stays false so the app can label them.
            return;
        }
        let facts = review_facts(review, student);
        let user = format!(
            "Write the post-game review for your student. You are given the engine's complete \
             findings; every number and every move below is fact, and the moments were chosen \
             by the engine, not by you.\n\n{facts}\n\n\
             Reply with JSON only, in exactly this shape:\n\
             {{\"headline\": \"one short sentence on how the game went\", \
             \"summary\": \"two or three sentences: what they did well, what cost them, how it \
             turned\", \"strengths\": [\"...\"], \"weaknesses\": [\"...\"], \
             \"moments\": [{{\"ply\": <one of the ply numbers above>, \"note\": \"one or two \
             sentences on that moment, addressed to the student as 'you'\"}}]}}\n\
             Write a note for every ply listed. Do not add plies that are not listed. Name \
             concrete moves and squares. No move numbers you were not given."
        );
        let system = "You are a chess coach writing a post-game review. The engine has already \
                      done the analysis; narrate it, never re-analyze it. Be direct and warm, \
                      the way a club coach is after a game. Reply with JSON only."
            .to_string();

        let Ok(raw) = self.complete_once(system, user).await else {
            return;
        };
        let Some(draft) = parse_draft(&raw) else {
            return;
        };
        apply_draft(review, draft);
        review.narrated = true;
    }

    /// A single completion outside the running coaching conversation: no
    /// tools (every fact is already in the prompt) and no transcript, so
    /// reviewing a past game cannot bleed into the live game's context.
    async fn complete_once(
        &mut self,
        system: String,
        user: String,
    ) -> Result<String, CoachError> {
        let request = CompletionRequest {
            system,
            messages: vec![Message::user(user)],
            tools: Vec::new(),
        };
        let response = self.model.complete(&request).await?;
        self.stats.llm_calls += 1;
        self.stats.input_tokens += response.usage.input_tokens;
        self.stats.output_tokens += response.usage.output_tokens;
        Ok(response.text.unwrap_or_default())
    }
}

/// Read an analysis from the student's point of view. Engine scores come
/// from the side to move; flip when that is not the student.
fn perspective(analysis: &Analysis, side_to_move: Color, student: Color) -> i32 {
    let cp = analysis.lines.first().map(|l| l.score.as_cp()).unwrap_or(0);
    if side_to_move == student {
        cp
    } else {
        -cp
    }
}

/// Ground the mover gave up on ply `i`, from the scan curve. The curve is
/// in the student's frame, so an opponent move's loss is the negated swing.
fn cp_loss_at(scan: &[ScanPoint], i: usize) -> i32 {
    let before = if i == 0 { scan[0].eval_cp } else { scan[i - 1].eval_cp };
    let swing = scan[i].eval_cp - before;
    let from_mover = if scan[i].by_student { swing } else { -swing };
    (-from_mover).max(0)
}

/// Choose which plies become moments. Deterministic: the same game always
/// yields the same review.
fn select_moments(scan: &[ScanPoint], max_moments: usize) -> Vec<(usize, MomentKind)> {
    if scan.is_empty() || max_moments == 0 {
        return Vec::new();
    }
    let mut candidates: Vec<(usize, MomentKind, i32)> = Vec::new();
    for (i, point) in scan.iter().enumerate() {
        let before = if i == 0 { scan[0].eval_cp } else { scan[i - 1].eval_cp };
        let swing = point.eval_cp - before;
        let loss = cp_loss_at(scan, i);
        if point.by_student {
            if loss >= SLIP_CP {
                candidates.push((i, MomentKind::Slip, loss));
            } else if point.sharpness_cp >= SHARP_GAP_CP && loss <= STRENGTH_MAX_LOSS_CP {
                // A sharp position where they picked the right branch.
                candidates.push((i, MomentKind::Strength, point.sharpness_cp));
            }
        } else if swing >= GIFT_CP {
            candidates.push((i, MomentKind::Gift, swing));
        }
    }
    // Weight by magnitude, then by ply so ties resolve the same way twice.
    candidates.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
    candidates.truncate(max_moments);

    // The heaviest swing of the game is the turning point, whatever else
    // it also was.
    if let Some(peak) = candidates
        .iter()
        .enumerate()
        .filter(|(_, c)| c.1 != MomentKind::Strength)
        .max_by_key(|(_, c)| c.2)
        .map(|(idx, _)| idx)
    {
        candidates[peak].1 = MomentKind::TurningPoint;
    }
    candidates.into_iter().map(|(ply, kind, _)| (ply, kind)).collect()
}

/// Replay `moves[..ply]` onto a fresh board. `fen_before` is the position
/// the scan already computed for that ply; replaying reproduces the move
/// history too, which SAN conversion of the engine's line needs.
fn replay_to(moves: &[String], ply: usize, fen_before: &str) -> Result<GameState, CoachError> {
    let mut game = GameState::new();
    for san in moves.iter().take(ply) {
        if game.play_san(san).is_err() {
            // A history that will not replay (custom start position) still
            // reviews fine from the bare FEN; only the SAN of the engine's
            // line loses its move numbering.
            return Ok(GameState::from_fen(fen_before)?);
        }
    }
    if game.fen() != fen_before {
        return Ok(GameState::from_fen(fen_before)?);
    }
    Ok(game)
}

// MARK: - Deterministic prose
//
// Every one of these is the answer when no model is available. They are
// plain, specific, and never speculative — the engine's findings in words.

fn pawns(cp: i32) -> String {
    format!("{:+.2}", cp as f64 / 100.0)
}

fn deterministic_headline_for(m: &ReviewMoment) -> String {
    let label = commentary::move_label(m.ply, &m.san);
    match m.kind {
        MomentKind::TurningPoint => format!("{label}: the turning point"),
        MomentKind::Slip => format!("{label}: {} centipawns given up", m.cp_loss),
        MomentKind::Strength => format!("{label}: the right move in a sharp spot"),
        MomentKind::Gift => format!("{label}: your opponent slipped"),
    }
}

fn deterministic_note(m: &ReviewMoment) -> String {
    let label = commentary::move_label(m.ply, &m.san);
    let mut text = match m.kind {
        MomentKind::Strength => format!(
            "{label} was the move the engine wanted, and the alternatives were much worse."
        ),
        MomentKind::Gift => format!(
            "{label} let the evaluation swing your way, from {} to {}.",
            pawns(m.eval_before_cp),
            pawns(m.eval_after_cp)
        ),
        _ => format!(
            "{label} moved the evaluation from {} to {}, giving up {} centipawns.",
            pawns(m.eval_before_cp),
            pawns(m.eval_after_cp),
            m.cp_loss
        ),
    };
    if let Some(best) = &m.best_san {
        if m.kind != MomentKind::Strength && m.by_student {
            text.push_str(&format!(" The engine preferred {best}."));
        }
    }
    if let Some(motif) = m.motifs.first() {
        text.push_str(&format!(" In the resulting position, {motif}."));
    }
    text
}

fn deterministic_headline(accuracy: f64, moments: &[ReviewMoment]) -> String {
    let slips = moments
        .iter()
        .filter(|m| m.by_student && m.kind != MomentKind::Strength)
        .count();
    match (accuracy, slips) {
        (a, 0) if a >= 80.0 => "A clean game with no real slips.".into(),
        (a, _) if a >= 80.0 => "Strong play overall, with a couple of moments to learn from.".into(),
        (_, 0) => "A steady game; the engine found nothing sharp to fault.".into(),
        (_, n) => format!(
            "{n} moment{} decided how this one went.",
            if n == 1 { "" } else { "s" }
        ),
    }
}

fn deterministic_summary(
    moments: &[ReviewMoment],
    opening: Option<&openings::Opening>,
    acl: f64,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(op) = opening {
        parts.push(format!(
            "You played the {} ({}), following book for {} moves.",
            op.name,
            op.eco,
            op.matched_plies.div_ceil(2)
        ));
    }
    parts.push(format!(
        "Across the game you gave up {acl:.0} centipawns per move on average."
    ));
    if let Some(turning) = moments.iter().find(|m| m.kind == MomentKind::TurningPoint) {
        parts.push(format!(
            "The game turned on {}.",
            commentary::move_label(turning.ply, &turning.san)
        ));
    }
    parts.join(" ")
}

fn deterministic_strengths(moments: &[ReviewMoment]) -> Vec<String> {
    moments
        .iter()
        .filter(|m| m.kind == MomentKind::Strength)
        .map(|m| {
            format!(
                "You found {} in a position where the alternatives were far worse.",
                commentary::move_label(m.ply, &m.san)
            )
        })
        .collect()
}

fn deterministic_weaknesses(moments: &[ReviewMoment]) -> Vec<String> {
    moments
        .iter()
        .filter(|m| m.by_student && matches!(m.kind, MomentKind::Slip | MomentKind::TurningPoint))
        .map(|m| match &m.best_san {
            Some(best) => format!(
                "{} cost {} centipawns; {best} was the move.",
                commentary::move_label(m.ply, &m.san),
                m.cp_loss
            ),
            None => format!(
                "{} cost {} centipawns.",
                commentary::move_label(m.ply, &m.san),
                m.cp_loss
            ),
        })
        .collect()
}

/// The facts block handed to the model. Mirrors the situation report's
/// contract: labelled plain text, nothing the engine did not produce.
fn review_facts(review: &GameReview, student: Color) -> String {
    let mut lines = vec![format!(
        "Student played: {}",
        if student == Color::White { "White" } else { "Black" }
    )];
    if let Some(op) = &review.opening {
        lines.push(format!(
            "Opening: {} {} (in book for {} plies)",
            op.eco, op.name, review.book_plies
        ));
    }
    lines.push(format!(
        "Accuracy: {:.1}% over {} judged moves, average loss {:.0} centipawns, rating estimate {}",
        review.accuracy, review.moves_judged, review.acl, review.est_rating
    ));
    lines.push(String::new());
    lines.push("Moments the engine selected (write one note for each):".into());
    for m in &review.moments {
        let mut line = format!(
            "- ply {}: {} ({}, played by {}), eval {} to {}, {} centipawns lost",
            m.ply,
            commentary::move_label(m.ply, &m.san),
            m.kind.label(),
            if m.by_student { "the student" } else { "the opponent" },
            pawns(m.eval_before_cp),
            pawns(m.eval_after_cp),
            m.cp_loss
        );
        if let Some(best) = &m.best_san {
            line.push_str(&format!(", engine preferred {best}"));
            if m.best_line_san.len() > 1 {
                line.push_str(&format!(" ({})", m.best_line_san.join(" ")));
            }
        }
        if !m.motifs.is_empty() {
            line.push_str(&format!("; in that position: {}", m.motifs.join("; ")));
        }
        lines.push(line);
    }
    lines.join("\n")
}

// MARK: - Model reply

#[derive(Debug, Deserialize)]
struct ReviewDraft {
    headline: Option<String>,
    summary: Option<String>,
    #[serde(default)]
    strengths: Vec<String>,
    #[serde(default)]
    weaknesses: Vec<String>,
    #[serde(default)]
    moments: Vec<MomentNote>,
}

#[derive(Debug, Deserialize)]
struct MomentNote {
    ply: usize,
    note: String,
}

/// Pull the JSON object out of a model reply. Models wrap JSON in fences,
/// prefix it with "Here you go:", or both; the object itself is what
/// matters, so take the outermost braces and try that.
fn parse_draft(raw: &str) -> Option<ReviewDraft> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if end <= start {
        return None;
    }
    serde_json::from_str(&raw[start..=end]).ok()
}

/// Merge the model's prose over the deterministic review. Empty fields are
/// left as they were, and notes for plies the engine did not choose are
/// dropped — a model cannot add a moment, only fill one in.
fn apply_draft(review: &mut GameReview, draft: ReviewDraft) {
    if let Some(headline) = draft.headline.filter(|s| !s.trim().is_empty()) {
        review.headline = headline;
    }
    if let Some(summary) = draft.summary.filter(|s| !s.trim().is_empty()) {
        review.summary = summary;
    }
    let clean = |list: Vec<String>| -> Vec<String> {
        list.into_iter().filter(|s| !s.trim().is_empty()).collect()
    };
    let strengths = clean(draft.strengths);
    if !strengths.is_empty() {
        review.strengths = strengths;
    }
    let weaknesses = clean(draft.weaknesses);
    if !weaknesses.is_empty() {
        review.weaknesses = weaknesses;
    }
    for note in draft.moments {
        if note.note.trim().is_empty() {
            continue;
        }
        if let Some(moment) = review.moments.iter_mut().find(|m| m.ply == note.ply) {
            moment.note = note.note;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(eval_cp: i32, by_student: bool, sharpness_cp: i32) -> ScanPoint {
        ScanPoint {
            eval_cp,
            sharpness_cp,
            fen_before: String::new(),
            fen_after: String::new(),
            by_student,
        }
    }

    /// Student to move each even ply; the curve dives on ply 4.
    fn curve() -> Vec<ScanPoint> {
        vec![
            point(20, true, 10),
            point(15, false, 10),
            point(25, true, 10),
            point(30, false, 10),
            point(-300, true, 10),  // ply 4: student blunders
            point(-290, false, 10), // opponent keeps it
            point(-120, true, 10),
            point(10, false, 10), // ply 7: opponent hands it back
        ]
    }

    #[test]
    fn biggest_swing_becomes_the_turning_point() {
        let chosen = select_moments(&curve(), 5);
        let turning: Vec<_> = chosen
            .iter()
            .filter(|(_, k)| *k == MomentKind::TurningPoint)
            .collect();
        assert_eq!(turning.len(), 1, "exactly one turning point");
        assert_eq!(turning[0].0, 4);
    }

    #[test]
    fn opponent_swing_reads_as_a_gift() {
        let chosen = select_moments(&curve(), 5);
        assert!(chosen
            .iter()
            .any(|(ply, kind)| *ply == 7 && *kind == MomentKind::Gift));
    }

    #[test]
    fn selection_respects_the_cap_and_is_stable() {
        let first = select_moments(&curve(), 2);
        let second = select_moments(&curve(), 2);
        assert_eq!(first.len(), 2);
        assert_eq!(
            first.iter().map(|c| c.0).collect::<Vec<_>>(),
            second.iter().map(|c| c.0).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_clean_game_yields_no_moments() {
        let flat: Vec<ScanPoint> = (0..10).map(|i| point(20, i % 2 == 0, 10)).collect();
        assert!(select_moments(&flat, 5).is_empty());
    }

    #[test]
    fn finding_the_only_move_in_a_sharp_spot_is_a_strength() {
        let sharp = vec![point(40, true, 250), point(35, false, 10)];
        let chosen = select_moments(&sharp, 5);
        assert!(chosen
            .iter()
            .any(|(ply, kind)| *ply == 0 && *kind == MomentKind::Strength));
    }

    #[test]
    fn cp_loss_reads_from_the_movers_side() {
        let scan = curve();
        // Ply 4 is the student's: the curve fell 330, so they lost 330.
        assert_eq!(cp_loss_at(&scan, 4), 330);
        // Ply 7 is the opponent's and the curve rose: their loss, not the
        // student's gain.
        assert_eq!(cp_loss_at(&scan, 7), 130);
    }

    #[test]
    fn draft_parses_out_of_a_fenced_reply() {
        let raw = "Sure, here it is:\n```json\n{\"headline\": \"Sharp game.\", \
                   \"moments\": [{\"ply\": 4, \"note\": \"This was the moment.\"}]}\n```";
        let draft = parse_draft(raw).expect("parses");
        assert_eq!(draft.headline.as_deref(), Some("Sharp game."));
        assert_eq!(draft.moments.len(), 1);
    }

    #[test]
    fn a_reply_that_is_not_json_leaves_the_review_alone() {
        assert!(parse_draft("I could not analyze that game, sorry.").is_none());
    }

    fn review_with_moment(ply: usize) -> GameReview {
        GameReview {
            headline: "deterministic headline".into(),
            summary: "deterministic summary".into(),
            strengths: vec![],
            weaknesses: vec![],
            accuracy: 70.0,
            acl: 40.0,
            est_rating: 1200,
            moves_judged: 20,
            opening: None,
            book_plies: 0,
            moments: vec![ReviewMoment {
                ply,
                san: "Bb5".into(),
                by_student: true,
                kind: MomentKind::Slip,
                fen_before: String::new(),
                fen_after: String::new(),
                eval_before_cp: 30,
                eval_after_cp: -300,
                cp_loss: 330,
                judgment: Some(Judgment::Blunder),
                best_san: Some("Nf3".into()),
                best_line_san: vec!["Nf3".into()],
                motifs: vec![],
                headline: "headline".into(),
                note: "deterministic note".into(),
            }],
            narrated: false,
        }
    }

    #[test]
    fn the_model_can_only_fill_slots_the_engine_chose() {
        let mut review = review_with_moment(4);
        let draft = parse_draft(
            "{\"moments\": [{\"ply\": 4, \"note\": \"Real note.\"}, \
             {\"ply\": 99, \"note\": \"Invented moment.\"}]}",
        )
        .unwrap();
        apply_draft(&mut review, draft);
        assert_eq!(review.moments.len(), 1, "no moment was added");
        assert_eq!(review.moments[0].note, "Real note.");
    }

    #[test]
    fn empty_model_fields_keep_the_deterministic_text() {
        let mut review = review_with_moment(4);
        let draft = parse_draft("{\"headline\": \"  \", \"strengths\": []}").unwrap();
        apply_draft(&mut review, draft);
        assert_eq!(review.headline, "deterministic headline");
        assert_eq!(review.moments[0].note, "deterministic note");
    }
}
