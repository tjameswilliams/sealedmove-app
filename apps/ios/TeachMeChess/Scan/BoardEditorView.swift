import SwiftUI

/// Builds a full FEN out of editor state, and pulls editor state back out
/// of a recognized board field.
enum FenComposer {
    static let files = ["a", "b", "c", "d", "e", "f", "g", "h"]

    /// Compose a complete FEN. Castling rights are granted only where king
    /// and rook still stand on their home squares (the safe reading of a
    /// photo, which carries no history); en passant is unknowable from a
    /// still image, so it is always "-".
    static func fen(pieces: [String: Piece], whiteToMove: Bool) -> String {
        var ranks: [String] = []
        for rank in (1...8).reversed() {
            var row = ""
            var empty = 0
            for file in 0..<8 {
                let square = "\(files[file])\(rank)"
                if let piece = pieces[square] {
                    if empty > 0 {
                        row += String(empty)
                        empty = 0
                    }
                    let letter = String(piece.kind)
                    row += piece.isWhite ? letter.uppercased() : letter
                } else {
                    empty += 1
                }
            }
            if empty > 0 { row += String(empty) }
            ranks.append(row)
        }

        var castling = ""
        if pieces["e1"] == Piece(isWhite: true, kind: "k") {
            if pieces["h1"] == Piece(isWhite: true, kind: "r") { castling += "K" }
            if pieces["a1"] == Piece(isWhite: true, kind: "r") { castling += "Q" }
        }
        if pieces["e8"] == Piece(isWhite: false, kind: "k") {
            if pieces["h8"] == Piece(isWhite: false, kind: "r") { castling += "k" }
            if pieces["a8"] == Piece(isWhite: false, kind: "r") { castling += "q" }
        }
        if castling.isEmpty { castling = "-" }

        return "\(ranks.joined(separator: "/")) \(whiteToMove ? "w" : "b") \(castling) - 0 1"
    }

    /// Sanity problems worth naming before shakmaty's terser rejection.
    static func validationProblem(pieces: [String: Piece]) -> String? {
        let whiteKings = pieces.values.filter { $0.isWhite && $0.kind == "k" }.count
        let blackKings = pieces.values.filter { !$0.isWhite && $0.kind == "k" }.count
        if whiteKings != 1 || blackKings != 1 {
            return "Each side needs exactly one king."
        }
        let badPawn = pieces.first { square, piece in
            piece.kind == "p" && (square.hasSuffix("1") || square.hasSuffix("8"))
        }
        if let badPawn {
            return "There's a pawn on \(badPawn.key), and pawns can't stand on the first or last rank."
        }
        return nil
    }
}

/// Tappable position editor: pick a piece (or the eraser) from the palette,
/// tap squares to place it. Tapping a square that already holds the picked
/// piece clears the square.
struct BoardEditorView: View {
    @Binding var pieces: [String: Piece]

    /// What the next square tap does.
    private enum Tool: Equatable {
        case place(Piece)
        case erase
    }

    @State private var tool: Tool = .erase

    var body: some View {
        VStack(spacing: 10) {
            boardGrid
                .aspectRatio(1, contentMode: .fit)
                .clipShape(RoundedRectangle(cornerRadius: 8))
                .overlay(
                    RoundedRectangle(cornerRadius: 8)
                        .strokeBorder(Color.black.opacity(0.25), lineWidth: 1))
            palette
        }
    }

    private var boardGrid: some View {
        VStack(spacing: 0) {
            ForEach(Array((1...8).reversed()), id: \.self) { rank in
                HStack(spacing: 0) {
                    ForEach(0..<8, id: \.self) { file in
                        squareView(file: file, rank: rank)
                    }
                }
            }
        }
    }

    @ViewBuilder
    private func squareView(file: Int, rank: Int) -> some View {
        let name = "\(FenComposer.files[file])\(rank)"
        let isLight = (file + rank) % 2 == 0
        let piece = pieces[name]

        ZStack {
            Rectangle()
                .fill(isLight ? Brand.boardLight : Brand.boardDark)
            if let piece {
                GeometryReader { geo in
                    OutlinedPieceGlyph(
                        glyph: piece.glyph,
                        size: geo.size.width * 0.85,
                        isWhite: piece.isWhite)
                        .frame(width: geo.size.width, height: geo.size.height)
                }
            }
        }
        .contentShape(Rectangle())
        .onTapGesture { tap(square: name) }
    }

    private func tap(square: String) {
        switch tool {
        case .erase:
            pieces[square] = nil
        case .place(let piece):
            pieces[square] = pieces[square] == piece ? nil : piece
        }
    }

    private var palette: some View {
        VStack(spacing: 6) {
            paletteRow(isWhite: true)
            paletteRow(isWhite: false)
        }
    }

    private func paletteRow(isWhite: Bool) -> some View {
        HStack(spacing: 6) {
            ForEach(Array("kqrbnp"), id: \.self) { kind in
                paletteButton(.place(Piece(isWhite: isWhite, kind: kind)))
            }
            // One eraser per row keeps the rows the same width; both tiles
            // select the same tool.
            paletteButton(.erase)
        }
    }

    @ViewBuilder
    private func paletteButton(_ target: Tool) -> some View {
        let selected = tool == target
        Button {
            tool = target
        } label: {
            ZStack {
                RoundedRectangle(cornerRadius: 6)
                    .fill(selected ? Brand.tournamentGreen.opacity(0.25)
                                   : Color(.tertiarySystemFill))
                switch target {
                case .place(let piece):
                    OutlinedPieceGlyph(glyph: piece.glyph, size: 26, isWhite: piece.isWhite)
                case .erase:
                    Image(systemName: "xmark")
                        .font(.system(size: 15, weight: .semibold))
                        .foregroundStyle(.secondary)
                }
            }
            .frame(width: 38, height: 38)
            .overlay(
                RoundedRectangle(cornerRadius: 6)
                    .strokeBorder(selected ? Brand.tournamentGreen : .clear, lineWidth: 2))
        }
        .buttonStyle(.plain)
        .accessibilityLabel(paletteLabel(target))
    }

    private func paletteLabel(_ target: Tool) -> String {
        switch target {
        case .erase: return "Eraser"
        case .place(let piece):
            let color = piece.isWhite ? "White" : "Black"
            let names: [Character: String] = [
                "k": "king", "q": "queen", "r": "rook",
                "b": "bishop", "n": "knight", "p": "pawn",
            ]
            return "\(color) \(names[piece.kind] ?? "piece")"
        }
    }
}
