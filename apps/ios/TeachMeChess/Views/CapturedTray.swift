import SwiftUI

/// One slim row of captured-piece glyphs hugging the board edge, with a
/// compact bold "+N" badge at the end when this row's side is ahead on
/// material. Same-piece runs read as a group because the glyphs overlap
/// slightly. Renders as nothing at all (zero height, no gap) while the row
/// has neither captures nor an advantage to show.
struct CapturedTrayRow: View {
    /// Piece letters exactly as the Rust `MaterialSummary` emits them
    /// (uppercase = white pieces, lowercase = black), already sorted
    /// most-valuable-first.
    let letters: [String]
    /// Pawn-unit advantage of the side that OWNS this row; the badge shows
    /// only when > 0.
    let advantage: Int

    var body: some View {
        if !letters.isEmpty || advantage > 0 {
            HStack(spacing: 4) {
                HStack(spacing: -4) {
                    ForEach(Array(letters.enumerated()), id: \.offset) { _, letter in
                        CapturedPieceGlyph(letter: letter)
                    }
                }
                if advantage > 0 {
                    Text("+\(advantage)")
                        .font(.caption2.bold().monospacedDigit())
                        .foregroundStyle(.secondary)
                }
            }
            .frame(height: 18)
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(accessibilityText)
        }
    }

    private var accessibilityText: String {
        let names = letters.map { CapturedPieceGlyph.pieceName($0) }.joined(separator: ", ")
        let captured = names.isEmpty ? "no pieces captured" : "captured \(names)"
        return advantage > 0 ? "\(captured), ahead by \(advantage)" : captured
    }
}

/// One small captured-piece glyph, tinted like the board's pieces so white
/// and black pieces stay tellable apart on any background.
struct CapturedPieceGlyph: View {
    /// Piece letter (uppercase = white piece, lowercase = black).
    let letter: String
    var size: CGFloat = 14

    var body: some View {
        let isWhite = letter.first?.isUppercase ?? false
        let piece = Piece(isWhite: isWhite, kind: Character(letter.lowercased()))
        Text(piece.glyph)
            .font(.system(size: size))
            .foregroundStyle(isWhite ? Color.white : Color.black)
            .shadow(color: isWhite ? .black.opacity(0.55) : .white.opacity(0.45), radius: 1)
    }

    static func pieceName(_ letter: String) -> String {
        switch letter.lowercased() {
        case "k": return "king"
        case "q": return "queen"
        case "r": return "rook"
        case "b": return "bishop"
        case "n": return "knight"
        default: return "pawn"
        }
    }
}
