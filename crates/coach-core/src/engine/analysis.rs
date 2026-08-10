//! Move classification: turn raw engine evals into the vocabulary the coach
//! (and the rating estimator) speaks.

use serde::{Deserialize, Serialize};

/// Classification of a played move relative to the engine's best.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Judgment {
    Best,
    Excellent,
    Good,
    Inaccuracy,
    Mistake,
    Blunder,
}

impl Judgment {
    /// Should the coach speak up about this move unprompted? Commentary
    /// cadence is a product decision as much as a cost decision: good coaches
    /// don't narrate every move. The engine classifies silently; the LLM is
    /// only invoked on notable moments (or when spoken to).
    pub fn is_notable(&self) -> bool {
        matches!(self, Judgment::Inaccuracy | Judgment::Mistake | Judgment::Blunder)
    }
}

/// Classify a move given the eval of the best line and the eval actually
/// achieved, both in centipawns **from the mover's perspective**.
///
/// Typical flow: analyze the position before the move (MultiPV — `lines[0]`
/// is `best_cp`), play the move, analyze the resulting position (single PV,
/// now from the opponent's perspective) and negate it to get `played_cp`.
///
/// Thresholds follow common annotator conventions; they'll be tuned once the
/// eval harness exists.
pub fn judge_move(best_cp: i32, played_cp: i32) -> (i32, Judgment) {
    let loss = (best_cp - played_cp).max(0);
    let judgment = match loss {
        0..=9 => Judgment::Best,
        10..=25 => Judgment::Excellent,
        26..=50 => Judgment::Good,
        51..=100 => Judgment::Inaccuracy,
        101..=250 => Judgment::Mistake,
        _ => Judgment::Blunder,
    };
    (loss, judgment)
}

// Per-game accuracy accumulation and the ACL → rating estimate live in
// `super::accuracy` ([`super::accuracy::GameAccuracy`],
// [`super::accuracy::estimated_rating_from_acl`]); the session feeds each
// move's cp loss from here into that accumulator.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_across_thresholds() {
        assert_eq!(judge_move(50, 50).1, Judgment::Best);
        assert_eq!(judge_move(50, 30).1, Judgment::Excellent);
        assert_eq!(judge_move(50, 10).1, Judgment::Good);
        assert_eq!(judge_move(50, -40).1, Judgment::Inaccuracy);
        assert_eq!(judge_move(50, -100).1, Judgment::Mistake);
        assert_eq!(judge_move(50, -300).1, Judgment::Blunder);
    }

    #[test]
    fn gaining_eval_is_never_penalized() {
        let (loss, judgment) = judge_move(20, 80);
        assert_eq!(loss, 0);
        assert_eq!(judgment, Judgment::Best);
    }
}
