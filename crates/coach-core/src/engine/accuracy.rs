//! Per-game accuracy accumulation and the ACL → rating estimator.
//!
//! This is the deterministic half of auto-advancement: every student move's
//! centipawn loss (from [`super::analysis::judge_move`]) feeds a
//! [`GameAccuracy`] accumulator; at game end the average centipawn loss (ACL)
//! maps onto an accuracy percentage and a rating estimate. Combined with
//! win-rate against a known Maia band, this drives the "ready to move up?"
//! prompt. No LLM anywhere in this path — it's cheap, offline, and testable.

use serde::{Deserialize, Serialize};

/// Accumulates per-move centipawn loss over one game.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct GameAccuracy {
    moves: u32,
    total_cp_loss: i64,
}

impl GameAccuracy {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one student move's centipawn loss. Negative losses (the engine
    /// eval improved on its own best line, e.g. deeper search after the move)
    /// are clamped to zero so a lucky eval wobble can't inflate accuracy.
    pub fn record(&mut self, cp_loss: i32) {
        self.moves += 1;
        self.total_cp_loss += i64::from(cp_loss.max(0));
    }

    /// Number of student moves recorded so far.
    pub fn moves(&self) -> u32 {
        self.moves
    }

    /// Average centipawn loss (ACL). Zero before any move is recorded.
    pub fn avg_centipawn_loss(&self) -> f64 {
        if self.moves == 0 {
            0.0
        } else {
            self.total_cp_loss as f64 / f64::from(self.moves)
        }
    }

    /// Accuracy on a 0–100 scale, from ACL via a lichess-style exponential:
    /// `103 * exp(-0.005 * acl)`, clamped to 0..=100.
    ///
    /// Rationale: accuracy should be near-perfect (100) at ACL ≈ 0, decay
    /// smoothly and *monotonically* as ACL grows, and never go negative. The
    /// exponential does exactly that — the 103 factor keeps ACL ≤ ~6 pinned
    /// at 100 (engine-level play), and the 0.005 decay puts typical club-level
    /// ACL (~50) around 80%, beginner ACL (~150) around 49%. Exact constants
    /// are a product tuning knob, not a claim of psychometric truth.
    pub fn accuracy_percent(&self) -> f64 {
        let acl = self.avg_centipawn_loss();
        (103.0 * (-0.005 * acl).exp()).clamp(0.0, 100.0)
    }
}

/// Anchor table for the ACL → rating mapping: (ACL, estimated rating).
/// Sorted by ACL ascending; ratings strictly decreasing. Values are rough
/// community folklore (strong players hold ACL in the teens; beginners live
/// north of 150) — good enough to steer bot-level advancement, not a FIDE
/// certificate.
const RATING_ANCHORS: &[(f64, f64)] = &[
    (15.0, 2300.0),
    (30.0, 2000.0),
    (50.0, 1800.0),
    (75.0, 1500.0),
    (100.0, 1200.0),
    (150.0, 950.0),
    (200.0, 700.0),
    (300.0, 500.0),
];

/// Map a game's average centipawn loss to an estimated rating.
///
/// Monotonic decreasing: piecewise-linear interpolation over
/// [`RATING_ANCHORS`], flat below the first anchor and beyond the last, and
/// finally clamped to 400..=2400.
pub fn estimated_rating_from_acl(acl: f64) -> u32 {
    let first = RATING_ANCHORS[0];
    let last = RATING_ANCHORS[RATING_ANCHORS.len() - 1];
    let rating = if !acl.is_finite() || acl >= last.0 {
        last.1
    } else if acl <= first.0 {
        first.1
    } else {
        // Find the surrounding anchor pair and interpolate linearly.
        let mut rating = last.1;
        for pair in RATING_ANCHORS.windows(2) {
            let (a_acl, a_rating) = pair[0];
            let (b_acl, b_rating) = pair[1];
            if acl <= b_acl {
                let t = (acl - a_acl) / (b_acl - a_acl);
                rating = a_rating + t * (b_rating - a_rating);
                break;
            }
        }
        rating
    };
    (rating.round() as i64).clamp(400, 2400) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accuracy_starts_perfect_and_decays() {
        let mut g = GameAccuracy::new();
        assert_eq!(g.moves(), 0);
        assert_eq!(g.avg_centipawn_loss(), 0.0);
        assert_eq!(g.accuracy_percent(), 100.0);

        g.record(0);
        g.record(20);
        assert_eq!(g.moves(), 2);
        assert!((g.avg_centipawn_loss() - 10.0).abs() < 1e-9);
        assert!(g.accuracy_percent() > 95.0);
    }

    #[test]
    fn negative_cp_loss_is_clamped() {
        let mut g = GameAccuracy::new();
        g.record(-50);
        assert_eq!(g.avg_centipawn_loss(), 0.0);
    }

    #[test]
    fn accuracy_is_monotonic_in_acl() {
        let mut prev = f64::INFINITY;
        for acl_tenths in 0..=3000 {
            let mut g = GameAccuracy::new();
            g.record(acl_tenths); // one move → ACL == cp_loss
            let acc = g.accuracy_percent();
            assert!(
                acc <= prev + 1e-9,
                "accuracy not monotonic at acl {acl_tenths}"
            );
            assert!((0.0..=100.0).contains(&acc));
            prev = acc;
        }
    }

    #[test]
    fn rating_hits_anchor_points() {
        assert_eq!(estimated_rating_from_acl(15.0), 2300);
        assert_eq!(estimated_rating_from_acl(30.0), 2000);
        assert_eq!(estimated_rating_from_acl(50.0), 1800);
        assert_eq!(estimated_rating_from_acl(75.0), 1500);
        assert_eq!(estimated_rating_from_acl(100.0), 1200);
        assert_eq!(estimated_rating_from_acl(150.0), 950);
        assert_eq!(estimated_rating_from_acl(200.0), 700);
        assert_eq!(estimated_rating_from_acl(300.0), 500);
        assert_eq!(estimated_rating_from_acl(1000.0), 500);
    }

    #[test]
    fn rating_interpolates_between_anchors() {
        // Midpoint of (50, 1800) and (75, 1500) → 1650.
        assert_eq!(estimated_rating_from_acl(62.5), 1650);
    }

    #[test]
    fn rating_is_clamped_and_flat_outside_anchors() {
        assert_eq!(estimated_rating_from_acl(0.0), 2300);
        assert_eq!(estimated_rating_from_acl(f64::INFINITY), 500);
        assert_eq!(estimated_rating_from_acl(f64::NAN), 500);
    }

    #[test]
    fn rating_is_monotonic_decreasing() {
        let mut prev = u32::MAX;
        for acl_i in 0..=4000 {
            let acl = acl_i as f64 / 10.0;
            let r = estimated_rating_from_acl(acl);
            assert!(r <= prev, "rating not monotonic at acl {acl}");
            assert!((400..=2400).contains(&r));
            prev = r;
        }
    }
}
