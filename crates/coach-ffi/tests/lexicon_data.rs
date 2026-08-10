//! Data guard for the iOS app's bundled chess lexicon.
//!
//! `apps/ios/TeachMeChess/Resources/Lexicon.json` ships curated openings
//! and concepts whose demo lines the app replays on the Rust board
//! (`BoardHandle`). A typo'd FEN or an illegal SAN would silently break
//! the animation at runtime — so every entry is replayed here through the
//! same `GameState` the app uses.

use coach_core::game::GameState;
use std::collections::HashSet;
use std::path::Path;

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    slug: String,
    name: String,
    #[serde(default)]
    start_fen: Option<String>,
    #[serde(default)]
    moves: Vec<String>,
}

fn load_entries() -> Vec<Entry> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../apps/ios/TeachMeChess/Resources/Lexicon.json");
    let json = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    serde_json::from_str(&json).expect("Lexicon.json parses as [Entry]")
}

#[test]
fn every_lexicon_demo_line_is_legal() {
    let entries = load_entries();
    assert!(!entries.is_empty(), "lexicon must not be empty");

    for entry in &entries {
        let mut state = match &entry.start_fen {
            Some(fen) => GameState::from_fen(fen).unwrap_or_else(|e| {
                panic!("{}: bad startFen {fen:?}: {e}", entry.slug)
            }),
            None => GameState::new(),
        };
        for (i, san) in entry.moves.iter().enumerate() {
            state.play_san(san).unwrap_or_else(|e| {
                panic!(
                    "{} ({}): illegal move {san:?} at ply {} of {:?}: {e}",
                    entry.slug,
                    entry.name,
                    i + 1,
                    entry.moves
                )
            });
        }
    }
}

#[test]
fn lexicon_slugs_are_unique() {
    let entries = load_entries();
    let mut seen = HashSet::new();
    for entry in &entries {
        assert!(
            seen.insert(entry.slug.clone()),
            "duplicate lexicon slug {:?}",
            entry.slug
        );
    }
}
