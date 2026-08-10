//! The coach agent loop: engine facts in, grounded commentary out.

pub mod commentary;
pub mod prompt;
pub mod tools;

pub use commentary::{
    CommentaryPolicy, CommentaryStyle, Decision, Focus, MilestoneKind, OpponentContext, Phase,
};
pub use prompt::Modality;

use crate::engine::{
    accuracy::{estimated_rating_from_acl, GameAccuracy},
    judge_move, Analysis, Judgment, Score, UciEngine,
};
use crate::game::{motifs, openings, GameState};
use crate::llm::{
    CoachModel, CompletionRequest, LlmError, Message, ModelTier, ToolCall,
};
use crate::store::{GameStore, StoreError, StoredChat};
use crate::student::{ConceptStatus, GameRecord, GameResult, StudentProfile};
use serde_json::{json, Value};
use shakmaty::Position as _;
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
pub enum CoachError {
    #[error("llm error: {0}")]
    Llm(#[from] LlmError),
    #[error("engine error: {0}")]
    Engine(#[from] std::io::Error),
    #[error("game error: {0}")]
    Game(#[from] crate::game::GameError),
    #[error("store error: {0}")]
    Store(#[from] StoreError),
    #[error("tool loop exceeded {0} iterations without a final answer")]
    ToolLoopExceeded(usize),
}

/// Engine verdict on the most recent student move — computed eagerly by the
/// session, served to the LLM via the `classify_last_move` tool.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MoveVerdict {
    pub played_san: String,
    pub cp_loss: i32,
    pub judgment: Judgment,
    /// Best move in the pre-move position, UCI.
    pub best_move_uci: String,
    /// Engine's top line from the pre-move position, UCI moves.
    pub best_line: Vec<String>,
    pub eval_before_cp: i32,
    pub eval_after_cp: i32,
    /// The move lets the opponent force checkmate in N moves. The single
    /// most important fact for the coach to surface — spelled out here
    /// explicitly because a bare cp number (±10000) buries it.
    pub allows_mate_in: Option<i32>,
    /// The student had a forced mate in N available and played something
    /// that loses it.
    pub missed_mate_in: Option<i32>,
}

/// Cumulative cost/effort counters for a session — the raw material for the
/// eval harness and for the paid tier's unit economics.
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct SessionStats {
    pub llm_calls: u32,
    pub tool_calls: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Store writes that failed and were dropped (persistence is
    /// best-effort: a failed write never fails the chess operation).
    #[serde(default)]
    pub record_failures: u32,
    /// Engine analyses served from the session's one-entry FEN cache
    /// instead of a fresh search (see `CoachSession::analyze_cached`).
    #[serde(default)]
    pub analysis_cache_hits: u32,
}

/// The bot the student plays against — Maia (lc0 + maia weights) in the
/// product; any UCI engine works. Maia is meant to be searched with
/// `go nodes 1`, which makes it play like a human at its trained band.
pub struct Opponent {
    engine: UciEngine,
    nodes: u64,
}

/// Opponent-via-analyst configuration: the analyst engine doubles as the
/// opponent, dialed down with Stockfish's "Skill Level" option.
///
/// This exists for the embedded-engine platforms (iOS): the statically
/// linked Stockfish is a per-process singleton, so there is no second
/// engine to run as the opponent. It is the iOS prototype path until Maia
/// is linked in-process — Stockfish at low skill blunders differently than
/// a human at the same strength.
#[derive(Debug, Clone, Copy)]
struct AnalystOpponent {
    /// Stockfish "Skill Level", 0..=20.
    skill_level: u8,
    /// Wall-clock budget per opponent move.
    movetime_ms: u32,
}

/// One coaching session: a game, an analyst engine, a student profile, and a
/// model. This is the object the FFI layer will expose to Swift/Kotlin.
pub struct CoachSession {
    model: Arc<dyn CoachModel>,
    analyst: UciEngine,
    opponent: Option<Opponent>,
    /// When set, the analyst doubles as the opponent (embedded-engine
    /// platforms). Mutually exclusive with `opponent` — the setters clear
    /// each other, last call wins.
    analyst_opponent: Option<AnalystOpponent>,
    pub game: GameState,
    pub profile: StudentProfile,
    pub modality: Modality,
    pub stats: SessionStats,
    transcript: Vec<Message>,
    last_verdict: Option<MoveVerdict>,
    analysis_depth: u32,
    /// Per-game accuracy accumulator: fed by every `judge_student_move`,
    /// consumed (and reset) by `finish_game`.
    accuracy: GameAccuracy,
    /// Attached persistence, if any (see [`Self::attach_store`]).
    store: Option<GameStore>,
    /// The store row current moves/chat record into. `None` between
    /// `finish_game` and the next `reset_game` (nothing to record into).
    store_game_id: Option<i64>,
    /// Deterministic commentary cadence (see [`commentary`]).
    policy: CommentaryPolicy,
    /// Post-move evals of the student's last few judged moves (student's
    /// perspective, oldest first, capped at [`EVAL_HISTORY_CAP`]). Feeds the
    /// situation report's trend line.
    eval_history: Vec<i32>,
    /// Milestones detected on the student's most recent move.
    last_milestones: Vec<MilestoneKind>,
    /// Context built from the opponent's most recent move; consumed (taken)
    /// by [`Self::react_to_opponent_move`].
    last_opponent_ctx: Option<OpponentContext>,
    /// One-entry analysis cache keyed by FEN. `opponent_reply` analyzes the
    /// position after the opponent's move to build [`OpponentContext`]; the
    /// immediately following `judge_student_move` needs a "before" analysis
    /// of that very same position, so it reuses this instead of re-searching
    /// (hits are counted in `stats.analysis_cache_hits`). Callers only rely
    /// on the top line, so a cached MultiPV-2 result satisfies a MultiPV-3
    /// request.
    cached_analysis: Option<(String, Analysis)>,
    /// Which color the student plays — learned from whose turn it is when
    /// `judge_student_move` runs. Defaults to White before the first move.
    student_color: shakmaty::Color,
}

/// What `resume_from_store` restored — everything the UI needs to redraw the
/// in-progress game (board, move list, chat feed).
#[derive(Debug, serde::Serialize)]
pub struct ResumeReport {
    pub game_id: i64,
    /// Position after replaying the stored moves.
    pub fen: String,
    pub history_san: Vec<String>,
    pub chat: Vec<StoredChat>,
    /// Bot level the game was started at.
    pub bot_level: u32,
}

/// End-of-game summary returned by [`CoachSession::finish_game`] — the facts
/// the UI shows on the result screen and the LLM can cite in a wrap-up.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct GameSummary {
    /// Average centipawn loss across the student's judged moves.
    pub acl: f64,
    /// Accuracy percentage (0..=100).
    pub accuracy: f64,
    /// Rating estimate for this game alone (pre-EMA).
    pub est_rating: u32,
    /// Whether the (updated) profile now qualifies for a level bump.
    pub ready_to_advance: bool,
    /// Number of student moves the engine judged this game.
    pub moves_judged: u32,
}

/// What one coaching turn produced.
pub struct CoachReply {
    pub commentary: String,
    pub verdict: MoveVerdict,
}

const MAX_TOOL_ITERATIONS: usize = 8;
/// How many student-move evals the session keeps for the trend line.
const EVAL_HISTORY_CAP: usize = 6;

impl CoachSession {
    pub fn new(
        model: Arc<dyn CoachModel>,
        analyst: UciEngine,
        game: GameState,
        profile: StudentProfile,
        modality: Modality,
    ) -> Self {
        Self {
            model,
            analyst,
            opponent: None,
            analyst_opponent: None,
            game,
            profile,
            modality,
            stats: SessionStats::default(),
            transcript: Vec::new(),
            last_verdict: None,
            analysis_depth: 16,
            accuracy: GameAccuracy::new(),
            store: None,
            store_game_id: None,
            policy: CommentaryPolicy::new(CommentaryStyle::default()),
            eval_history: Vec::new(),
            last_milestones: Vec::new(),
            last_opponent_ctx: None,
            cached_analysis: None,
            student_color: shakmaty::Color::White,
        }
    }

    /// Set how much the coach talks (see [`CommentaryStyle`]). Takes effect
    /// on the next move; cadence counters carry over.
    pub fn set_commentary_style(&mut self, style: CommentaryStyle) {
        self.policy.set_style(style);
    }

    /// Which opponent this session is configured with, as the string stored
    /// in `games.opponent_kind`.
    fn opponent_kind(&self) -> String {
        if let Some(cfg) = self.analyst_opponent {
            format!("analyst_skill:{}", cfg.skill_level)
        } else if self.opponent.is_some() {
            "external".to_string()
        } else {
            "none".to_string()
        }
    }

    /// Best-effort store write: no-op when no store/game row is attached; a
    /// failure bumps `stats.record_failures` and is logged, never propagated
    /// — persistence must not break the chess operation it rides on.
    fn store_try(&mut self, f: impl FnOnce(&GameStore, i64) -> Result<(), StoreError>) {
        let Some(game_id) = self.store_game_id else {
            return;
        };
        let Some(store) = self.store.as_ref() else {
            return;
        };
        if let Err(e) = f(store, game_id) {
            self.stats.record_failures += 1;
            eprintln!("coach-core: store write dropped: {e}");
        }
    }

    /// Starting FEN for a new store row: `None` for the standard start,
    /// `Some(current fen)` when the (move-less) game began from a setup.
    fn starting_fen_for_store(&self) -> Option<String> {
        if self.game.history_san().is_empty() {
            let fen = self.game.fen();
            if fen != GameState::new().fen() {
                return Some(fen);
            }
        }
        None
    }

    /// Begin a fresh row in the attached store (closing any still-open one
    /// as aborted, per the store invariant). Best-effort.
    fn store_begin_game(&mut self) {
        let Some(store) = self.store.as_ref() else {
            return;
        };
        let fen = self.starting_fen_for_store();
        match store.begin_game(fen.as_deref(), self.profile.bot_level, &self.opponent_kind()) {
            Ok(id) => self.store_game_id = Some(id),
            Err(e) => {
                self.store_game_id = None;
                self.stats.record_failures += 1;
                eprintln!("coach-core: store begin_game dropped: {e}");
            }
        }
    }

    /// Attach SQLite persistence for the life of the session. If the current
    /// game has no moves yet, a game row is begun immediately (closing any
    /// still-open row as aborted); otherwise the row starts at the next
    /// [`Self::reset_game`]. All subsequent moves, chat, and transcripts
    /// auto-record; a failed write never fails the chess operation (see
    /// `SessionStats::record_failures`).
    pub fn attach_store(&mut self, store: GameStore) -> Result<(), CoachError> {
        self.store = Some(store);
        self.store_game_id = None;
        if self.game.history_san().is_empty() {
            let fen = self.starting_fen_for_store();
            let id = self
                .store
                .as_ref()
                .expect("just set")
                .begin_game(fen.as_deref(), self.profile.bot_level, &self.opponent_kind())?;
            self.store_game_id = Some(id);
        }
        Ok(())
    }

    /// Resume the store's in-progress game, if any, onto this session.
    ///
    /// With an open game: replays its stored moves onto a fresh board (from
    /// its starting FEN), restores the LLM transcript, rebuilds the per-game
    /// accuracy accumulator from the stored student cp-losses, restores the
    /// last student verdict, adopts the row for further writes, and returns
    /// the [`ResumeReport`]. The restored `last_verdict` carries only the
    /// persisted fields (san, cp_loss, judgment, allows/missed mate);
    /// `best_move_uci`, `best_line`, and the eval fields are defaulted
    /// (empty/0) — they exist per-analysis and are not stored.
    ///
    /// A corrupt open game (bad FEN or a stored move that no longer replays)
    /// is closed as aborted in the store and treated as "no open game" —
    /// resume never errors the app over stale data.
    ///
    /// Without an open game this behaves exactly like [`Self::attach_store`]
    /// and returns `Ok(None)`.
    pub fn resume_from_store(
        &mut self,
        store: GameStore,
    ) -> Result<Option<ResumeReport>, CoachError> {
        let Some(open) = store.open_game()? else {
            self.attach_store(store)?;
            return Ok(None);
        };

        // Replay the stored moves; any corruption aborts the stored game and
        // falls back to a fresh attach.
        let replayed = (|| -> Result<GameState, crate::game::GameError> {
            let mut game = match open.starting_fen.as_deref() {
                Some(f) => GameState::from_fen(f)?,
                None => GameState::new(),
            };
            for m in &open.moves {
                game.play_san(&m.san)?;
            }
            Ok(game)
        })();
        let game = match replayed {
            Ok(g) => g,
            Err(e) => {
                eprintln!(
                    "coach-core: stored game {} no longer replays ({e}); aborting it",
                    open.id
                );
                store.abort_game(open.id)?;
                self.attach_store(store)?;
                return Ok(None);
            }
        };

        self.game = game;
        self.transcript = open
            .transcript_json
            .as_deref()
            .and_then(|j| serde_json::from_str(j).ok())
            .unwrap_or_default();
        self.accuracy = GameAccuracy::new();
        for m in open.moves.iter().filter(|m| m.by_student) {
            if let Some(cp) = m.cp_loss {
                self.accuracy.record(cp);
            }
        }
        self.last_verdict = open
            .moves
            .iter()
            .rev()
            .find(|m| m.by_student)
            .and_then(|m| {
                let judgment = crate::store::judgment_from_str(m.judgment.as_deref()?)?;
                Some(MoveVerdict {
                    played_san: m.san.clone(),
                    cp_loss: m.cp_loss.unwrap_or(0),
                    judgment,
                    // Not persisted — defaulted (documented above).
                    best_move_uci: String::new(),
                    best_line: Vec::new(),
                    eval_before_cp: 0,
                    eval_after_cp: 0,
                    allows_mate_in: m.allows_mate_in,
                    missed_mate_in: m.missed_mate_in,
                })
            });

        let report = ResumeReport {
            game_id: open.id,
            fen: self.game.fen(),
            history_san: self.game.history_san().to_vec(),
            chat: open.chat,
            bot_level: open.bot_level,
        };
        self.store = Some(store);
        self.store_game_id = Some(open.id);
        Ok(Some(report))
    }

    /// Persist a message the UI generated locally (canned greeting, backend
    /// switch notice, review annotation) into the game's chat feed.
    /// Best-effort no-op without an attached store. `role` should be one of
    /// student/coach/system.
    pub fn log_feed(&mut self, role: &str, is_review: bool, text: &str) {
        let ply = self.game.history_san().len() as u32;
        self.store_try(|s, id| s.record_chat(id, ply, role, is_review, text));
    }

    /// Serialize and persist the LLM transcript (best-effort).
    fn store_save_transcript(&mut self) {
        let Ok(json) = serde_json::to_string(&self.transcript) else {
            return;
        };
        self.store_try(|s, id| s.save_transcript(id, &json));
    }

    /// Record a just-played move (last in history) into the store, plus the
    /// opening identification when the book still matches. Best-effort.
    fn store_record_last_move(
        &mut self,
        uci: Option<&str>,
        by_student: bool,
        verdict: Option<&MoveVerdict>,
    ) {
        let history = self.game.history_san();
        let Some(san) = history.last().cloned() else {
            return;
        };
        // 0-based ply of the move just played.
        let ply = (history.len() - 1) as u32;
        let opening = openings::lookup(history);
        self.store_try(|s, id| {
            s.record_move(id, ply, &san, uci, by_student, verdict)?;
            if let Some(op) = &opening {
                s.set_opening(id, &op.eco, &op.name)?;
            }
            Ok(())
        });
    }

    /// Swap the LLM backend at runtime, keeping the engines, game, profile,
    /// and stats. Clears the conversation transcript — a new coach must not
    /// inherit another model's tool-call history (tool-call ids and message
    /// shapes are backend-specific, and replaying them confuses the new
    /// model).
    ///
    /// This exists because embedded-engine platforms (iOS) cannot build a
    /// second session per process — the statically linked Stockfish is a
    /// singleton — so switching coach backends must happen in place.
    pub fn set_model(&mut self, model: Arc<dyn CoachModel>) {
        self.model = model;
        self.transcript.clear();
    }

    /// Attach an opponent engine (Maia). `nodes` is the search budget per
    /// move — 1 for authentic Maia play.
    pub fn set_opponent(&mut self, engine: UciEngine, nodes: u64) {
        self.analyst_opponent = None;
        self.opponent = Some(Opponent { engine, nodes });
    }

    /// Use the analyst engine as the opponent, dialed down via Stockfish's
    /// "Skill Level" option (`skill_level` clamps to 0..=20) with a
    /// wall-clock budget of `movetime_ms` per move.
    ///
    /// This is the mode for embedded-engine platforms (iOS), where the
    /// statically linked Stockfish is a per-process singleton and no second
    /// opponent engine can run — the iOS prototype path until Maia is
    /// linked in-process. [`Self::opponent_reply`] temporarily lowers
    /// `Skill Level` for the opponent search and restores it to 20
    /// afterwards, so `analyze` calls stay full strength.
    pub fn set_opponent_analyst(&mut self, skill_level: u8, movetime_ms: u32) {
        self.opponent = None;
        self.analyst_opponent = Some(AnalystOpponent {
            skill_level: skill_level.min(20),
            movetime_ms,
        });
    }

    /// Analyze `fen`, serving the session's one-entry cache when it already
    /// holds this position (see the `cached_analysis` field for why); fresh
    /// results replace the cache. Callers must only rely on the top line —
    /// a hit may carry fewer PV lines than `multipv` requested.
    async fn analyze_cached(&mut self, fen: &str, multipv: u32) -> Result<Analysis, CoachError> {
        if let Some((cached_fen, analysis)) = &self.cached_analysis {
            if cached_fen == fen {
                self.stats.analysis_cache_hits += 1;
                return Ok(analysis.clone());
            }
        }
        let analysis = self.analyst.analyze(fen, self.analysis_depth, multipv).await?;
        self.cached_analysis = Some((fen.to_string(), analysis.clone()));
        Ok(analysis)
    }

    /// Engine-only judgment of a student move: plays it on the board and
    /// returns the verdict. No LLM involved — this is the cheap step that
    /// runs on *every* move; commentary cadence decides separately whether
    /// the LLM speaks (see [`Self::react_to_student_move`]).
    pub async fn judge_student_move(&mut self, san: &str) -> Result<MoveVerdict, CoachError> {
        self.student_color = self.game.turn();
        // Milestone raw material, captured before the move lands.
        let captures_before = self
            .game
            .history_san()
            .iter()
            .any(|m| m.contains('x'));
        let queens_before = !self
            .game
            .position()
            .board()
            .by_role(shakmaty::Role::Queen)
            .is_empty();

        let before = self.analyze_cached(&self.game.fen(), 3).await?;
        self.game.play_san(san)?;
        let after = self.analyze_cached(&self.game.fen(), 1).await?;

        self.last_milestones = self.detect_milestones(captures_before, queens_before);

        let eval_before = before
            .lines
            .first()
            .map(|l| l.score.as_cp())
            .unwrap_or(0);
        // `after` is from the opponent's perspective — negate.
        let eval_after = -after.lines.first().map(|l| l.score.as_cp()).unwrap_or(0);
        let (cp_loss, judgment) = judge_move(eval_before, eval_after);

        let after_score = after.lines.first().map(|l| l.score);
        // Opponent (side to move now) has forced mate → student allowed it.
        let allows_mate_in = match after_score {
            Some(Score::Mate(n)) if n > 0 => Some(n),
            _ => None,
        };
        // Student had mate available and no longer does.
        let missed_mate_in = match before.lines.first().map(|l| l.score) {
            Some(Score::Mate(n)) if n > 0 => match after_score {
                Some(Score::Mate(m)) if m < 0 => None, // still mating, just slower
                _ => Some(n),
            },
            _ => None,
        };

        let verdict = MoveVerdict {
            played_san: san.to_string(),
            cp_loss,
            judgment,
            best_move_uci: before.best_move.clone(),
            best_line: before
                .lines
                .first()
                .map(|l| l.pv.clone())
                .unwrap_or_default(),
            eval_before_cp: eval_before,
            eval_after_cp: eval_after,
            allows_mate_in,
            missed_mate_in,
        };
        self.accuracy.record(verdict.cp_loss);
        self.eval_history.push(verdict.eval_after_cp);
        if self.eval_history.len() > EVAL_HISTORY_CAP {
            self.eval_history.remove(0);
        }
        self.last_verdict = Some(verdict.clone());
        self.store_record_last_move(None, true, Some(&verdict));
        Ok(verdict)
    }

    /// Milestones the student's just-played move hit (last move in
    /// history). `captures_before`/`queens_before` are snapshots taken
    /// before the move landed.
    fn detect_milestones(&self, captures_before: bool, queens_before: bool) -> Vec<MilestoneKind> {
        let Some(san) = self.game.history_san().last() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if san.starts_with("O-O") {
            out.push(MilestoneKind::Castled);
        }
        if san.contains('x') && !captures_before {
            out.push(MilestoneKind::FirstCapture);
        }
        if san.contains('=') {
            out.push(MilestoneKind::Promotion);
        }
        if self.game.position().is_check() {
            out.push(MilestoneKind::Check);
        }
        let queens_after = !self
            .game
            .position()
            .board()
            .by_role(shakmaty::Role::Queen)
            .is_empty();
        if queens_before && !queens_after {
            out.push(MilestoneKind::QueensOff);
        }
        out
    }

    /// Close out the game: fold the accumulated per-move accuracy into a
    /// [`GameRecord`], persist it on the profile (EMA rating update, win
    /// counter, history), and return the summary. Resets the accumulator so
    /// the session can host another game.
    pub fn finish_game(&mut self, result: GameResult) -> GameSummary {
        let acl = self.accuracy.avg_centipawn_loss();
        let accuracy = self.accuracy.accuracy_percent();
        let est_rating = estimated_rating_from_acl(acl);
        let moves_judged = self.accuracy.moves();

        self.store_try(|s, id| s.finish_game(id, result, acl, accuracy, est_rating, moves_judged));
        // The row is closed: nothing records into it anymore. The next
        // `reset_game` begins a fresh row.
        self.store_game_id = None;

        self.profile.record_game(GameRecord {
            result,
            acl,
            accuracy,
            est_rating,
            bot_level: self.profile.bot_level,
            moves_judged,
        });
        self.accuracy = GameAccuracy::new();

        GameSummary {
            acl,
            accuracy,
            est_rating,
            ready_to_advance: self.profile.ready_to_advance(),
            moves_judged,
        }
    }

    /// Start a fresh game on the same session: swap in a new [`GameState`]
    /// (standard start, or `fen`), clear the coaching transcript and last
    /// verdict, and reset the per-game accuracy accumulator. The engines,
    /// model backend, student profile, and cumulative stats are kept.
    ///
    /// This exists because embedded-engine platforms (iOS) cannot build a
    /// second session per process — the statically linked Stockfish is a
    /// singleton — so "new game" must reuse the session object.
    pub fn reset_game(&mut self, fen: Option<&str>) -> Result<(), crate::game::GameError> {
        self.game = match fen {
            Some(f) => GameState::from_fen(f)?,
            None => GameState::new(),
        };
        self.transcript.clear();
        self.last_verdict = None;
        self.accuracy = GameAccuracy::new();
        self.policy = CommentaryPolicy::new(self.policy.style());
        self.eval_history.clear();
        self.last_milestones.clear();
        self.last_opponent_ctx = None;
        self.cached_analysis = None;
        // Fresh board → fresh store row. The store invariant closes any
        // still-open row (e.g. reset without finish) as aborted.
        self.store_begin_game();
        Ok(())
    }

    /// Ask the model to react to the most recent judged move (LLM + tools).
    pub async fn coach_reaction(&mut self) -> Result<String, CoachError> {
        let played = self
            .last_verdict
            .as_ref()
            .map(|v| v.played_san.clone())
            .unwrap_or_else(|| "their move".into());
        self.transcript.push(Message::user(format!(
            "The student just played {played}. Use your tools to understand the move, then \
             react as their coach."
        )));
        let text = self.run_tool_loop(None).await?;
        let ply = self.game.history_san().len() as u32;
        self.store_try(|s, id| s.record_chat(id, ply, "coach", false, &text));
        self.store_save_transcript();
        Ok(text)
    }

    /// Assemble the deterministic situation report for the current position
    /// (see [`commentary::situation_report`]), reusing the cached analysis
    /// when it matches the current FEN.
    fn build_situation_report(
        &self,
        phase: Phase,
        opening: Option<&crate::game::openings::Opening>,
    ) -> String {
        let fen = self.game.fen();
        let analysis = self
            .cached_analysis
            .as_ref()
            .filter(|(f, _)| *f == fen)
            .map(|(_, a)| a);
        commentary::situation_report(
            &self.game,
            self.student_color,
            phase,
            &self.eval_history,
            analysis,
            opening,
        )
    }

    /// React to the student's most recent judged move, at the cadence the
    /// commentary policy dictates:
    ///
    /// - `Brief` → a deterministic canned line from the expanded pool
    ///   ([`commentary::brief_line`], rotated by move number).
    /// - `Full(focuses)` → the LLM, fed the situation report plus explicit
    ///   focus instructions. With a `NullBackend` (engine-only play) this
    ///   degrades to the same canned line — the policy still runs, nothing
    ///   crashes.
    ///
    /// Records what it said to the store's chat feed either way.
    pub async fn react_to_student_move(&mut self) -> Result<String, CoachError> {
        let Some(verdict) = self.last_verdict.clone() else {
            let text = self.brief_reaction();
            let ply = self.game.history_san().len() as u32;
            self.store_try(|s, id| s.record_chat(id, ply, "coach", false, &text));
            return Ok(text);
        };
        let move_number = (self.game.history_san().len() as u32 + 1) / 2;
        let phase = commentary::detect_phase(self.game.position());
        let opening = openings::lookup(self.game.history_san());
        let decision = self.policy.on_student_move(
            &verdict,
            &self.last_milestones.clone(),
            move_number,
            phase,
            opening.is_some(),
        );

        let text = match decision {
            Decision::Silent | Decision::Brief => commentary::brief_line(&verdict, move_number),
            Decision::Full(focuses) => {
                let report = self.build_situation_report(phase, opening.as_ref());
                let mut msg = format!(
                    "The student just played {}. Here is the deterministic situation \
                     report (engine + board facts — you may cite it directly):\n{report}\n\n\
                     Focus for this reply:\n",
                    verdict.played_san
                );
                for f in &focuses {
                    msg.push_str(&format!("- {}\n", commentary::focus_instruction(f)));
                }
                self.transcript.push(Message::user(msg));
                let fallback = commentary::brief_line(&verdict, move_number);
                let text = self.run_tool_loop(Some(fallback)).await?;
                self.store_save_transcript();
                text
            }
        };
        let ply = self.game.history_san().len() as u32;
        self.store_try(|s, id| s.record_chat(id, ply, "coach", false, &text));
        Ok(text)
    }

    /// React to the opponent's most recent move, if the commentary policy
    /// thinks it deserves a word: threat warnings, phase transitions, and
    /// "how the game is developing" summaries. Returns `Ok(None)` when the
    /// policy stays silent (always, in `Quiet` style). Consumes the pending
    /// opponent context — calling twice after one opponent move yields
    /// `None` the second time.
    ///
    /// Engine-only degradation: with a `NullBackend` the `Full` path falls
    /// back to a canned "keep an eye on…" note built purely from the
    /// deterministic context ([`commentary::engine_only_note`]), so
    /// engine-only players still get threat warnings and summaries.
    pub async fn react_to_opponent_move(&mut self) -> Result<Option<String>, CoachError> {
        let Some(ctx) = self.last_opponent_ctx.take() else {
            return Ok(None);
        };
        let eval_for_student = self
            .cached_analysis
            .as_ref()
            .filter(|(f, _)| *f == self.game.fen())
            .and_then(|(_, a)| a.lines.first())
            .map(|l| l.score.as_cp());
        let fallback = |focuses: &[Focus]| {
            commentary::engine_only_note(focuses, &ctx, eval_for_student)
        };

        let text = match self.policy.on_opponent_move(&ctx) {
            Decision::Silent => return Ok(None),
            Decision::Brief => fallback(&[Focus::ThreatWarning]),
            Decision::Full(focuses) => {
                let phase = ctx.phase;
                let opening = openings::lookup(self.game.history_san());
                let report = self.build_situation_report(phase, opening.as_ref());
                let mut msg = format!(
                    "The opponent just played {}. Here is the deterministic situation \
                     report (engine + board facts — you may cite it directly):\n{report}\n\n\
                     Focus for this reply (address the STUDENT, about their opponent's \
                     move):\n",
                    ctx.opponent_san
                );
                for f in &focuses {
                    msg.push_str(&format!("- {}\n", commentary::focus_instruction(f)));
                }
                self.transcript.push(Message::user(msg));
                let canned = fallback(&focuses);
                let text = self.run_tool_loop(Some(canned)).await?;
                self.store_save_transcript();
                text
            }
        };
        let ply = self.game.history_san().len() as u32;
        self.store_try(|s, id| s.record_chat(id, ply, "coach", false, &text));
        Ok(Some(text))
    }

    /// The full turn: judge, then comment.
    pub async fn comment_on_move(&mut self, san: &str) -> Result<CoachReply, CoachError> {
        let verdict = self.judge_student_move(san).await?;
        let commentary = self.coach_reaction().await?;
        Ok(CoachReply {
            commentary,
            verdict,
        })
    }

    /// Free-form chat ("what's a fork?", "why was that bad?").
    pub async fn chat(&mut self, user_text: &str) -> Result<String, CoachError> {
        let ply = self.game.history_san().len() as u32;
        self.store_try(|s, id| s.record_chat(id, ply, "student", false, user_text));
        self.transcript.push(Message::user(user_text.to_string()));
        let text = self.run_tool_loop(None).await?;
        self.store_try(|s, id| s.record_chat(id, ply, "coach", false, &text));
        self.store_save_transcript();
        Ok(text)
    }

    /// Have the opponent reply to the current position. Returns the move as
    /// `(san, uci)`, or `None` if no opponent is attached or the game is
    /// over.
    ///
    /// Two modes: a dedicated opponent engine ([`Self::set_opponent`], the
    /// Maia path), or opponent-via-analyst ([`Self::set_opponent_analyst`],
    /// the embedded-engine path) — the latter searches with a reduced
    /// "Skill Level" and restores full strength afterwards.
    pub async fn opponent_reply(&mut self) -> Result<Option<(String, String)>, CoachError> {
        if self.game.is_game_over() {
            return Ok(None);
        }
        let fen = self.game.fen();

        if let Some(cfg) = self.analyst_opponent {
            self.analyst
                .set_option("Skill Level", &cfg.skill_level.to_string())
                .await?;
            self.analyst.set_option("MultiPV", "1").await?;
            let search = self.analyst.best_move(&fen, cfg.movetime_ms).await;
            // Restore full strength even if the search failed, so later
            // `analyze` calls are never silently weakened.
            self.analyst.set_option("Skill Level", "20").await?;
            let uci = search?;
            if uci.is_empty() || uci == "(none)" {
                return Ok(None);
            }
            let san = self.game.play_uci(&uci)?;
            self.store_record_last_move(Some(&uci), false, None);
            self.build_opponent_context(&san).await?;
            return Ok(Some((san, uci)));
        }

        let Some(opp) = self.opponent.as_mut() else {
            return Ok(None);
        };
        let uci = opp.engine.best_move_nodes(&fen, opp.nodes).await?;
        if uci.is_empty() || uci == "(none)" {
            return Ok(None);
        }
        let san = self.game.play_uci(&uci)?;
        self.store_record_last_move(Some(&uci), false, None);
        self.build_opponent_context(&san).await?;
        Ok(Some((san, uci)))
    }

    /// After the opponent's move lands: run ONE analysis of the new position
    /// (student to move) and distill it into the [`OpponentContext`] that
    /// [`Self::react_to_opponent_move`] consults. The analysis lands in the
    /// FEN-keyed cache, so the student's next `judge_student_move` reuses it
    /// as its "before" search — the context costs no net extra engine time
    /// whenever a student move follows.
    async fn build_opponent_context(&mut self, opponent_san: &str) -> Result<(), CoachError> {
        if self.game.is_game_over() {
            // Nothing to warn about; the game-over flow takes it from here.
            self.last_opponent_ctx = None;
            return Ok(());
        }
        let fen = self.game.fen();
        let analysis = self.analyze_cached(&fen, 2).await?;

        // Side to move is the student, so the top line's score is already
        // from the student's perspective.
        let top = analysis.lines.first();
        let eval_now = top.map(|l| l.score.as_cp()).unwrap_or(0);
        let prev = self.eval_history.last().copied().unwrap_or(0);
        let eval_swing_cp = eval_now - prev;
        let threatens_mate = matches!(top.map(|l| l.score), Some(Score::Mate(n)) if n < 0);
        // "Wins material": the eval swung against the student and the top
        // line's early moves include captures.
        let wins_material = eval_swing_cp <= -120
            && top
                .map(|l| {
                    commentary::pv_to_san(&self.game, &l.pv, 4)
                        .iter()
                        .any(|s| s.contains('x'))
                })
                .unwrap_or(false);
        let student_name = match self.student_color {
            shakmaty::Color::White => "white",
            shakmaty::Color::Black => "black",
        };
        let motifs_against_student = motifs::detect(self.game.position())
            .into_iter()
            .filter(|m| m.against == student_name)
            .map(|m| m.description)
            .collect();

        self.last_opponent_ctx = Some(OpponentContext {
            opponent_san: opponent_san.to_string(),
            move_number: self.game.position().fullmoves().get(),
            eval_swing_cp,
            threatens_mate,
            wins_material,
            motifs_against_student,
            phase: commentary::detect_phase(self.game.position()),
        });
        Ok(())
    }

    /// Run the model/tool loop until the model produces text. `fallback` is
    /// used when the model returns nothing usable (notably `NullBackend` in
    /// engine-only play): callers pass a canned line matched to what they
    /// asked for, so the coach degrades to something on-topic instead of a
    /// generic verdict line. `None` falls back to [`Self::brief_reaction`].
    async fn run_tool_loop(&mut self, fallback: Option<String>) -> Result<String, CoachError> {
        let caps = self.model.capabilities();
        let tools = if !caps.supports_tools {
            Vec::new()
        } else {
            match caps.tier {
                ModelTier::Full => tools::full_toolset(),
                ModelTier::Compact => tools::compact_toolset(),
            }
        };

        for _ in 0..MAX_TOOL_ITERATIONS {
            let request = CompletionRequest {
                system: prompt::build_system_prompt(caps.tier, self.modality, &self.profile.summary()),
                messages: self.transcript.clone(),
                tools: tools.clone(),
            };
            let response = self.model.complete(&request).await?;
            self.stats.llm_calls += 1;
            self.stats.tool_calls += response.tool_calls.len() as u32;
            self.stats.input_tokens += response.usage.input_tokens;
            self.stats.output_tokens += response.usage.output_tokens;

            if response.tool_calls.is_empty() {
                let text = match response.text {
                    Some(t) if !t.trim().is_empty() => t,
                    _ => fallback.unwrap_or_else(|| self.brief_reaction()),
                };
                self.transcript.push(Message::assistant(text.clone()));
                return Ok(text);
            }

            self.transcript.push(Message::assistant_with_tools(
                response.text,
                response.tool_calls.clone(),
            ));
            for call in &response.tool_calls {
                let result = self.dispatch_tool(call).await;
                let payload = match result {
                    Ok(v) => v.to_string(),
                    Err(e) => json!({ "error": e.to_string() }).to_string(),
                };
                self.transcript.push(Message::tool_result(call.id.clone(), payload));
            }
        }
        Err(CoachError::ToolLoopExceeded(MAX_TOOL_ITERATIONS))
    }

    async fn dispatch_tool(&mut self, call: &ToolCall) -> Result<Value, CoachError> {
        match call.name.as_str() {
            "evaluate_position" => {
                let multipv = call.arguments["multipv"].as_u64().unwrap_or(3).clamp(1, 5) as u32;
                let depth = call.arguments["depth"]
                    .as_u64()
                    .unwrap_or(self.analysis_depth as u64)
                    .clamp(8, 22) as u32;
                let analysis = self.analyst.analyze(&self.game.fen(), depth, multipv).await?;
                Ok(json!({
                    "side_to_move": format!("{:?}", self.game.turn()),
                    "best_move": analysis.best_move,
                    "lines": analysis.lines,
                }))
            }
            "classify_last_move" => Ok(match &self.last_verdict {
                Some(v) => json!(v),
                None => json!({ "error": "no move has been played yet" }),
            }),
            "lookup_opening" => Ok(json!(openings::lookup(self.game.history_san()))),
            "detect_motifs" => Ok(json!(motifs::detect(self.game.position()))),
            "get_student_profile" => Ok(json!(self.profile)),
            "update_student_profile" => {
                let concept = call.arguments["concept"].as_str().unwrap_or("unknown");
                let status = match call.arguments["status"].as_str() {
                    Some("demonstrated") => ConceptStatus::Demonstrated,
                    Some("mastered") => ConceptStatus::Mastered,
                    _ => ConceptStatus::Taught,
                };
                let note = call.arguments["note"].as_str().map(str::to_string);
                self.profile.touch_concept(concept, status, note);
                Ok(json!({ "ok": true }))
            }
            unknown => Ok(json!({ "error": format!("unknown tool '{unknown}'") })),
        }
    }

    /// Canned commentary from the engine verdict alone — the quiet-move
    /// cadence path, and the fallback when a weak BYOLLM backend returns
    /// nothing usable. The coach degrades, never crashes.
    pub fn brief_reaction(&self) -> String {
        match &self.last_verdict {
            Some(v) => match v.judgment {
                Judgment::Best | Judgment::Excellent => {
                    format!("{} — nice, that's right in line with the engine's choice.", v.played_san)
                }
                Judgment::Good => format!("{} is a solid move.", v.played_san),
                Judgment::Inaccuracy => format!(
                    "{} is playable, but there was something a bit stronger here.",
                    v.played_san
                ),
                Judgment::Mistake | Judgment::Blunder => {
                    if v.allows_mate_in.is_some() {
                        format!(
                            "{} is dangerous — it gives your opponent a forced checkmate. \
                             Look at the squares around your king.",
                            v.played_san
                        )
                    } else if v.missed_mate_in.is_some() {
                        format!(
                            "{} lets a big chance slip — you had a forced win here. \
                             Look for the most forcing moves.",
                            v.played_san
                        )
                    } else {
                        format!(
                            "{} runs into trouble — take another look at this position.",
                            v.played_san
                        )
                    }
                }
            },
            None => "Let's keep going — your move.".to_string(),
        }
    }
}

#[cfg(test)]
mod session_tests {
    use super::*;
    use crate::engine::ChannelTransport;
    use crate::llm::{
        Capabilities, CompletionRequest, CompletionResponse, LlmError, ModelTier, NullBackend,
    };
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Mutex;
    use std::time::Duration;
    use tokio::time::timeout;

    const TEST_TIMEOUT: Duration = Duration::from_secs(5);

    /// Test double for the LLM: records the last [`CompletionRequest`] it
    /// received and returns fixed text. `supports_tools: false` keeps the
    /// tool loop to a single iteration.
    struct CapturingModel {
        calls: AtomicU32,
        last_request: Mutex<Option<CompletionRequest>>,
    }

    impl CapturingModel {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicU32::new(0),
                last_request: Mutex::new(None),
            })
        }
    }

