//! The exported session object: a blocking, thread-safe wrapper around
//! `coach_core::coach::CoachSession`.

use crate::foreign::{ForeignCoachModel, ForeignModelAdapter};
use crate::{FfiError, RUNTIME};
use coach_core::coach::{CoachSession, CommentaryStyle, Modality};
use coach_core::engine::UciEngine;
use coach_core::game::GameState;
use coach_core::llm::{build_backend, BackendConfig, CoachModel, NullBackend};
use coach_core::store::GameStore;
use coach_core::student::{GameResult, StudentProfile};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

/// How the game ended, from the student's point of view. Mirrors
/// coach-core's `GameResult`.
#[derive(Debug, Clone, Copy, uniffi::Enum)]
pub enum FfiGameResult {
    Win,
    Loss,
    Draw,
    Aborted,
}

impl From<FfiGameResult> for GameResult {
    fn from(r: FfiGameResult) -> Self {
        match r {
            FfiGameResult::Win => GameResult::Win,
            FfiGameResult::Loss => GameResult::Loss,
            FfiGameResult::Draw => GameResult::Draw,
            FfiGameResult::Aborted => GameResult::Aborted,
        }
    }
}

/// How much the coach talks. Mirrors coach-core's `CommentaryStyle`.
#[derive(Debug, Clone, Copy, uniffi::Enum)]
pub enum FfiCommentaryStyle {
    /// LLM only on notable moves (inaccuracy/mistake/blunder), canned lines
    /// otherwise, no opponent or game-arc commentary.
    Quiet,
    /// Shift-triggered: silent until the game significantly shifts (eval
    /// drift, mistake/blunder, forced mate), then one recap of the stretch
    /// of moves that led to the swing.
    Balanced,
    /// Full reaction to every student move; opponent commentary whenever
    /// anything is worth flagging; frequent development summaries.
    Chatty,
}

impl From<FfiCommentaryStyle> for CommentaryStyle {
    fn from(s: FfiCommentaryStyle) -> Self {
        match s {
            FfiCommentaryStyle::Quiet => CommentaryStyle::Quiet,
            FfiCommentaryStyle::Balanced => CommentaryStyle::Balanced,
            FfiCommentaryStyle::Chatty => CommentaryStyle::Chatty,
        }
    }
}

/// One coaching session, exported to Swift/Kotlin.
///
/// All methods BLOCK the calling thread (the crate runs an internal tokio
/// runtime) — call from a background thread/queue, never the UI thread. The
/// object is `Send + Sync`; methods serialize on an internal mutex, so
/// concurrent calls from multiple threads are safe but run one at a time.
#[derive(uniffi::Object)]
pub struct CoachSessionHandle {
    inner: Mutex<CoachSession>,
}

/// Shared constructor plumbing: assemble the session around an
/// already-connected analyst engine and the chosen model backend.
fn assemble_session(
    model: Arc<dyn CoachModel>,
    analyst: UciEngine,
    fen: Option<String>,
    profile_json: Option<String>,
    voice: bool,
) -> Result<CoachSession, FfiError> {
    let game = match fen {
        Some(f) => GameState::from_fen(&f)?,
        None => GameState::new(),
    };
    let profile: StudentProfile = match profile_json {
        Some(j) => serde_json::from_str(&j)?,
        None => StudentProfile::default(),
    };
    let modality = if voice { Modality::Voice } else { Modality::Text };
    Ok(CoachSession::new(model, analyst, game, profile, modality))
}

/// Constructor plumbing for the spawned-engine path (desktop/server).
fn build_session(
    model: Arc<dyn CoachModel>,
    engine_path: &str,
    fen: Option<String>,
    profile_json: Option<String>,
    voice: bool,
) -> Result<CoachSession, FfiError> {
    let analyst = RUNTIME.block_on(UciEngine::spawn(engine_path))?;
    assemble_session(model, analyst, fen, profile_json, voice)
}

