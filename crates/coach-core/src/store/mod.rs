//! SQLite persistence for games, moves, and chat — plus in-progress-game
//! resume.
//!
//! One [`GameStore`] wraps one SQLite connection. The session holds a
//! read-write connection for the life of a game; history browsing (the FFI
//! free functions) opens short-lived read-only connections. The database is
//! in WAL mode, so the one writer and any number of readers coexist without
//! blocking each other.
//!
//! Schema versioning: `PRAGMA user_version` is set to [`SCHEMA_VERSION`] on
//! create; future migrations key off it.

use crate::coach::MoveVerdict;
use crate::engine::Judgment;
use crate::student::GameResult;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::Serialize;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Current schema version, stored in `PRAGMA user_version`.
pub const SCHEMA_VERSION: i32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("store schema version {0} is newer than this build supports ({SCHEMA_VERSION})")]
    SchemaTooNew(i32),
}

/// Unix seconds now. Falls back to 0 if the clock is before the epoch
/// (never in practice).
fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `GameResult` → its serde name (win/loss/draw/aborted), the form stored in
/// `games.result`.
fn result_str(r: GameResult) -> &'static str {
    match r {
        GameResult::Win => "win",
        GameResult::Loss => "loss",
        GameResult::Draw => "draw",
        GameResult::Aborted => "aborted",
    }
}

/// `Judgment` → its serde name, the form stored in `moves.judgment`.
fn judgment_str(j: Judgment) -> &'static str {
    j.label()
}

/// Parse a stored judgment string back into a [`Judgment`].
pub(crate) fn judgment_from_str(s: &str) -> Option<Judgment> {
    Some(match s {
        "best" => Judgment::Best,
        "excellent" => Judgment::Excellent,
        "good" => Judgment::Good,
        "inaccuracy" => Judgment::Inaccuracy,
        "mistake" => Judgment::Mistake,
        "blunder" => Judgment::Blunder,
        _ => return None,
    })
}

/// One stored move row.
#[derive(Debug, Clone, Serialize)]
pub struct StoredMove {
    pub ply: u32,
    pub san: String,
    pub uci: Option<String>,
    pub by_student: bool,
    pub judgment: Option<String>,
    pub cp_loss: Option<i32>,
    pub allows_mate_in: Option<i32>,
    pub missed_mate_in: Option<i32>,
    pub created_at: i64,
}

/// One stored chat/feed message.
#[derive(Debug, Clone, Serialize)]
pub struct StoredChat {
    pub at_ply: u32,
    /// student / coach / system.
    pub role: String,
    pub is_review: bool,
    pub text: String,
    pub created_at: i64,
}

/// The still-open game, as loaded for resume.
#[derive(Debug, Serialize)]
pub struct OpenGame {
    pub id: i64,
    pub starting_fen: Option<String>,
    pub bot_level: u32,
    pub moves: Vec<StoredMove>,
    pub chat: Vec<StoredChat>,
    pub transcript_json: Option<String>,
}

/// One row of the games table (history listing).
#[derive(Debug, Clone, Serialize)]
pub struct GameRow {
    pub id: i64,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    /// win/loss/draw/aborted; `None` while the game is still open.
    pub result: Option<String>,
    pub starting_fen: Option<String>,
    pub bot_level: u32,
    pub opponent_kind: String,
    pub eco: Option<String>,
    pub opening: Option<String>,
    pub acl: Option<f64>,
    pub accuracy: Option<f64>,
    pub est_rating: Option<i64>,
    pub moves_judged: Option<i64>,
    /// Total moves recorded for the game (both sides).
    pub move_count: u32,
}

/// One game with its full move list and chat feed.
#[derive(Debug, Serialize)]
pub struct GameDetail {
    pub game: GameRow,
    pub moves: Vec<StoredMove>,
    pub chat: Vec<StoredChat>,
}