    #[async_trait]
    impl CoachModel for CapturingModel {
        fn capabilities(&self) -> Capabilities {
            Capabilities {
                supports_tools: false,
                context_tokens: 8192,
                tier: ModelTier::Compact,
            }
        }

        async fn complete(&self, req: &CompletionRequest) -> Result<CompletionResponse, LlmError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            *self.last_request.lock().unwrap() = Some(req.clone());
            Ok(CompletionResponse {
                text: Some("coached!".into()),
                tool_calls: vec![],
                usage: Default::default(),
            })
        }
    }

    /// Scripted fake engine over a [`ChannelTransport`] (same pattern as the
    /// `engine::uci` transport tests): each `go` pops the next canned reply
    /// off the script, so the test controls every analysis result in order.
    fn scripted_engine(script: Vec<Vec<&'static str>>) -> ChannelTransport {
        let (transport, engine_out, mut engine_in) = ChannelTransport::new(64);
        tokio::spawn(async move {
            let mut script = script.into_iter();
            while let Some(cmd) = engine_in.recv().await {
                match cmd.split_whitespace().next().unwrap_or("") {
                    "uci" => {
                        engine_out.send("id name ScriptFish".into()).await.ok();
                        engine_out.send("uciok".into()).await.ok();
                    }
                    "isready" => {
                        engine_out.send("readyok".into()).await.ok();
                    }
                    "go" => {
                        let reply = script.next().expect("scripted engine ran out of replies");
                        for line in reply {
                            engine_out.send(line.to_string()).await.ok();
                        }
                    }
                    _ => {}
                }
            }
        });
        transport
    }

    async fn session_with(
        model: Arc<dyn CoachModel>,
        script: Vec<Vec<&'static str>>,
    ) -> CoachSession {
        let analyst = UciEngine::from_transport(Box::new(scripted_engine(script)))
            .await
            .expect("handshake");
        CoachSession::new(
            model,
            analyst,
            GameState::new(),
            crate::student::StudentProfile::default(),
            Modality::Text,
        )
    }

    /// Script for judging 1. e4: a "before" analysis of the start position
    /// and an "after" analysis from Black's perspective (cp -20 → +20 for
    /// the student → tiny loss → good-class move).
    fn e4_script() -> Vec<Vec<&'static str>> {
        vec![
            vec![
                "info depth 12 multipv 1 score cp 30 pv e2e4 e7e5 g1f3",
                "bestmove e2e4",
            ],
            vec![
                "info depth 12 multipv 1 score cp -20 pv e7e5 g1f3 b8c6",
                "bestmove e7e5",
            ],
        ]
    }

    #[tokio::test]
    async fn chatty_good_move_takes_full_path_with_report_and_focus() {
        let model = CapturingModel::new();
        let mut session = session_with(model.clone(), e4_script()).await;
        session.set_commentary_style(CommentaryStyle::Chatty);

        let verdict = timeout(TEST_TIMEOUT, session.judge_student_move("e4"))
            .await
            .expect("judge timed out")
            .unwrap();
        assert!(!verdict.judgment.is_notable(), "e4 must judge as good-class");

        let text = timeout(TEST_TIMEOUT, session.react_to_student_move())
            .await
            .expect("react timed out")
            .unwrap();
        assert_eq!(text, "coached!");
        assert_eq!(model.calls.load(Ordering::SeqCst), 1, "full path calls the model once");

        let req = model.last_request.lock().unwrap().clone().expect("request captured");
        let user_msg = &req.messages.last().expect("has messages").content;
        // Situation-report labels…
        for label in ["Move:", "Material:", "Eval:", "Threats:"] {
            assert!(user_msg.contains(label), "missing {label} in:\n{user_msg}");
        }
        // …and the focus instructions for a good move in Chatty style.
        assert!(
            user_msg.contains("Acknowledge what the move accomplishes"),
            "missing Encourage instruction in:\n{user_msg}"
        );
        assert!(
            user_msg.contains("WHY the move is strong"),
            "missing ExplainWhyGood instruction in:\n{user_msg}"
        );
    }

    #[tokio::test]
    async fn quiet_good_move_is_brief_and_never_calls_the_model() {
        let model = CapturingModel::new();
        let mut session = session_with(model.clone(), e4_script()).await;
        session.set_commentary_style(CommentaryStyle::Quiet);

        let verdict = timeout(TEST_TIMEOUT, session.judge_student_move("e4"))
            .await
            .expect("judge timed out")
            .unwrap();

        let text = timeout(TEST_TIMEOUT, session.react_to_student_move())
            .await
            .expect("react timed out")
            .unwrap();
        assert_eq!(text, commentary::brief_line(&verdict, 1));
        assert_eq!(model.calls.load(Ordering::SeqCst), 0, "quiet brief path must not call the model");
    }

    #[tokio::test]
    async fn null_backend_full_path_degrades_to_canned_line() {
        let mut session = session_with(Arc::new(NullBackend), e4_script()).await;
        session.set_commentary_style(CommentaryStyle::Chatty);

        let verdict = timeout(TEST_TIMEOUT, session.judge_student_move("e4"))
            .await
            .expect("judge timed out")
            .unwrap();
        let text = timeout(TEST_TIMEOUT, session.react_to_student_move())
            .await
            .expect("react timed out")
            .unwrap();
        // Full decision + NullBackend → the expanded canned line, not a crash.
        assert_eq!(text, commentary::brief_line(&verdict, 1));
    }

    #[tokio::test]
    async fn opponent_reply_analysis_is_reused_by_next_judge() {
        // Script order: judge e4 (before, after), opponent movetime search,
        // opponent-context analysis, then judge Nf3 (before = CACHE HIT, so
        // only the "after" search runs).
        let script = vec![
            vec![
                "info depth 12 multipv 1 score cp 30 pv e2e4 e7e5 g1f3",
                "bestmove e2e4",
            ],
            vec![
                "info depth 12 multipv 1 score cp -20 pv e7e5 g1f3 b8c6",
                "bestmove e7e5",
            ],
            // Opponent (analyst-as-opponent) picks e5.
            vec!["bestmove e7e5"],
            // One analysis of the position after 1. e4 e5 → OpponentContext.
            vec![
                "info depth 12 multipv 1 score cp 25 pv g1f3 b8c6 f1b5",
                "info depth 12 multipv 2 score cp 10 pv f1c4 g8f6 d2d3",
                "bestmove g1f3",
            ],
            // Judge Nf3: "after" analysis only (before is served from cache).
            vec![
                "info depth 12 multipv 1 score cp -15 pv b8c6 f1b5 a7a6",
                "bestmove b8c6",
            ],
        ];
        let mut session = session_with(Arc::new(NullBackend), script).await;
        session.set_opponent_analyst(5, 50);

        timeout(TEST_TIMEOUT, session.judge_student_move("e4"))
            .await
            .expect("judge e4 timed out")
            .unwrap();
        assert_eq!(session.stats.analysis_cache_hits, 0);

        let reply = timeout(TEST_TIMEOUT, session.opponent_reply())
            .await
            .expect("opponent timed out")
            .unwrap();
        assert_eq!(reply, Some(("e5".to_string(), "e7e5".to_string())));

        timeout(TEST_TIMEOUT, session.judge_student_move("Nf3"))
            .await
            .expect("judge Nf3 timed out")
            .unwrap();
        assert_eq!(
            session.stats.analysis_cache_hits, 1,
            "the before-analysis of Nf3 must reuse the opponent-context analysis"
        );
    }

    #[tokio::test]
    async fn quiet_opponent_move_reaction_is_none() {
        let script = vec![
            vec![
                "info depth 12 multipv 1 score cp 30 pv e2e4 e7e5 g1f3",
                "bestmove e2e4",
            ],
            vec![
                "info depth 12 multipv 1 score cp -20 pv e7e5 g1f3 b8c6",
                "bestmove e7e5",
            ],
            vec!["bestmove e7e5"],
            vec![
                "info depth 12 multipv 1 score cp 25 pv g1f3 b8c6 f1b5",
                "bestmove g1f3",
            ],
        ];
        let model = CapturingModel::new();
        let mut session = session_with(model.clone(), script).await;
        session.set_commentary_style(CommentaryStyle::Quiet);
        session.set_opponent_analyst(5, 50);

        timeout(TEST_TIMEOUT, session.judge_student_move("e4"))
            .await
            .expect("judge timed out")
            .unwrap();
        timeout(TEST_TIMEOUT, session.opponent_reply())
            .await
            .expect("opponent timed out")
            .unwrap();
        let reaction = timeout(TEST_TIMEOUT, session.react_to_opponent_move())
            .await
            .expect("react timed out")
            .unwrap();
        assert_eq!(reaction, None);
        assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn milestone_detection_from_san_and_board() {
        let mut session_game = GameState::new();
        for m in ["e4", "e5", "Nf3", "Nc6", "Bb5", "a6", "Bxc6"] {
            session_game.play_san(m).unwrap();
        }
        // Direct check of the pure pieces: last move was the first capture.
        assert!(session_game.history_san().last().unwrap().contains('x'));
        let earlier_capture = session_game.history_san()[..6].iter().any(|m| m.contains('x'));
        assert!(!earlier_capture);
    }
}
