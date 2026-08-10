//! The student's persistent memory: what the coach has taught, how well it
//! stuck, and where the student's level sits.
//!
//! Deliberately *not* a vector store. The chess curriculum is a known, finite
//! domain (a few hundred concepts), so it's modeled as a structured concept
//! map with mastery states — smaller, fully offline, inspectable, and free to
//! query on the BYOLLM tier. The LLM reads it via `get_student_profile` and
//! writes via `update_student_profile`; curriculum *logic* (what to teach
//! next, when to reinforce) stays in deterministic code.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// Where a concept sits on the taught → mastered ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConceptStatus {
    NotIntroduced,
    /// The coach has explained it.
    Taught,
    /// The student has successfully used it in a game.
    Demonstrated,
    /// Used repeatedly without prompting; stop reinforcing.
    Mastered,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConceptRecord {
    pub status: ConceptStatus,
    /// Game counter when this concept was last taught/observed — the input
    /// to spaced-repetition scheduling.
    pub last_touched_game: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// How a game ended, from the student's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameResult {
    Win,
    Loss,
    Draw,
    /// Quit/abandoned mid-game. Counts as played, but a short aborted game
    /// carries too little signal to move the rating estimate.
    Aborted,
}

/// One finished game's accuracy footprint — the per-game input to the rating
/// EMA and the advancement check.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct GameRecord {
    pub result: GameResult,
    /// Average centipawn loss across the student's judged moves.
    pub acl: f64,
    /// Accuracy percentage (0..=100) derived from `acl`.
    pub accuracy: f64,
    /// Rating estimate for this single game (from the ACL mapping).
    pub est_rating: u32,
    /// Bot level (Maia band) the game was played against.
    pub bot_level: u32,
    /// Number of student moves the engine judged.
    #[serde(default)]
    pub moves_judged: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StudentProfile {
    /// Estimated rating, driven by move accuracy + results vs. Maia bands.
    pub rating_estimate: u32,
    /// Current opponent level (a Maia rating band: 1100..=1900).
    pub bot_level: u32,
    pub games_played: u32,
    /// Wins against the current bot level — the auto-advancement counter.
    pub wins_at_current_level: u32,
    /// Concept id → record. Ids are stable slugs: "fork", "pin",
    /// "back_rank_mate", "opening:ruy_lopez", …
    pub concepts: BTreeMap<String, ConceptRecord>,
    /// Free-form coach observations ("tends to trade queens too early").
    pub notes: Vec<String>,
    /// Per-game accuracy/result history, newest last. `#[serde(default)]`
    /// so profiles written before this field existed still load.
    #[serde(default)]
    pub game_history: Vec<GameRecord>,
}

impl Default for StudentProfile {
    fn default() -> Self {
        Self {
            rating_estimate: 800,
            bot_level: 1100,
            games_played: 0,
            wins_at_current_level: 0,
            concepts: BTreeMap::new(),
            notes: Vec::new(),
            game_history: Vec::new(),
        }
    }
}

impl StudentProfile {
    pub fn load(path: &Path) -> std::io::Result<Self> {
        let data = std::fs::read_to_string(path)?;
        serde_json::from_str(&data)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let data = serde_json::to_string_pretty(self).expect("profile serializes");
        std::fs::write(path, data)
    }

    /// Record that a concept was taught/observed at some status. Status only
    /// moves forward — a mastered concept never regresses to taught (a run of
    /// bad games is handled by the notes/rating, not the concept ladder).
    pub fn touch_concept(&mut self, id: &str, status: ConceptStatus, note: Option<String>) {
        let game = self.games_played;
        let entry = self
            .concepts
            .entry(id.to_string())
            .or_insert(ConceptRecord {
                status: ConceptStatus::NotIntroduced,
                last_touched_game: game,
                note: None,
            });
        entry.status = entry.status.max(status);
        entry.last_touched_game = game;
        if note.is_some() {
            entry.note = note;
        }
    }

    /// Compact one-paragraph summary for the system prompt. Kept short on
    /// purpose — the compact (on-device) tier has ~4K tokens total.
    pub fn summary(&self) -> String {
        let taught: Vec<&str> = self
            .concepts
            .iter()
            .filter(|(_, r)| r.status >= ConceptStatus::Taught && r.status < ConceptStatus::Mastered)
            .map(|(id, _)| id.as_str())
            .collect();
        let mastered: Vec<&str> = self
            .concepts
            .iter()
            .filter(|(_, r)| r.status == ConceptStatus::Mastered)
            .map(|(id, _)| id.as_str())
            .collect();
        format!(
            "Estimated rating {}. Playing bot level {}. Games played: {}. \
             Concepts in progress: [{}]. Mastered: [{}]. Coach notes: {}",
            self.rating_estimate,
            self.bot_level,
            self.games_played,
            taught.join(", "),
            mastered.join(", "),
            if self.notes.is_empty() {
                "none yet".to_string()
            } else {
                self.notes.join("; ")
            }
        )
    }