/// Constructor plumbing for the embedded-engine path (iOS: no subprocess
/// spawning). The statically linked Stockfish 11 is a per-process
/// singleton; a second embedded session errors.
#[cfg(feature = "embedded-stockfish")]
fn build_session_embedded(
    model: Arc<dyn CoachModel>,
    fen: Option<String>,
    profile_json: Option<String>,
    voice: bool,
) -> Result<CoachSession, FfiError> {
    let analyst = RUNTIME.block_on(async {
        let transport = stockfish_embedded::embedded_stockfish_transport().await?;
        UciEngine::from_transport(Box::new(transport)).await
    })?;
    assemble_session(model, analyst, fen, profile_json, voice)
}

impl CoachSessionHandle {
    fn wrap(session: CoachSession) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(session),
        })
    }

    /// A poisoned mutex only means a previous call panicked mid-operation;
    /// the session state is still the best we have — recover it.
    fn lock(&self) -> MutexGuard<'_, CoachSession> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[uniffi::export]
impl CoachSessionHandle {
    /// Session backed by any OpenAI-compatible HTTP endpoint (BYOLLM:
    /// OpenAI, OpenRouter, Groq, Ollama, LM Studio, …).
    #[uniffi::constructor]
    pub fn new_with_openai(
        base_url: String,
        api_key: Option<String>,
        model: String,
        engine_path: String,
        fen: Option<String>,
        profile_json: Option<String>,
        voice: bool,
    ) -> Result<Arc<Self>, FfiError> {
        let backend = build_backend(BackendConfig::OpenAiCompat {
            base_url,
            api_key,
            model,
            capabilities: None,
        });
        Ok(Self::wrap(build_session(
            backend,
            &engine_path,
            fen,
            profile_json,
            voice,
        )?))
    }

    /// Session backed by the native Anthropic API (paid tier).
    #[uniffi::constructor]
    pub fn new_with_anthropic(
        api_key: String,
        model: String,
        engine_path: String,
        fen: Option<String>,
        profile_json: Option<String>,
        voice: bool,
    ) -> Result<Arc<Self>, FfiError> {
        let backend = build_backend(BackendConfig::Anthropic {
            api_key,
            model,
            base_url: None,
        });
        Ok(Self::wrap(build_session(
            backend,
            &engine_path,
            fen,
            profile_json,
            voice,
        )?))
    }

    /// Engine-only session: no LLM, commentary falls back to templated
    /// engine verdicts (`brief_reaction`).
    #[uniffi::constructor]
    pub fn new_engine_only(
        engine_path: String,
        fen: Option<String>,
        profile_json: Option<String>,
        voice: bool,
    ) -> Result<Arc<Self>, FfiError> {
        Ok(Self::wrap(build_session(
            Arc::new(NullBackend),
            &engine_path,
            fen,
            profile_json,
            voice,
        )?))
    }

    /// Session backed by a foreign-implemented model (Apple Foundation
    /// Models on iOS, Gemini Nano on Android).
    #[uniffi::constructor]
    pub fn new_with_foreign_model(
        model: Arc<dyn ForeignCoachModel>,
        engine_path: String,
        fen: Option<String>,
        profile_json: Option<String>,
        voice: bool,
    ) -> Result<Arc<Self>, FfiError> {
        let adapter: Arc<dyn CoachModel> = Arc::new(ForeignModelAdapter { inner: model });
        Ok(Self::wrap(build_session(
            adapter,
            &engine_path,
            fen,
            profile_json,
            voice,
        )?))
    }

    /// Attach the opponent engine (Maia via lc0 needs
    /// `--weights=<file>` in `args`; `nodes = 1` for authentic Maia play).
    pub fn attach_opponent(
        &self,
        engine_path: String,
        args: Vec<String>,
        nodes: u64,
    ) -> Result<(), FfiError> {
        let engine = RUNTIME.block_on(UciEngine::spawn_with_args(&engine_path, &args))?;
        self.lock().set_opponent(engine, nodes);
        Ok(())
    }

