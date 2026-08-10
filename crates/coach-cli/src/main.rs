//! Headless driver for coach-core: prove the engine loop and the coach loop
//! before any UI exists, run scripted evals across backends, and play full
//! games against the Maia opponent in the terminal.

mod eval;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use coach_core::coach::{CoachSession, CommentaryStyle, Modality};
use coach_core::engine::UciEngine;
use coach_core::game::{speak_san, GameState};
use coach_core::llm::{build_backend, BackendConfig, CoachModel, NullBackend};
use coach_core::store::GameStore;
use coach_core::student::StudentProfile;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "coach", about = "teachmechess core — headless CLI")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Analyze a position with the engine (no LLM involved).
    Analyze {
        /// FEN of the position; defaults to the starting position.
        #[arg(long)]
        fen: Option<String>,
        /// Path to a UCI engine binary, or "embedded" for the statically
        /// linked Stockfish 11.
        #[arg(long, default_value = "stockfish")]
        engine: String,
        #[arg(long, default_value_t = 16)]
        depth: u32,
        #[arg(long, default_value_t = 3)]
        multipv: u32,
    },
    /// Render SAN moves as spoken English (the TTS verbalization layer).
    Speak {
        /// One or more SAN moves: Nf3 exd5 O-O Qh4#
        moves: Vec<String>,
    },
    /// One full coaching turn: play a move, get grounded commentary.
    ///
    /// Backend config comes from env: LLM_BASE_URL (default OpenAI),
    /// LLM_API_KEY, LLM_MODEL (default gpt-4o-mini) — i.e. the BYOLLM story.
    Coach {
        /// SAN move the "student" plays.
        #[arg(long = "move")]
        mv: String,
        /// Starting position FEN; defaults to the initial position.
        #[arg(long)]
        fen: Option<String>,
        /// Path to a UCI engine binary, or "embedded" for the statically
        /// linked Stockfish 11.
        #[arg(long, default_value = "stockfish")]
        engine: String,
        /// Emit voice-register commentary instead of text-register.
        #[arg(long)]
        voice: bool,
        /// Student profile JSON to load/update.
        #[arg(long, default_value = "student_profile.json")]
        profile: PathBuf,
    },
    /// Play a full game against the bot in the terminal, with coaching.
    ///
    /// The opponent should be Maia for human-like play:
    ///   coach play --opponent lc0 --opponent-arg=--weights=weights/maia-1100.pb.gz
    ///
    /// Every move, coach comment, and chat auto-records into the SQLite
    /// database at --db. 'quit' (or EOF/Ctrl-D) exits and LEAVES THE GAME
    /// OPEN — running `coach play` again with the same --db resumes it.
    /// Only checkmate/stalemate/'resign' finalize the game.
    Play {
        /// Analyst engine (judges every move).
        #[arg(long, default_value = "stockfish")]
        engine: String,
        /// Opponent engine binary (lc0 for Maia).
        #[arg(long, default_value = "lc0")]
        opponent: String,
        /// Extra args for the opponent binary (repeatable), e.g. --weights=…
        #[arg(long = "opponent-arg")]
        opponent_args: Vec<String>,
        /// Node budget per opponent move. 1 = authentic Maia policy play.
        #[arg(long, default_value_t = 1)]
        nodes: u64,
        /// Skip the LLM entirely: canned commentary built from engine facts.
        /// The commentary policy still runs — threat notes and summaries
        /// appear as deterministic one-liners.
        #[arg(long)]
        engine_only: bool,
        /// Voice-register commentary.
        #[arg(long)]
        voice: bool,
        /// How much the coach talks: quiet (notable moves only), balanced
        /// (milestones, occasional "why that was good", threat warnings,
        /// periodic summaries), or chatty (every move + more).
        #[arg(long, value_enum, default_value_t = StyleArg::Balanced)]
        style: StyleArg,
        /// Deprecated alias for `--style chatty`.
        #[arg(long)]
        chatty: bool,
        #[arg(long, default_value = "student_profile.json")]
        profile: PathBuf,
        /// SQLite database games/moves/chat record into (created on first
        /// use). An in-progress game there is resumed automatically.
        #[arg(long, default_value = "coach.db")]
        db: PathBuf,
    },
    /// Browse the games recorded in the --db database.
    ///
    /// Without --game: table of finished games (date, result, moves,
    /// accuracy, est. rating, opening), newest first. With --game <id>:
    /// that game's move list and chat feed.
    History {
        /// SQLite database written by `coach play --db`.
        #[arg(long, default_value = "coach.db")]
        db: PathBuf,
        /// Show at most this many games.
        #[arg(long, default_value_t = 20)]
        limit: u32,
        /// Show one game's moves + chat instead of the table.
        #[arg(long)]
        game: Option<i64>,
    },
    /// Eval harness: run a scripted suite through N backends side by side.
    Eval {
        /// TOML suite file (see evals/basic.toml).
        #[arg(long, default_value = "evals/basic.toml")]
        suite: PathBuf,
        #[arg(long, default_value = "eval_report.md")]
        report: PathBuf,
        #[arg(long, default_value = "eval_results.json")]
        json: PathBuf,
    },
}