    /// Record a finished game: push it to history, bump `games_played`,
    /// fold the game's rating estimate into `rating_estimate` as an EMA
    /// (`new = 0.7 * old + 0.3 * game_est`), and count a `Win` toward
    /// advancement. Aborted games with fewer than 5 judged moves carry too
    /// little signal, so they skip the rating update (but still count as
    /// played and stay in history).
    pub fn record_game(&mut self, record: GameRecord) {
        const EMA_OLD_WEIGHT: f64 = 0.7;
        const EMA_NEW_WEIGHT: f64 = 0.3;
        const MIN_MOVES_FOR_RATING: u32 = 5;

        self.games_played += 1;
        let skip_rating =
            record.result == GameResult::Aborted && record.moves_judged < MIN_MOVES_FOR_RATING;
        if !skip_rating {
            let blended = EMA_OLD_WEIGHT * f64::from(self.rating_estimate)
                + EMA_NEW_WEIGHT * f64::from(record.est_rating);
            self.rating_estimate = blended.round() as u32;
        }
        if record.result == GameResult::Win {
            self.wins_at_current_level += 1;
        }
        self.game_history.push(record);
    }

    /// Auto-advancement check: enough wins at this level *and* a rating
    /// estimate within striking distance of the current band, so a student
    /// who wins on opponent blunders alone isn't promoted. Deterministic —
    /// purely a function of the profile's counters.
    pub fn ready_to_advance(&self) -> bool {
        const WINS_NEEDED: u32 = 3;
        self.wins_at_current_level >= WINS_NEEDED
            && self.rating_estimate + 100 >= self.bot_level
    }

    /// Move up one Maia band (steps of 100, capped at 1900) and reset the
    /// win counter for the new level.
    pub fn advance_level(&mut self) {
        const MAX_BOT_LEVEL: u32 = 1900;
        const BAND_STEP: u32 = 100;
        self.bot_level = (self.bot_level + BAND_STEP).min(MAX_BOT_LEVEL);
        self.wins_at_current_level = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concept_status_never_regresses() {
        let mut p = StudentProfile::default();
        p.touch_concept("fork", ConceptStatus::Mastered, None);
        p.touch_concept("fork", ConceptStatus::Taught, None);
        assert_eq!(p.concepts["fork"].status, ConceptStatus::Mastered);
    }

    fn record(result: GameResult, est_rating: u32, moves_judged: u32) -> GameRecord {
        GameRecord {
            result,
            acl: 80.0,
            accuracy: 69.0,
            est_rating,
            bot_level: 1100,
            moves_judged,
        }
    }

    #[test]
    fn record_game_applies_ema() {
        let mut p = StudentProfile::default(); // rating 800
        p.record_game(record(GameResult::Loss, 1200, 30));
        // 0.7*800 + 0.3*1200 = 920
        assert_eq!(p.rating_estimate, 920);
        assert_eq!(p.games_played, 1);
        assert_eq!(p.wins_at_current_level, 0);
        assert_eq!(p.game_history.len(), 1);

        p.record_game(record(GameResult::Win, 1200, 30));
        // 0.7*920 + 0.3*1200 = 1004
        assert_eq!(p.rating_estimate, 1004);
        assert_eq!(p.wins_at_current_level, 1);
    }

    #[test]
    fn short_aborted_game_skips_rating_but_counts_as_played() {
        let mut p = StudentProfile::default();
        p.record_game(record(GameResult::Aborted, 2400, 3));
        assert_eq!(p.rating_estimate, 800);
        assert_eq!(p.games_played, 1);
        assert_eq!(p.game_history.len(), 1);

        // A long aborted game still carries signal → rating updates.
        p.record_game(record(GameResult::Aborted, 1200, 20));
        assert_eq!(p.rating_estimate, 920);
    }

    #[test]
    fn advancement_needs_wins_and_rating() {
        let mut p = StudentProfile::default(); // rating 800, bot 1100
        for _ in 0..3 {
            p.record_game(record(GameResult::Win, 800, 30));
        }
        assert_eq!(p.wins_at_current_level, 3);
        // rating_estimate stays 800 < 1100 - 100 → not ready on wins alone.
        assert!(!p.ready_to_advance());

        p.rating_estimate = 1000; // 1000 + 100 >= 1100
        assert!(p.ready_to_advance());

        p.advance_level();
        assert_eq!(p.bot_level, 1200);
        assert_eq!(p.wins_at_current_level, 0);
        assert!(!p.ready_to_advance());
    }

    #[test]
    fn advance_level_caps_at_1900() {
        let mut p = StudentProfile::default();
        p.bot_level = 1900;
        p.advance_level();
        assert_eq!(p.bot_level, 1900);
    }

    #[test]
    fn old_profile_json_without_history_still_loads() {
        let json = r#"{
            "rating_estimate": 900,
            "bot_level": 1100,
            "games_played": 4,
            "wins_at_current_level": 1,
            "concepts": {},
            "notes": []
        }"#;
        let p: StudentProfile = serde_json::from_str(json).unwrap();
        assert!(p.game_history.is_empty());
        assert_eq!(p.rating_estimate, 900);
    }

    #[test]
    fn roundtrips_json() {
        let mut p = StudentProfile::default();
        p.touch_concept("pin", ConceptStatus::Taught, Some("via Ruy Lopez game".into()));
        let json = serde_json::to_string(&p).unwrap();
        let back: StudentProfile = serde_json::from_str(&json).unwrap();
        assert_eq!(back.concepts["pin"].status, ConceptStatus::Taught);
    }
}