    /// Use the analyst engine as the opponent, dialed down via Stockfish's
    /// "Skill Level" option (0..=20) with `movetime_ms` per move. The mode
    /// for embedded-engine platforms (iOS), where the statically linked
    /// engine is a per-process singleton and no second opponent engine can
    /// run — the prototype path until Maia is linked in-process. Analysis
    /// stays full strength: skill is restored to 20 after each opponent
    /// move.
    pub fn attach_opponent_analyst(&self, skill_level: u8, movetime_ms: u32) {
        self.lock().set_opponent_analyst(skill_level, movetime_ms);
    }

    /// Swap the coach's LLM backend at runtime to any OpenAI-compatible
    /// HTTP endpoint (BYOLLM: DeepSeek, OpenAI, OpenRouter, Groq, Ollama,
    /// LM Studio, …). Keeps the engines, game, and student profile; clears
    /// the conversation transcript (a new coach must not inherit another
    /// model's tool-call history). This is how backend switching works on
    /// embedded-engine platforms (iOS), where the engine singleton forbids
    /// constructing a second session.
    pub fn set_backend_openai(&self, base_url: String, api_key: Option<String>, model: String) {
        let backend = build_backend(BackendConfig::OpenAiCompat {
            base_url,
            api_key,
            model,
            capabilities: None,
        });
        self.lock().set_model(backend);
    }

    /// Swap the coach's LLM backend at runtime to the native Anthropic API.
    /// Same semantics as [`Self::set_backend_openai`].
    pub fn set_backend_anthropic(&self, api_key: String, model: String) {
        let backend = build_backend(BackendConfig::Anthropic {
            api_key,
            model,
            base_url: None,
        });
        self.lock().set_model(backend);
    }

    /// Swap the coach's LLM backend at runtime to a foreign-implemented
    /// model (Apple Foundation Models on iOS, Gemini Nano on Android).
    /// Same semantics as [`Self::set_backend_openai`].
    pub fn set_backend_foreign(&self, model: Arc<dyn ForeignCoachModel>) {
        let adapter: Arc<dyn CoachModel> = Arc::new(ForeignModelAdapter { inner: model });
        self.lock().set_model(adapter);
    }

    /// Swap the coach's LLM backend at runtime to no LLM at all: commentary
    /// falls back to templated engine verdicts (`brief_reaction`). Same
    /// semantics as [`Self::set_backend_openai`].
    pub fn set_backend_engine_only(&self) {
        self.lock().set_model(Arc::new(NullBackend));
    }

    /// Start a fresh game in place: new board (standard start, or `fen`),
    /// cleared coaching transcript, reset per-game accuracy accumulator.
    /// Keeps the engines, model backend, and student profile. Use this for
    /// "new game" instead of constructing a second session — the embedded
    /// engine is a per-process singleton, so a second embedded session
    /// cannot be created.
    pub fn reset_game(&self, fen: Option<String>) -> Result<(), FfiError> {
        Ok(self.lock().reset_game(fen.as_deref())?)
    }

    /// Engine-only judgment of a student move (no LLM). Plays the move and
    /// returns the verdict as JSON (`MoveVerdict` serialization: cp_loss,
    /// judgment, best_move_uci, best_line, allows_mate_in, …).
    pub fn judge_move(&self, san: String) -> Result<String, FfiError> {
        let mut session = self.lock();
        let verdict = RUNTIME.block_on(session.judge_student_move(&san))?;
        Ok(serde_json::to_string(&verdict)?)
    }

    /// Ask the model to react to the most recent judged move (LLM + tools).
    pub fn coach_reaction(&self) -> Result<String, FfiError> {
        let mut session = self.lock();
        Ok(RUNTIME.block_on(session.coach_reaction())?)
    }

    /// Free-form chat with the coach ("what's a fork?").
    pub fn chat(&self, text: String) -> Result<String, FfiError> {
        let mut session = self.lock();
        Ok(RUNTIME.block_on(session.chat(&text))?)
    }