/// CLI mirror of [`CommentaryStyle`] (coach-core doesn't depend on clap).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum StyleArg {
    Quiet,
    Balanced,
    Chatty,
}

impl From<StyleArg> for CommentaryStyle {
    fn from(s: StyleArg) -> Self {
        match s {
            StyleArg::Quiet => CommentaryStyle::Quiet,
            StyleArg::Balanced => CommentaryStyle::Balanced,
            StyleArg::Chatty => CommentaryStyle::Chatty,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Analyze {
            fen,
            engine,
            depth,
            multipv,
        } => analyze(fen, &engine, depth, multipv).await,
        Cmd::Speak { moves } => {
            if moves.is_empty() {
                bail!("give at least one SAN move, e.g. `coach speak Nf3 exd5 O-O`");
            }
            for m in moves {
                println!("{m:8} → {}", speak_san(&m));
            }
            Ok(())
        }
        Cmd::Coach {
            mv,
            fen,
            engine,
            voice,
            profile,
        } => coach_turn(mv, fen, &engine, voice, profile).await,
        Cmd::Play {
            engine,
            opponent,
            opponent_args,
            nodes,
            engine_only,
            voice,
            style,
            chatty,
            profile,
            db,
        } => {
            let style = if chatty {
                eprintln!("note: --chatty is deprecated, use --style chatty");
                CommentaryStyle::Chatty
            } else {
                style.into()
            };
            play(
                &engine,
                &opponent,
                &opponent_args,
                nodes,
                engine_only,
                voice,
                style,
                profile,
                db,
            )
            .await
        }
        Cmd::History { db, limit, game } => history(&db, limit, game),
        Cmd::Eval {
            suite,
            report,
            json,
        } => eval::run(&suite, &report, &json).await,
    }
}

/// Build the analyst engine. `--engine embedded` uses the statically linked
/// Stockfish 11 (the iOS engine path — a per-process singleton) instead of
/// spawning a binary.
async fn build_engine(engine_path: &str) -> Result<UciEngine> {
    if engine_path == "embedded" {
        let transport = stockfish_embedded::embedded_stockfish_transport()
            .await
            .context("failed to start the embedded Stockfish (already running in this process?)")?;
        return UciEngine::from_transport(Box::new(transport))
            .await
            .context("embedded Stockfish UCI handshake failed");
    }
    UciEngine::spawn(engine_path)
        .await
        .with_context(|| format!("failed to launch engine '{engine_path}' — is it installed?"))
}

/// BYOLLM backend from env vars; NullBackend when the caller wants engine-only.
fn backend_from_env(engine_only: bool) -> Arc<dyn CoachModel> {
    if engine_only {
        return Arc::new(NullBackend);
    }
    let provider = std::env::var("LLM_PROVIDER").unwrap_or_default();
    if provider.eq_ignore_ascii_case("anthropic") {
        let api_key = std::env::var("ANTHROPIC_API_KEY")
            .expect("LLM_PROVIDER=anthropic requires ANTHROPIC_API_KEY to be set");
        let model = std::env::var("LLM_MODEL").unwrap_or_else(|_| "claude-opus-5".into());
        return build_backend(BackendConfig::Anthropic {
            api_key,
            model,
            base_url: None,
        });
    }
    let base_url =
        std::env::var("LLM_BASE_URL").unwrap_or_else(|_| "https://api.openai.com/v1".into());
    let api_key = std::env::var("LLM_API_KEY").ok();
    let model = std::env::var("LLM_MODEL").unwrap_or_else(|_| "gpt-4o-mini".into());
    build_backend(BackendConfig::OpenAiCompat {
        base_url,
        api_key,
        model,
        capabilities: None,
    })
}

async fn analyze(fen: Option<String>, engine_path: &str, depth: u32, multipv: u32) -> Result<()> {
    let game = match &fen {
        Some(f) => GameState::from_fen(f).context("invalid FEN")?,
        None => GameState::new(),
    };
    let mut engine = build_engine(engine_path).await?;
    let analysis = engine.analyze(&game.fen(), depth, multipv).await?;

    println!("position: {}", game.fen());
    println!("best move: {}", analysis.best_move);
    for line in &analysis.lines {
        let eval = match line.score {
            coach_core::engine::Score::Cp(cp) => format!("{:+.2}", cp as f64 / 100.0),
            coach_core::engine::Score::Mate(n) => format!("mate {n}"),
        };
        println!(
            "  #{} [{}] depth {}: {}",
            line.multipv,
            eval,
            line.depth,
            line.pv.join(" ")
        );
    }
    Ok(())
}

async fn coach_turn(
    mv: String,
    fen: Option<String>,
    engine_path: &str,
    voice: bool,
    profile_path: PathBuf,
) -> Result<()> {
    let game = match &fen {
        Some(f) => GameState::from_fen(f).context("invalid FEN")?,
        None => GameState::new(),
    };
    let analyst = build_engine(engine_path).await?;
    let profile = StudentProfile::load(&profile_path).unwrap_or_default();

    let modality = if voice { Modality::Voice } else { Modality::Text };
    let mut session = CoachSession::new(backend_from_env(false), analyst, game, profile, modality);

    let reply = session.comment_on_move(&mv).await?;
    println!(
        "[engine] {} — {:?}, cp loss {}, best was {}",
        reply.verdict.played_san,
        reply.verdict.judgment,
        reply.verdict.cp_loss,
        reply.verdict.best_move_uci
    );
    println!("\n[coach] {}", reply.commentary);

    session.profile.save(&profile_path)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn play(
    engine_path: &str,
    opponent_path: &str,
    opponent_args: &[String],
    nodes: u64,
    engine_only: bool,
    voice: bool,
    style: CommentaryStyle,
    profile_path: PathBuf,
    db_path: PathBuf,
) -> Result<()> {
    let analyst = UciEngine::spawn(engine_path)
        .await
        .with_context(|| format!("failed to launch analyst '{engine_path}'"))?;
    let opponent = UciEngine::spawn_with_args(opponent_path, opponent_args)
        .await
        .with_context(|| {
            format!(
                "failed to launch opponent '{opponent_path}' — for Maia: brew install lc0, then \
                 pass --opponent-arg=--weights=weights/maia-1100.pb.gz"
            )
        })?;

    let profile = StudentProfile::load(&profile_path).unwrap_or_default();
    let modality = if voice { Modality::Voice } else { Modality::Text };
    let mut session = CoachSession::new(
        backend_from_env(engine_only),
        analyst,
        GameState::new(),
        profile,
        modality,
    );
    session.set_opponent(opponent, nodes);
    session.set_commentary_style(style);

    // Persistence: resume the database's in-progress game, or start
    // recording a fresh one.
    let store = GameStore::open(&db_path)
        .with_context(|| format!("failed to open game database {}", db_path.display()))?;
    match session.resume_from_store(store)? {
        Some(report) => {
            println!(
                "Resuming game in progress ({} moves): {}\n",
                report.history_san.len(),
                report.history_san.join(" ")
            );
            // If the quit happened after the student's move but before the
            // bot's reply, it is Black to move — let the bot catch up.
            if !session.game.turn_white() && !session.game.is_game_over() {
                if let Some((san, _uci)) = session.opponent_reply().await? {
                    println!("  bot plays {san} ({})\n", speak_san(&san));
                }
            }
        }
        None => println!("Recording to {}.\n", db_path.display()),
    }

    println!(
        "You are White. Enter moves in SAN (e4, Nf3, O-O). 'quit' saves the game \
         for later ('coach play --db {}' resumes it); 'resign' ends it as a loss.\n",
        db_path.display()
    );
    let stdin = std::io::stdin();
    // How the game ended, from the student's (White's) perspective, once it
    // is actually over on the board.
    use coach_core::student::GameResult;
    let student_result = |session: &CoachSession| match session.game.outcome_text() {
        Some(t) if t.starts_with("1-0") => GameResult::Win,
        Some(t) if t.starts_with("0-1") => GameResult::Loss,
        Some(_) => GameResult::Draw,
        None => GameResult::Aborted,
    };
    // `Some(result)` finalizes the stored game; `None` (quit/EOF) leaves it
    // open so the next `coach play --db` resumes it.
    let finished: Option<GameResult>;
    loop {
        println!("{}\n", session.game.board_diagram());
        if session.game.is_game_over() {
            if let Some(outcome) = session.game.outcome_text() {
                println!("Game over: {outcome}");
            }
            finished = Some(student_result(&session));
            break;
        }

        print!("your move> ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        if stdin.read_line(&mut line)? == 0 {
            finished = None; // EOF mid-game: leave the stored game open
            break;
        }
        let mv = line.trim();
        if mv.is_empty() {
            continue;
        }
        if mv.eq_ignore_ascii_case("quit") {
            finished = None; // game stays open in the store for resume
            break;
        }
        if mv.eq_ignore_ascii_case("resign") {
            finished = Some(GameResult::Loss);
            break;
        }

        // 1. Engine judges every move (cheap, silent).
        let verdict = match session.judge_student_move(mv).await {
            Ok(v) => v,
            Err(e) => {
                println!("  ✗ {e}");
                continue;
            }
        };
        println!(
            "  [engine] {:?}, cp loss {}",
            verdict.judgment, verdict.cp_loss
        );

        // 2. Commentary: the policy decides brief vs. full per --style; with
        //    --engine-only (NullBackend) the full path degrades to the same
        //    canned lines, so this one call covers both modes.
        //    react_to_student_move records its own chat row in the store.
        match session.react_to_student_move().await {
            Ok(text) => println!("  [coach] {text}"),
            Err(e) => {
                let brief = session.brief_reaction();
                println!("  [coach unavailable: {e}] {brief}");
                session.log_feed("coach", false, &brief);
            }
        }

        // 3. Bot replies.
        match session.opponent_reply().await? {
            Some((san, _uci)) => {
                println!("\n  bot plays {san} ({})", speak_san(&san));
                // 4. Policy-gated opponent commentary: threat warnings and
                //    "how the game is developing" summaries. Silent unless
                //    there is something worth saying.
                match session.react_to_opponent_move().await {
                    Ok(Some(text)) => println!("  [coach] {text}\n"),
                    Ok(None) => println!(),
                    Err(e) => println!("  [coach unavailable: {e}]\n"),
                }
            }
            None => {
                if let Some(outcome) = session.game.outcome_text() {
                    println!("\nGame over: {outcome}");
                }
                finished = Some(student_result(&session));
                break;
            }
        }
    }

    match finished {
        // The game actually ended: close it out — fold accuracy into the
        // profile (rating EMA, win counter, history), finalize the stored
        // row, and show the student where they stand.
        Some(result) => {
            let summary = session.finish_game(result);
            println!(
                "\nGame summary: {} moves judged, avg centipawn loss {:.1}, accuracy {:.1}%, \
                 this game looked like ~{} strength.",
                summary.moves_judged, summary.acl, summary.accuracy, summary.est_rating
            );
            println!(
                "Overall estimate: {} (bot level {}, {} win(s) at this level).",
                session.profile.rating_estimate,
                session.profile.bot_level,
                session.profile.wins_at_current_level
            );
            if summary.ready_to_advance && session.profile.bot_level < 1900 {
                println!(
                    "You're ready to move up! Next game, face the {} bot: \
                     pass --opponent-arg=--weights=weights/maia-{}.pb.gz",
                    session.profile.bot_level + 100,
                    session.profile.bot_level + 100
                );
            }
        }
        // Quit/EOF: the stored game stays open for resume.
        None => {
            println!(
                "\nGame saved in progress — run `coach play --db {}` to pick it back up.",
                db_path.display()
            );
        }
    }

    session.profile.save(&profile_path)?;
    Ok(())
}

/// Render unix seconds as "YYYY-MM-DD HH:MM" UTC (no chrono dependency;
/// Howard Hinnant's civil-from-days algorithm).
fn format_unix(ts: i64) -> String {
    let days = ts.div_euclid(86_400);
    let secs = ts.rem_euclid(86_400);
    let (hh, mm) = (secs / 3600, (secs % 3600) / 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}")
}

/// `coach history`: table of finished games, or one game's moves + chat.
fn history(db_path: &PathBuf, limit: u32, game: Option<i64>) -> Result<()> {
    if !db_path.exists() {
        bail!(
            "no database at {} — play a game first: coach play --db {}",
            db_path.display(),
            db_path.display()
        );
    }
    let store = GameStore::open_read_only(db_path)
        .with_context(|| format!("failed to open game database {}", db_path.display()))?;

    if let Some(id) = game {
        let Some(detail) = store.game_detail(id)? else {
            bail!("no game with id {id} in {}", db_path.display());
        };
        let g = &detail.game;
        println!(
            "Game {} — {} — {} — bot level {} — opponent {}",
            g.id,
            format_unix(g.started_at),
            g.result.as_deref().unwrap_or("in progress"),
            g.bot_level,
            g.opponent_kind
        );
        if let (Some(eco), Some(name)) = (&g.eco, &g.opening) {
            println!("Opening: {eco} {name}");
        }
        if let (Some(acl), Some(acc), Some(est)) = (g.acl, g.accuracy, g.est_rating) {
            println!("ACL {acl:.1}, accuracy {acc:.1}%, est. rating {est}");
        }
        println!("\nMoves:");
        for m in &detail.moves {
            let side = if m.by_student { "you" } else { "bot" };
            let judged = match (&m.judgment, m.cp_loss) {
                (Some(j), Some(cp)) => format!("  [{j}, cp loss {cp}]"),
                _ => String::new(),
            };
            println!("  {:>3}. {:8} ({side}){judged}", m.ply + 1, m.san);
        }
        if !detail.chat.is_empty() {
            println!("\nChat:");
            for c in &detail.chat {
                let review = if c.is_review { " (review)" } else { "" };
                println!("  [ply {:>3}] {:7}{review}: {}", c.at_ply, c.role, c.text);
            }
        }
        return Ok(());
    }

    let rows = store.list_games(limit, 0)?;
    if rows.is_empty() {
        println!("No finished games in {} yet.", db_path.display());
        return Ok(());
    }
    println!(
        "{:>4}  {:16}  {:8}  {:>5}  {:>6}  {:>5}  OPENING",
        "ID", "DATE", "RESULT", "MOVES", "ACC%", "EST"
    );
    for g in &rows {
        let opening = match (&g.eco, &g.opening) {
            (Some(eco), Some(name)) => format!("{eco} {name}"),
            _ => "—".to_string(),
        };
        println!(
            "{:>4}  {:16}  {:8}  {:>5}  {:>6}  {:>5}  {}",
            g.id,
            format_unix(g.started_at),
            g.result.as_deref().unwrap_or("open"),
            g.move_count,
            g.accuracy.map(|a| format!("{a:.1}")).unwrap_or_else(|| "—".into()),
            g.est_rating.map(|r| r.to_string()).unwrap_or_else(|| "—".into()),
            opening
        );
    }
    println!("\nUse `coach history --db {} --game <ID>` for moves + chat.", db_path.display());
    Ok(())
}
