//! SAN → spoken English, for the TTS pipeline. "Nf3" read aloud is garbage;
//! this turns it into "knight to f3". Deterministic, so it lives in the core —
//! the LLM is *prompted* to write speakable prose in voice mode, but move
//! names embedded in board updates always come through here.

/// Render a SAN move as speakable English.
///
/// Handles castling, captures, disambiguation, promotion, check and mate
/// suffixes, and annotation glyphs (`!`, `?`).
pub fn speak_san(san: &str) -> String {
    let mut s = san.trim();

    // Strip annotation glyphs (Nf3!?, e4!!).
    while let Some(stripped) = s.strip_suffix(['!', '?']) {
        s = stripped;
    }

    let suffix = if let Some(stripped) = s.strip_suffix('#') {
        s = stripped;
        ", checkmate"
    } else if let Some(stripped) = s.strip_suffix('+') {
        s = stripped;
        ", check"
    } else {
        ""
    };

    let core = match s {
        "O-O" | "0-0" => "castles kingside".to_string(),
        "O-O-O" | "0-0-0" => "castles queenside".to_string(),
        _ => speak_standard(s),
    };
    format!("{core}{suffix}")
}

fn piece_name(c: char) -> Option<&'static str> {
    match c {
        'K' => Some("king"),
        'Q' => Some("queen"),
        'R' => Some("rook"),
        'B' => Some("bishop"),
        'N' => Some("knight"),
        _ => None,
    }
}

fn speak_standard(s: &str) -> String {
    // Split off promotion: "e8=Q" / "exd8=Q".
    let (body, promotion) = match s.split_once('=') {
        Some((body, promo)) => {
            let name = promo
                .chars()
                .next()
                .and_then(piece_name)
                .unwrap_or("queen");
            (body, Some(name))
        }
        None => (s, None),
    };

    let chars: Vec<char> = body.chars().collect();
    if chars.len() < 2 {
        return body.to_string(); // unparseable; speak as-is
    }

    // Piece letter, if any.
    let (piece, rest) = match piece_name(chars[0]) {
        Some(name) => (name, &chars[1..]),
        None => ("pawn", &chars[..]),
    };

    // Target square = final two chars; everything before it (minus 'x') is a
    // disambiguator ("Nbd2" → 'b', "R1e2" → '1', "exd5" → 'e').
    let capture = rest.contains(&'x');
    let target: String = rest[rest.len().saturating_sub(2)..].iter().collect();
    let disamb: String = rest[..rest.len().saturating_sub(2)]
        .iter()
        .filter(|c| **c != 'x')
        .collect();

    let mut out = String::new();
    if piece == "pawn" && capture && !disamb.is_empty() {
        // "exd5" — the disambiguator is the pawn's file.
        out.push_str(&format!("{disamb}-pawn takes on {target}"));
    } else {
        out.push_str(piece);
        if !disamb.is_empty() {
            out.push_str(&format!(" on {disamb}"));
        }
        if capture {
            out.push_str(&format!(" takes on {target}"));
        } else {
            out.push_str(&format!(" to {target}"));
        }
    }

    if let Some(promo) = promotion {
        out.push_str(&format!(", promoting to a {promo}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speaks_common_moves() {
        assert_eq!(speak_san("e4"), "pawn to e4");
        assert_eq!(speak_san("Nf3"), "knight to f3");
        assert_eq!(speak_san("exd5"), "e-pawn takes on d5");
        assert_eq!(speak_san("Bxe5+"), "bishop takes on e5, check");
        assert_eq!(speak_san("O-O"), "castles kingside");
        assert_eq!(speak_san("O-O-O+"), "castles queenside, check");
        assert_eq!(speak_san("Qh4#"), "queen to h4, checkmate");
    }

    #[test]
    fn speaks_disambiguation_and_promotion() {
        assert_eq!(speak_san("Nbd2"), "knight on b to d2");
        assert_eq!(speak_san("R1e2"), "rook on 1 to e2");
        assert_eq!(speak_san("e8=Q"), "pawn to e8, promoting to a queen");
        assert_eq!(
            speak_san("exd8=N+"),
            "e-pawn takes on d8, promoting to a knight, check"
        );
    }

    #[test]
    fn strips_annotation_glyphs() {
        assert_eq!(speak_san("Nf3!?"), "knight to f3");
    }
}