    /// Have the opponent engine reply to the current position. Returns the
    /// move in SAN, or `None` if no opponent is attached or the game is over.
    pub fn opponent_reply(&self) -> Result<Option<String>, FfiError> {
        let mut session = self.lock();
        let reply = RUNTIME.block_on(session.opponent_reply())?;
        Ok(reply.map(|(san, _uci)| san))
    }

    /// Canned engine-verdict commentary — the quiet-move cadence path.
    pub fn brief_reaction(&self) -> String {
        self.lock().brief_reaction()
    }

    /// Set how much the coach talks (see [`FfiCommentaryStyle`]). Takes
    /// effect from the next reaction.
    pub fn set_commentary_style(&self, style: FfiCommentaryStyle) {
        self.lock().set_commentary_style(style.into());
    }

    /// React to the student's most recent judged move at the cadence the
    /// commentary policy dictates: `None` when the policy stays silent
    /// (Balanced, while the game holds steady), a canned line for quiet
    /// moves, the full LLM reaction (situation report + focus instructions)
    /// or a shift recap otherwise. Auto-records to the store's chat feed
    /// whenever it speaks; do not `log_feed` the result again.
    pub fn react_to_student_move(&self) -> Result<Option<String>, FfiError> {
        let mut session = self.lock();
        Ok(RUNTIME.block_on(session.react_to_student_move())?)
    }

    /// React to the opponent's most recent move if the commentary policy
    /// thinks it deserves a word (threat warnings, phase transitions,
    /// development summaries). Returns `None` when the policy stays silent
    /// (always, in Quiet style). Auto-records to the store's chat feed when
    /// it speaks — do not `log_feed` the result again.
    pub fn react_to_opponent_move(&self) -> Result<Option<String>, FfiError> {
        let mut session = self.lock();
        Ok(RUNTIME.block_on(session.react_to_opponent_move())?)
    }

    /// Declare whether the current backend can narrate a supplied set of
    /// facts. Set it false for stub/canned backends: the catch-up recap and
    /// the game review then use the engine's own deterministic wording,
    /// which is specific about the actual game, instead of the generic
    /// advice a stub returns. Re-apply it after every backend switch.
    pub fn set_narration(&self, enabled: bool) {
        self.lock().set_narration(enabled);
    }

    /// Catch the student up after the coach was paused: one message
    /// covering everything from `from_ply` half-moves to now. `from_ply` is
    /// the move count when the coach last spoke, so the window is the moves
    /// after that. Returns a short "you're up to date" line without calling
    /// the model when no move was played in between. Auto-records to the
    /// store's chat feed — do not `log_feed` the result again.
    pub fn recap_since(&self, from_ply: u32) -> Result<String, FfiError> {
        let mut session = self.lock();
        Ok(RUNTIME.block_on(session.recap_since(from_ply))?)
    }

    /// Whole-game review of the game on the board, as JSON (`GameReview`:
    /// headline, summary, strengths, weaknesses, accuracy, and the
    /// engine-anchored `moments`, each carrying the ply, both FENs, evals,
    /// the engine's preferred line, and the coach's note).
    ///
    /// SLOW: this sweeps every position at scan depth and re-searches the
    /// hotspots at full depth, so it runs for seconds to tens of seconds on
    /// a long game. Call it off the main thread and show progress.
    pub fn review_game(&self) -> Result<String, FfiError> {
        let mut session = self.lock();
        let review = RUNTIME.block_on(session.review_current_game())?;
        Ok(serde_json::to_string(&review)?)
    }

    /// Same review, for a game that is no longer on the board — pass the
    /// SAN move list (and the starting FEN, if it was not the standard
    /// start) straight out of the store. The live game is untouched, so a
    /// student can review last week's game mid-game.
    ///
    /// SLOW, exactly as [`Self::review_game`].
    pub fn review_moves(
        &self,
        moves: Vec<String>,
        starting_fen: Option<String>,
        student_is_white: bool,
    ) -> Result<String, FfiError> {
        let student = if student_is_white {
            coach_core::Color::White
        } else {
            coach_core::Color::Black
        };
        let mut session = self.lock();
        let review = RUNTIME.block_on(session.review_moves(
            &moves,
            starting_fen.as_deref(),
            student,
            coach_core::coach::review::DEFAULT_MAX_MOMENTS,
        ))?;
        Ok(serde_json::to_string(&review)?)
    }

