//! The app's lexicon ships a demo line per entry, played out on an
//! animated board in the term sheet. A wrong FEN or an illegal SAN there
//! fails silently at runtime — the board simply stops partway through the
//! line — so nothing in the app would tell us the content broke.
//!
//! This test replays every entry through the same board layer the app
//! uses. It reaches across into `apps/ios` on purpose: the content is the
//! thing under test, and this crate owns the only chess engine that can
//! check it.

use coach_core::game::GameState;
use shakmaty::Position;
use std::path::PathBuf;

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    slug: String,
    name: String,
    category: String,
    eco: Option<String>,
    #[serde(default)]
    definition: String,
    #[serde(default)]
    start_fen: Option<String>,
    #[serde(default)]
    moves: Vec<String>,
}

fn lexicon() -> Vec<Entry> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../apps/ios/TeachMeChess/Resources/Lexicon.json");
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parsing lexicon: {e}"))
}

#[test]
fn every_demo_line_replays_legally() {
    let mut failures = Vec::new();
    for entry in lexicon() {
        let mut game = match &entry.start_fen {
            Some(fen) => match GameState::from_fen(fen) {
                Ok(g) => g,
                Err(e) => {
                    failures.push(format!("{}: bad startFen ({e})", entry.slug));
                    continue;
                }
            },
            None => GameState::new(),
        };
        for (i, san) in entry.moves.iter().enumerate() {
            if let Err(e) = game.play_san(san) {
                failures.push(format!(
                    "{}: move {} ({san}) is illegal here: {e}",
                    entry.slug,
                    i + 1
                ));
                break;
            }
        }
    }
    assert!(failures.is_empty(), "lexicon demo lines:\n  {}", failures.join("\n  "));
}

/// SAN decorations are claims about the position, and the board layer
/// accepts them without checking. A caption promising mate over a line that
/// merely gives check teaches the pattern wrong.
#[test]
fn mate_and_check_marks_are_honest() {
    let mut failures = Vec::new();
    for entry in lexicon() {
        let mut game = match &entry.start_fen {
            Some(fen) => match GameState::from_fen(fen) {
                Ok(g) => g,
                Err(_) => continue, // Reported by the replay test.
            },
            None => GameState::new(),
        };
        for san in &entry.moves {
            if game.play_san(san).is_err() {
                break;
            }
            let pos = game.position();
            let in_check = pos.is_check();
            if san.ends_with('#') && !pos.is_checkmate() {
                failures.push(format!("{}: {san} is written as mate but is not", entry.slug));
            } else if san.ends_with('+') && !in_check {
                failures.push(format!("{}: {san} is written as check but is not", entry.slug));
            } else if !san.ends_with('+') && !san.ends_with('#') && in_check {
                failures.push(format!("{}: {san} gives check but is not marked", entry.slug));
            }
        }
    }
    assert!(failures.is_empty(), "lexicon SAN marks:\n  {}", failures.join("\n  "));
}

#[test]
fn entries_are_well_formed_and_unique() {
    let entries = lexicon();
    let mut seen = std::collections::HashSet::new();
    let mut problems = Vec::new();
    for entry in &entries {
        if !seen.insert(entry.slug.clone()) {
            problems.push(format!("{}: duplicate slug", entry.slug));
        }
        if entry.name.trim().is_empty() {
            problems.push(format!("{}: empty name", entry.slug));
        }
        if entry.definition.len() < 40 {
            problems.push(format!("{}: definition is too thin to teach anything", entry.slug));
        }
        if !["opening", "tactic", "concept"].contains(&entry.category.as_str()) {
            problems.push(format!("{}: unknown category {}", entry.slug, entry.category));
        }
        if entry.category == "opening" && entry.eco.is_none() {
            problems.push(format!("{}: opening with no ECO code", entry.slug));
        }
        // An entry with no demo line has nothing to show on the sheet.
        if entry.moves.is_empty() && entry.start_fen.is_none() {
            problems.push(format!("{}: no demo position or line", entry.slug));
        }
    }
    assert!(problems.is_empty(), "lexicon entries:\n  {}", problems.join("\n  "));
}

/// Every motif the detectors can report names a lexicon slug; a motif whose
/// slug has no entry is a dead link in the coach's prose.
#[test]
fn every_motif_kind_has_a_lexicon_entry() {
    use coach_core::game::motifs::MotifKind::*;
    let slugs: std::collections::HashSet<String> =
        lexicon().into_iter().map(|e| e.slug).collect();
    let kinds = [
        DoubleCheck,
        HangingPiece,
        Fork,
        Pin,
        Skewer,
        TrappedPiece,
        RemovingTheDefender,
        OverloadedDefender,
        BackRankWeakness,
        SmotheredKing,
        DiscoveredAttackAvailable,
        Battery,
        PassedPawn,
        OutsidePassedPawn,
        Outpost,
        BishopPair,
        IsolatedPawn,
        DoubledPawns,
        BackwardPawn,
        Opposition,
        LucenaPosition,
        PhilidorPosition,
    ];
    let missing: Vec<&str> = kinds
        .iter()
        .map(|k| k.slug())
        .filter(|s| !slugs.contains(*s))
        .collect();
    assert!(missing.is_empty(), "motifs with no lexicon entry: {missing:?}");
}
