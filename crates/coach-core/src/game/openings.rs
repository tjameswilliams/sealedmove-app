//! Opening identification from the move history.
//!
//! Backed by the full lichess `chess-openings` ECO database (~3,500 named
//! lines), vendored as TSVs in `data/openings/` (CC0 — see ATTRIBUTION.md
//! there) and embedded via `include_str!`. Parsed lazily on first lookup
//! into a hash map keyed by the space-joined SAN prefix, so a lookup is at
//! most `min(history_len, longest_book_line)` hash probes.

use std::collections::HashMap;
use std::sync::LazyLock;

/// A named opening matched against the game so far.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Opening {
    pub eco: String,
    pub name: String,
    /// How many plies of the game the book line covers.
    pub matched_plies: usize,
}

/// The raw lichess TSVs, embedded at compile time.
const RAW_TSVS: [&str; 5] = [
    include_str!("../../data/openings/a.tsv"),
    include_str!("../../data/openings/b.tsv"),
    include_str!("../../data/openings/c.tsv"),
    include_str!("../../data/openings/d.tsv"),
    include_str!("../../data/openings/e.tsv"),
];

struct Book {
    /// Space-joined SAN line -> (eco, name, plies).
    by_line: HashMap<String, (&'static str, &'static str, usize)>,
    /// Longest book line, in plies — bounds the prefix search.
    max_plies: usize,
}

static BOOK: LazyLock<Book> = LazyLock::new(|| {
    let mut by_line = HashMap::new();
    let mut max_plies = 0;
    for tsv in RAW_TSVS {
        for line in tsv.lines().skip(1) {
            if line.trim().is_empty() {
                continue;
            }
            let mut cols = line.split('\t');
            let (Some(eco), Some(name), Some(pgn)) =
                (cols.next(), cols.next(), cols.next())
            else {
                debug_assert!(false, "malformed opening TSV row: {line:?}");
                continue;
            };
            // `pgn` looks like "1. e4 e5 2. Nf3" — drop the move-number
            // tokens (they all end in '.'; SAN tokens never do).
            let san_line = pgn
                .split_whitespace()
                .filter(|tok| !tok.ends_with('.'))
                .collect::<Vec<_>>();
            let plies = san_line.len();
            if plies == 0 {
                continue;
            }
            max_plies = max_plies.max(plies);
            by_line.insert(san_line.join(" "), (eco, name, plies));
        }
    }
    Book { by_line, max_plies }
});

/// Number of distinct book lines (test/diagnostic aid).
#[cfg(test)]
fn book_len() -> usize {
    BOOK.by_line.len()
}

/// Longest-prefix match of the move history (SAN) against the book.
pub fn lookup(history_san: &[String]) -> Option<Opening> {
    let book = &*BOOK;
    let upper = history_san.len().min(book.max_plies);
    for plies in (1..=upper).rev() {
        let key = history_san[..plies].join(" ");
        if let Some(&(eco, name, matched_plies)) = book.by_line.get(&key) {
            return Some(Opening {
                eco: eco.to_string(),
                name: name.to_string(),
                matched_plies,
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hist(moves: &[&str]) -> Vec<String> {
        moves.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn full_database_is_loaded() {
        // The lichess DB has ~3,500 named lines; make sure we did not
        // silently regress to a stub.
        assert!(book_len() > 3000, "only {} book lines parsed", book_len());
    }

    #[test]
    fn finds_longest_match() {
        let opening = lookup(&hist(&["e4", "e5", "Nf3", "Nc6", "Bb5", "Nf6"])).unwrap();
        assert_eq!(opening.name, "Ruy Lopez: Berlin Defense");
        assert_eq!(opening.eco, "C65");
        assert_eq!(opening.matched_plies, 6);
    }

    #[test]
    fn partial_game_matches_shorter_line() {
        let opening = lookup(&hist(&["e4", "c5", "Nf3"])).unwrap();
        assert_eq!(opening.name, "Sicilian Defense");
        assert_eq!(opening.eco, "B27");
        assert_eq!(opening.matched_plies, 3);
    }

    #[test]
    fn unknown_line_is_none() {
        // Every legal first move is in the full DB, so a truly unknown
        // line needs an empty or nonsense history.
        assert!(lookup(&hist(&[])).is_none());
        assert!(lookup(&hist(&["Zz9", "Zz8"])).is_none());
    }

    #[test]
    fn offbook_continuation_keeps_prefix_match() {
        // "a3 h6" is not a named line, but "a3" is Anderssen's Opening —
        // longest-prefix semantics should surface it.
        let opening = lookup(&hist(&["a3", "h6"])).unwrap();
        assert_eq!(opening.name, "Anderssen's Opening");
        assert_eq!(opening.matched_plies, 1);
    }

    #[test]
    fn kings_indian_defense() {
        let opening = lookup(&hist(&["d4", "Nf6", "c4", "g6", "Nc3", "Bg7"])).unwrap();
        assert_eq!(opening.name, "King's Indian Defense");
        assert_eq!(opening.eco, "E61");
        // The DB's E61 line stops after 3. Nc3; ...Bg7 is off-book here.
        assert_eq!(opening.matched_plies, 5);
    }

    #[test]
    fn najdorf() {
        let opening = lookup(&hist(&[
            "e4", "c5", "Nf3", "d6", "d4", "cxd4", "Nxd4", "Nf6", "Nc3", "a6",
        ]))
        .unwrap();
        assert_eq!(opening.name, "Sicilian Defense: Najdorf Variation");
        assert_eq!(opening.eco, "B90");
        assert_eq!(opening.matched_plies, 10);
    }

    #[test]
    fn queens_gambit_declined() {
        let opening = lookup(&hist(&["d4", "d5", "c4", "e6"])).unwrap();
        assert_eq!(opening.name, "Queen's Gambit Declined");
        assert_eq!(opening.eco, "D30");
        assert_eq!(opening.matched_plies, 4);
    }
}