    /// Close out the game: update the student profile (rating EMA, win
    /// counter, history) and return the `GameSummary` as JSON (acl,
    /// accuracy, est_rating, ready_to_advance, moves_judged).
    pub fn finish_game(&self, result: FfiGameResult) -> String {
        let summary = self.lock().finish_game(result.into());
        serde_json::to_string(&summary).expect("GameSummary serializes")
    }

    /// Current position, FEN.
    pub fn fen(&self) -> String {
        self.lock().game.fen()
    }

    /// Unicode board diagram (rank 8 at the top).
    pub fn board_diagram(&self) -> String {
        self.lock().game.board_diagram()
    }

    pub fn is_game_over(&self) -> bool {
        self.lock().game.is_game_over()
    }

    /// Move history in SAN, first move first.
    pub fn history_san(&self) -> Vec<String> {
        self.lock().game.history_san().to_vec()
    }

    /// Captured pieces per side plus the on-board material balance, as
    /// JSON — same shape as [`crate::board::BoardHandle::material_summary`].
    pub fn material_summary(&self) -> String {
        serde_json::to_string(&self.lock().game.material_summary())
            .expect("MaterialSummary serializes")
    }

    /// The student profile as JSON — persist this between sessions and pass
    /// it back via the constructors' `profile_json`.
    pub fn profile_json(&self) -> String {
        serde_json::to_string(&self.lock().profile).expect("StudentProfile serializes")
    }

    /// Cumulative session cost counters as JSON (`SessionStats`).
    pub fn stats_json(&self) -> String {
        serde_json::to_string(&self.lock().stats).expect("SessionStats serializes")
    }

    /// Attach SQLite persistence at `db_path` for the life of the session
    /// (schema is created on first open; WAL mode). If the current game has
    /// no moves yet a game row is begun immediately; from then on every
    /// move, chat message, and LLM transcript auto-records. A failed store
    /// write never fails the chess operation (see `record_failures` in
    /// `stats_json`).
    pub fn attach_store(&self, db_path: String) -> Result<(), FfiError> {
        let store = GameStore::open(Path::new(&db_path))?;
        Ok(self.lock().attach_store(store)?)
    }

    /// Resume the in-progress game stored at `db_path`, if any, onto this
    /// session, and keep the store attached for further auto-recording.
    ///
    /// Returns the `ResumeReport` as JSON (`game_id`, `fen`, `history_san`,
    /// `chat`, `bot_level`) when a game was resumed, or `None` when there
    /// was no open game (the store is then attached exactly like
    /// [`Self::attach_store`]). A corrupt stored game is closed as aborted
    /// and treated as "no open game" — this never errors over stale data.
    pub fn resume_from_store(&self, db_path: String) -> Result<Option<String>, FfiError> {
        let store = GameStore::open(Path::new(&db_path))?;
        let report = self.lock().resume_from_store(store)?;
        report
            .map(|r| serde_json::to_string(&r).map_err(FfiError::from))
            .transpose()
    }

    /// Persist a message the UI generated locally (canned greeting, backend
    /// switch notice) into the current game's chat feed. `role` should be
    /// one of student/coach/system. Best-effort no-op without an attached
    /// store.
    pub fn log_feed(&self, role: String, is_review: bool, text: String) {
        self.lock().log_feed(&role, is_review, &text);
    }
}

/// Finished games at `db_path`, newest first, as a JSON array of game rows
/// (id, started_at, ended_at, result, bot_level, opponent_kind, eco,
/// opening, acl, accuracy, est_rating, moves_judged, move_count).
///
/// A free function (not a session method) so the History screen works
/// without touching the live session: it opens a short-lived read-only
/// connection per call — SQLite WAL lets this read run concurrently with
/// the session's write connection.
#[uniffi::export]
pub fn history_list(db_path: String, limit: u32, offset: u32) -> Result<String, FfiError> {
    let store = GameStore::open_read_only(Path::new(&db_path))?;
    Ok(serde_json::to_string(&store.list_games(limit, offset)?)?)
}