/// Store-wide aggregates for the profile/stats screen.
#[derive(Debug, Clone, Serialize)]
pub struct StoreStats {
    /// All game rows, including a still-open one.
    pub games: u32,
    pub wins: u32,
    pub losses: u32,
    pub draws: u32,
    pub aborted: u32,
    /// Best accuracy over finished games.
    pub best_accuracy: Option<f64>,
    /// Bot level of the most recently started game.
    pub current_bot_level: Option<u32>,
}

const SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS games (
    id              INTEGER PRIMARY KEY,
    started_at      INTEGER NOT NULL,
    ended_at        INTEGER,
    result          TEXT,
    starting_fen    TEXT,
    bot_level       INTEGER NOT NULL,
    opponent_kind   TEXT NOT NULL,
    eco             TEXT,
    opening         TEXT,
    acl             REAL,
    accuracy        REAL,
    est_rating      INTEGER,
    moves_judged    INTEGER,
    transcript_json TEXT
);
CREATE TABLE IF NOT EXISTS moves (
    game_id        INTEGER NOT NULL REFERENCES games(id) ON DELETE CASCADE,
    ply            INTEGER NOT NULL,
    san            TEXT NOT NULL,
    uci            TEXT,
    by_student     INTEGER NOT NULL,
    judgment       TEXT,
    cp_loss        INTEGER,
    allows_mate_in INTEGER,
    missed_mate_in INTEGER,
    created_at     INTEGER NOT NULL,
    PRIMARY KEY (game_id, ply)
);
CREATE TABLE IF NOT EXISTS chat_messages (
    id         INTEGER PRIMARY KEY,
    game_id    INTEGER NOT NULL REFERENCES games(id) ON DELETE CASCADE,
    at_ply     INTEGER NOT NULL,
    role       TEXT NOT NULL,
    is_review  INTEGER NOT NULL DEFAULT 0,
    text       TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
";

/// SQLite-backed persistence for games, moves, and chat.
pub struct GameStore {
    conn: Connection,
}

impl GameStore {
    /// Open (creating if needed) the store at `path`. Sets WAL journaling,
    /// enables foreign keys, and creates the schema if missing.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        // WAL so the session's writer and history's readers coexist. The
        // pragma returns a row — query it rather than execute.
        let _mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
        Self::init(conn)
    }

    /// In-memory store for tests. (`journal_mode` stays `memory` — WAL only
    /// applies to file databases.)
    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::init(Connection::open_in_memory()?)
    }

    /// Read-only connection for history browsing while another connection
    /// may be writing (WAL allows this). Falls back to a normal read-write
    /// open if the read-only open fails (e.g. the `-shm` file cannot be
    /// created by a pure reader).
    pub fn open_read_only(path: &Path) -> Result<Self, StoreError> {
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        match Connection::open_with_flags(path, flags) {
            Ok(conn) => {
                conn.execute_batch("PRAGMA foreign_keys=ON;")?;
                Self::check_version(&conn)?;
                Ok(Self { conn })
            }
            Err(_) => Self::open(path),
        }
    }

    fn init(conn: Connection) -> Result<Self, StoreError> {
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        Self::check_version(&conn)?;
        conn.execute_batch(SCHEMA_SQL)?;
        let version: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version < SCHEMA_VERSION {
            // Fresh database (0) → stamp the current version. Future
            // migrations for 1..SCHEMA_VERSION go here.
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        Ok(Self { conn })
    }

    fn check_version(conn: &Connection) -> Result<(), StoreError> {
        let version: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(StoreError::SchemaTooNew(version));
        }
        Ok(())
    }

    /// Start a new game row and return its id.
    ///
    /// INVARIANT: at most one open game — any still-open game is first
    /// closed as `aborted` (with `ended_at`).
    pub fn begin_game(
        &self,
        starting_fen: Option<&str>,
        bot_level: u32,
        opponent_kind: &str,
    ) -> Result<i64, StoreError> {
        let now = now_unix();
        self.conn.execute(
            "UPDATE games SET result='aborted', ended_at=?1 WHERE result IS NULL",
            (now,),
        )?;
        self.conn.execute(
            "INSERT INTO games (started_at, starting_fen, bot_level, opponent_kind)
             VALUES (?1, ?2, ?3, ?4)",
            (now, starting_fen, bot_level, opponent_kind),
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Record one played move. `ply` is 0-based (move i of the game is ply
    /// i). Upserts, so re-recording a ply (e.g. after a resume replay)
    /// overwrites rather than errors.
    pub fn record_move(
        &self,
        game_id: i64,
        ply: u32,
        san: &str,
        uci: Option<&str>,
        by_student: bool,
        verdict: Option<&MoveVerdict>,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT OR REPLACE INTO moves
                (game_id, ply, san, uci, by_student, judgment, cp_loss,
                 allows_mate_in, missed_mate_in, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            (
                game_id,
                ply,
                san,
                uci,
                by_student,
                verdict.map(|v| judgment_str(v.judgment)),
                verdict.map(|v| v.cp_loss),
                verdict.and_then(|v| v.allows_mate_in),
                verdict.and_then(|v| v.missed_mate_in),
                now_unix(),
            ),
        )?;
        Ok(())
    }

    /// Record one chat/feed message. `role` ∈ student/coach/system.
    pub fn record_chat(
        &self,
        game_id: i64,
        at_ply: u32,
        role: &str,
        is_review: bool,
        text: &str,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO chat_messages (game_id, at_ply, role, is_review, text, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            (game_id, at_ply, role, is_review, text, now_unix()),
        )?;
        Ok(())
    }

    /// Set/refresh the identified opening (longest match wins by virtue of
    /// being called again as the game deepens).
    pub fn set_opening(&self, game_id: i64, eco: &str, name: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE games SET eco=?2, opening=?3 WHERE id=?1",
            (game_id, eco, name),
        )?;
        Ok(())
    }

    /// Save the serialized LLM transcript for the game (overwrites).
    pub fn save_transcript(&self, game_id: i64, json: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE games SET transcript_json=?2 WHERE id=?1",
            (game_id, json),
        )?;
        Ok(())
    }

    /// Finalize a game row with its result and summary numbers.
    #[allow(clippy::too_many_arguments)]
    pub fn finish_game(
        &self,
        game_id: i64,
        result: GameResult,
        acl: f64,
        accuracy: f64,
        est_rating: u32,
        moves_judged: u32,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE games SET result=?2, ended_at=?3, acl=?4, accuracy=?5,
                              est_rating=?6, moves_judged=?7
             WHERE id=?1",
            (
                game_id,
                result_str(result),
                now_unix(),
                acl,
                accuracy,
                est_rating,
                moves_judged,
            ),
        )?;
        Ok(())
    }

    /// Close a game as `aborted` without summary numbers (e.g. its stored
    /// moves no longer replay).
    pub fn abort_game(&self, game_id: i64) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE games SET result='aborted', ended_at=?2 WHERE id=?1 AND result IS NULL",
            (game_id, now_unix()),
        )?;
        Ok(())
    }

    /// The still-open game, if any, with everything needed to resume it.
    pub fn open_game(&self) -> Result<Option<OpenGame>, StoreError> {
        let row = self
            .conn
            .query_row(
                "SELECT id, starting_fen, bot_level, transcript_json
                 FROM games WHERE result IS NULL ORDER BY id DESC LIMIT 1",
                [],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, u32>(2)?,
                        r.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((id, starting_fen, bot_level, transcript_json)) = row else {
            return Ok(None);
        };
        Ok(Some(OpenGame {
            id,
            starting_fen,
            bot_level,
            moves: self.moves_for(id)?,
            chat: self.chat_for(id)?,
            transcript_json,
        }))
    }

    /// Every recorded move of one game, ply order. Public because the live
    /// session reads its own in-progress rows back (the pause recap needs
    /// the judgments it stayed quiet about).
    pub fn moves_for(&self, game_id: i64) -> Result<Vec<StoredMove>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT ply, san, uci, by_student, judgment, cp_loss,
                    allows_mate_in, missed_mate_in, created_at
             FROM moves WHERE game_id=?1 ORDER BY ply",
        )?;
        let rows = stmt.query_map((game_id,), |r| {
            Ok(StoredMove {
                ply: r.get(0)?,
                san: r.get(1)?,
                uci: r.get(2)?,
                by_student: r.get(3)?,
                judgment: r.get(4)?,
                cp_loss: r.get(5)?,
                allows_mate_in: r.get(6)?,
                missed_mate_in: r.get(7)?,
                created_at: r.get(8)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    fn chat_for(&self, game_id: i64) -> Result<Vec<StoredChat>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT at_ply, role, is_review, text, created_at
             FROM chat_messages WHERE game_id=?1 ORDER BY id",
        )?;
        let rows = stmt.query_map((game_id,), |r| {
            Ok(StoredChat {
                at_ply: r.get(0)?,
                role: r.get(1)?,
                is_review: r.get(2)?,
                text: r.get(3)?,
                created_at: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    const GAME_ROW_COLS: &'static str =
        "g.id, g.started_at, g.ended_at, g.result, g.starting_fen, g.bot_level,
         g.opponent_kind, g.eco, g.opening, g.acl, g.accuracy, g.est_rating, g.moves_judged,
         (SELECT COUNT(*) FROM moves m WHERE m.game_id = g.id)";

    fn map_game_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<GameRow> {
        Ok(GameRow {
            id: r.get(0)?,
            started_at: r.get(1)?,
            ended_at: r.get(2)?,
            result: r.get(3)?,
            starting_fen: r.get(4)?,
            bot_level: r.get(5)?,
            opponent_kind: r.get(6)?,
            eco: r.get(7)?,
            opening: r.get(8)?,
            acl: r.get(9)?,
            accuracy: r.get(10)?,
            est_rating: r.get(11)?,
            moves_judged: r.get(12)?,
            move_count: r.get(13)?,
        })
    }

    /// Finished games, newest first.
    pub fn list_games(&self, limit: u32, offset: u32) -> Result<Vec<GameRow>, StoreError> {
        let sql = format!(
            "SELECT {} FROM games g WHERE g.result IS NOT NULL
             ORDER BY g.started_at DESC, g.id DESC LIMIT ?1 OFFSET ?2",
            Self::GAME_ROW_COLS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map((limit, offset), Self::map_game_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// One game (open or finished) with its moves and chat.
    pub fn game_detail(&self, id: i64) -> Result<Option<GameDetail>, StoreError> {
        let sql = format!("SELECT {} FROM games g WHERE g.id=?1", Self::GAME_ROW_COLS);
        let game = self
            .conn
            .query_row(&sql, (id,), Self::map_game_row)
            .optional()?;
        let Some(game) = game else { return Ok(None) };
        Ok(Some(GameDetail {
            moves: self.moves_for(id)?,
            chat: self.chat_for(id)?,
            game,
        }))
    }

    /// Store-wide aggregates.
    pub fn stats(&self) -> Result<StoreStats, StoreError> {
        let (games, wins, losses, draws, aborted, best_accuracy) = self.conn.query_row(
            "SELECT COUNT(*),
                    COALESCE(SUM(result='win'), 0),
                    COALESCE(SUM(result='loss'), 0),
                    COALESCE(SUM(result='draw'), 0),
                    COALESCE(SUM(result='aborted'), 0),
                    MAX(CASE WHEN result IS NOT NULL THEN accuracy END)
             FROM games",
            [],
            |r| {
                Ok((
                    r.get::<_, u32>(0)?,
                    r.get::<_, u32>(1)?,
                    r.get::<_, u32>(2)?,
                    r.get::<_, u32>(3)?,
                    r.get::<_, u32>(4)?,
                    r.get::<_, Option<f64>>(5)?,
                ))
            },
        )?;
        let current_bot_level: Option<u32> = self
            .conn
            .query_row(
                "SELECT bot_level FROM games ORDER BY started_at DESC, id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        Ok(StoreStats {
            games,
            wins,
            losses,
            draws,
            aborted,
            best_accuracy,
            current_bot_level,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verdict(san: &str, cp_loss: i32, judgment: Judgment) -> MoveVerdict {
        MoveVerdict {
            played_san: san.into(),
            cp_loss,
            judgment,
            best_move_uci: "e2e4".into(),
            best_line: vec!["e2e4".into(), "e7e5".into()],
            eval_before_cp: 30,
            eval_after_cp: 30 - cp_loss,
            allows_mate_in: None,
            missed_mate_in: None,
        }
    }

    #[test]
    fn begin_record_finish_round_trip() {
        let store = GameStore::open_in_memory().unwrap();
        let id = store.begin_game(None, 1100, "external").unwrap();

        store
            .record_move(id, 0, "e4", Some("e2e4"), true, Some(&verdict("e4", 5, Judgment::Best)))
            .unwrap();
        store.record_move(id, 1, "e5", Some("e7e5"), false, None).unwrap();
        store.record_chat(id, 1, "coach", false, "Nice opening move.").unwrap();
        store.set_opening(id, "B00", "King's Pawn Game").unwrap();
        store.save_transcript(id, "[]").unwrap();
        store
            .finish_game(id, GameResult::Win, 12.5, 96.0, 1400, 1)
            .unwrap();

        let detail = store.game_detail(id).unwrap().unwrap();
        assert_eq!(detail.game.result.as_deref(), Some("win"));
        assert_eq!(detail.game.eco.as_deref(), Some("B00"));
        assert_eq!(detail.game.move_count, 2);
        assert_eq!(detail.game.est_rating, Some(1400));
        assert!(detail.game.ended_at.is_some());
        assert_eq!(detail.moves.len(), 2);
        assert_eq!(detail.moves[0].san, "e4");
        assert_eq!(detail.moves[0].judgment.as_deref(), Some("best"));
        assert_eq!(detail.moves[0].cp_loss, Some(5));
        assert!(detail.moves[0].by_student);
        assert!(detail.moves[1].judgment.is_none());
        assert!(!detail.moves[1].by_student);
        assert_eq!(detail.chat.len(), 1);
        assert_eq!(detail.chat[0].role, "coach");
    }

    #[test]
    fn at_most_one_open_game() {
        let store = GameStore::open_in_memory().unwrap();
        let a = store.begin_game(None, 1100, "none").unwrap();
        let b = store.begin_game(None, 1200, "none").unwrap();
        assert_ne!(a, b);

        // Beginning `b` closed `a` as aborted.
        let a_row = store.game_detail(a).unwrap().unwrap().game;
        assert_eq!(a_row.result.as_deref(), Some("aborted"));
        assert!(a_row.ended_at.is_some());

        let open = store.open_game().unwrap().unwrap();
        assert_eq!(open.id, b);
        assert_eq!(open.bot_level, 1200);
    }

    #[test]
    fn cascade_delete_removes_moves_and_chat() {
        let store = GameStore::open_in_memory().unwrap();
        let id = store.begin_game(None, 1100, "none").unwrap();
        store.record_move(id, 0, "e4", None, true, None).unwrap();
        store.record_chat(id, 0, "system", false, "hello").unwrap();

        store.conn.execute("DELETE FROM games WHERE id=?1", (id,)).unwrap();
        let moves: u32 = store
            .conn
            .query_row("SELECT COUNT(*) FROM moves", [], |r| r.get(0))
            .unwrap();
        let chat: u32 = store
            .conn
            .query_row("SELECT COUNT(*) FROM chat_messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(moves, 0);
        assert_eq!(chat, 0);
    }

    #[test]
    fn resume_payload_contains_everything() {
        let store = GameStore::open_in_memory().unwrap();
        assert!(store.open_game().unwrap().is_none());

        let id = store.begin_game(Some("FEN"), 1300, "analyst_skill:5").unwrap();
        store
            .record_move(id, 0, "e4", Some("e2e4"), true, Some(&verdict("e4", 3, Judgment::Excellent)))
            .unwrap();
        store.record_move(id, 1, "c5", Some("c7c5"), false, None).unwrap();
        store.record_chat(id, 1, "student", false, "why c5?").unwrap();
        store.record_chat(id, 1, "coach", false, "The Sicilian.").unwrap();
        store.save_transcript(id, r#"[{"role":"user","content":"hi"}]"#).unwrap();

        let open = store.open_game().unwrap().unwrap();
        assert_eq!(open.id, id);
        assert_eq!(open.starting_fen.as_deref(), Some("FEN"));
        assert_eq!(open.bot_level, 1300);
        assert_eq!(open.moves.len(), 2);
        assert_eq!(open.moves[1].san, "c5");
        assert_eq!(open.chat.len(), 2);
        assert_eq!(open.chat[0].text, "why c5?");
        assert!(open.transcript_json.unwrap().contains("hi"));

        // Aborting closes it; nothing left open.
        store.abort_game(id).unwrap();
        assert!(store.open_game().unwrap().is_none());
    }

    #[test]
    fn list_games_is_finished_only_newest_first() {
        let store = GameStore::open_in_memory().unwrap();
        let a = store.begin_game(None, 1100, "none").unwrap();
        store.record_move(a, 0, "e4", None, true, None).unwrap();
        store.finish_game(a, GameResult::Loss, 90.0, 65.0, 1000, 10).unwrap();
        let b = store.begin_game(None, 1100, "none").unwrap();
        store.finish_game(b, GameResult::Win, 40.0, 85.0, 1600, 20).unwrap();
        let _open = store.begin_game(None, 1100, "none").unwrap();

        let rows = store.list_games(10, 0).unwrap();
        // started_at ties (same second) break by id DESC → b before a.
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, b);
        assert_eq!(rows[0].result.as_deref(), Some("win"));
        assert_eq!(rows[1].id, a);
        assert_eq!(rows[1].move_count, 1);

        // Pagination.
        let page = store.list_games(1, 1).unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].id, a);
    }

    #[test]
    fn stats_aggregate() {
        let store = GameStore::open_in_memory().unwrap();
        let stats = store.stats().unwrap();
        assert_eq!(stats.games, 0);
        assert!(stats.current_bot_level.is_none());
        assert!(stats.best_accuracy.is_none());

        let a = store.begin_game(None, 1100, "none").unwrap();
        store.finish_game(a, GameResult::Win, 40.0, 85.0, 1600, 20).unwrap();
        let b = store.begin_game(None, 1100, "none").unwrap();
        store.finish_game(b, GameResult::Loss, 90.0, 65.0, 1000, 12).unwrap();
        let c = store.begin_game(None, 1200, "none").unwrap();
        store.finish_game(c, GameResult::Draw, 60.0, 75.0, 1300, 30).unwrap();
        let _open = store.begin_game(None, 1300, "none").unwrap();

        let stats = store.stats().unwrap();
        assert_eq!(stats.games, 4);
        assert_eq!(stats.wins, 1);
        assert_eq!(stats.losses, 1);
        assert_eq!(stats.draws, 1);
        assert_eq!(stats.aborted, 0);
        assert_eq!(stats.best_accuracy, Some(85.0));
        assert_eq!(stats.current_bot_level, Some(1300));
    }

    #[test]
    fn schema_version_is_stamped() {
        let store = GameStore::open_in_memory().unwrap();
        let v: i32 = store
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
    }

    #[test]
    fn file_store_reopens_and_reads_back() {
        let dir = std::env::temp_dir().join(format!("coach-store-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.db");
        let _ = std::fs::remove_file(&path);

        {
            let store = GameStore::open(&path).unwrap();
            let id = store.begin_game(None, 1100, "none").unwrap();
            store.record_move(id, 0, "d4", Some("d2d4"), true, None).unwrap();
        }
        {
            let store = GameStore::open(&path).unwrap();
            let open = store.open_game().unwrap().unwrap();
            assert_eq!(open.moves[0].san, "d4");
            // Read-only opener sees the same data.
            let ro = GameStore::open_read_only(&path).unwrap();
            assert_eq!(ro.stats().unwrap().games, 1);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