/// One game's full record at `db_path` as JSON (`{game, moves, chat}`), or
/// `None` if no such game id. Free function; see [`history_list`] for the
/// concurrency story.
#[uniffi::export]
pub fn game_detail(db_path: String, game_id: i64) -> Result<Option<String>, FfiError> {
    let store = GameStore::open_read_only(Path::new(&db_path))?;
    store
        .game_detail(game_id)?
        .map(|d| serde_json::to_string(&d).map_err(FfiError::from))
        .transpose()
}

/// Store-wide aggregates at `db_path` as JSON (games, wins, losses, draws,
/// aborted, best_accuracy, current_bot_level). Free function; see
/// [`history_list`] for the concurrency story.
#[uniffi::export]
pub fn store_stats(db_path: String) -> Result<String, FfiError> {
    let store = GameStore::open_read_only(Path::new(&db_path))?;
    Ok(serde_json::to_string(&store.stats()?)?)
}

/// Constructors backed by the statically linked Stockfish 11 analyst — the
/// iOS path, where spawning an engine process is forbidden. Same signatures
/// as the spawned-engine constructors minus `engine_path`.
///
/// The embedded engine is a per-process SINGLETON: exactly one of these
/// constructors may succeed per process lifetime; further calls (or a call
/// after any prior embedded start) return an engine error.
#[cfg(feature = "embedded-stockfish")]
#[uniffi::export]
impl CoachSessionHandle {
    /// Embedded-engine session backed by any OpenAI-compatible HTTP
    /// endpoint (BYOLLM).
    #[uniffi::constructor]
    pub fn new_with_openai_embedded(
        base_url: String,
        api_key: Option<String>,
        model: String,
        fen: Option<String>,
        profile_json: Option<String>,
        voice: bool,
    ) -> Result<Arc<Self>, FfiError> {
        let backend = build_backend(BackendConfig::OpenAiCompat {
            base_url,
            api_key,
            model,
            capabilities: None,
        });
        Ok(Self::wrap(build_session_embedded(
            backend,
            fen,
            profile_json,
            voice,
        )?))
    }

    /// Embedded-engine session backed by the native Anthropic API (paid
    /// tier).
    #[uniffi::constructor]
    pub fn new_with_anthropic_embedded(
        api_key: String,
        model: String,
        fen: Option<String>,
        profile_json: Option<String>,
        voice: bool,
    ) -> Result<Arc<Self>, FfiError> {
        let backend = build_backend(BackendConfig::Anthropic {
            api_key,
            model,
            base_url: None,
        });
        Ok(Self::wrap(build_session_embedded(
            backend,
            fen,
            profile_json,
            voice,
        )?))
    }

    /// Embedded-engine session with no LLM: commentary falls back to
    /// templated engine verdicts (`brief_reaction`).
    #[uniffi::constructor]
    pub fn new_engine_only_embedded(
        fen: Option<String>,
        profile_json: Option<String>,
        voice: bool,
    ) -> Result<Arc<Self>, FfiError> {
        Ok(Self::wrap(build_session_embedded(
            Arc::new(NullBackend),
            fen,
            profile_json,
            voice,
        )?))
    }

    /// Embedded-engine session backed by a foreign-implemented model
    /// (Apple Foundation Models on iOS, Gemini Nano on Android).
    #[uniffi::constructor]
    pub fn new_with_foreign_model_embedded(
        model: Arc<dyn ForeignCoachModel>,
        fen: Option<String>,
        profile_json: Option<String>,
        voice: bool,
    ) -> Result<Arc<Self>, FfiError> {
        let adapter: Arc<dyn CoachModel> = Arc::new(ForeignModelAdapter { inner: model });
        Ok(Self::wrap(build_session_embedded(
            adapter,
            fen,
            profile_json,
            voice,
        )?))
    }
}
