import SwiftUI

/// 8x8 tappable chess board with coordinate labels, selection + legal-move
/// dots, and last-move highlighting. White at the bottom by default.
struct BoardView: View {
    let model: GameViewModel
    var flipped = false
    /// The compact context strip renders the board as a small thumbnail —
    /// the "Opponent is thinking…" capsule would overflow it, so the strip
    /// shows its own indicator instead.
    var showsThinkingOverlay = true

    private static let files = ["a", "b", "c", "d", "e", "f", "g", "h"]

    private var rankOrder: [Int] { flipped ? Array(1...8) : Array((1...8).reversed()) }
    private var fileOrder: [Int] { flipped ? Array((0..<8).reversed()) : Array(0..<8) }

    var body: some View {
        VStack(spacing: 0) {
            ForEach(rankOrder, id: \.self) { rank in
                HStack(spacing: 0) {
                    ForEach(fileOrder, id: \.self) { file in
                        squareView(file: file, rank: rank)
                    }
                }
            }
        }
        .aspectRatio(1, contentMode: .fit)
        .clipShape(RoundedRectangle(cornerRadius: 8))
        .overlay(
            RoundedRectangle(cornerRadius: 8)
                .strokeBorder(Color.black.opacity(0.25), lineWidth: 1)
        )
        // Coach-spotlight arrows (tapped move mentions in the feed).
        .overlay(ArrowOverlay(arrows: model.spotlightArrows, flipped: flipped))
        .overlay(alignment: .top) {
            if showsThinkingOverlay && model.isBotThinking {
                HStack(spacing: 6) {
                    ProgressView().controlSize(.mini)
                    Text("Opponent is thinking…")
                        .font(.caption2)
                }
                .padding(.horizontal, 10)
                .padding(.vertical, 5)
                .background(.thinMaterial, in: Capsule())
                .padding(.top, 8)
            }
        }
    }

    /// Chip glyph + color for an engine judgment, chess-annotation style.
    static func verdictStyle(_ judgment: String) -> (symbol: String, color: Color) {
        switch judgment {
        case "best": return ("★", Brand.tournamentGreen)
        case "excellent": return ("!", Brand.tournamentGreen)
        case "good": return ("✓", .green)
        case "inaccuracy": return ("?!", .yellow)
        case "mistake": return ("?", .orange)
        case "blunder": return ("??", Brand.annotation)
        default: return ("·", .gray)
        }
    }

    private func squareName(file: Int, rank: Int) -> String {
        "\(Self.files[file])\(rank)"
    }

    @ViewBuilder
    private func squareView(file: Int, rank: Int) -> some View {
        let name = squareName(file: file, rank: rank)
        // a1 dark, h1 light: even file+rank (0-indexed file, 1-indexed rank)
        // is a light square.
        let isLight = (file + rank) % 2 == 0
        let piece = model.pieces[name]
        let isSelected = model.selectedSquare == name
        let isLegalTarget = model.legalTargets.contains(name)
        let isLastMove = model.lastMoveSquares.contains(name)
        let isSpotlit = model.spotlightSquares.contains(name)

        ZStack {
            Rectangle()
                .fill(isLight ? Brand.boardLight : Brand.boardDark)
            if isLastMove {
                Rectangle().fill(Color.yellow.opacity(0.35))
            }
            if isSelected {
                Rectangle().fill(Color.blue.opacity(0.35))
            }
            if isSpotlit {
                SpotlightSquare()
            }

            // Coordinate labels on the board edge squares.
            GeometryReader { geo in
                let labelColor = isLight ? Brand.boardDark : Brand.boardLight
                ZStack(alignment: .topLeading) {
                    Color.clear
                    if file == (flipped ? 7 : 0) {
                        Text("\(rank)")
                            .font(.system(size: geo.size.width * 0.22, weight: .semibold))
                            .foregroundStyle(labelColor)
                            .padding(2)
                    }
                }
                ZStack(alignment: .bottomTrailing) {
                    Color.clear
                    if rank == (flipped ? 8 : 1) {
                        Text(Self.files[file])
                            .font(.system(size: geo.size.width * 0.22, weight: .semibold))
                            .foregroundStyle(labelColor)
                            .padding(2)
                    }
                }
            }

            if let piece {
                GeometryReader { geo in
                    OutlinedPieceGlyph(
                        glyph: piece.glyph,
                        size: geo.size.width * 0.85,
                        isWhite: piece.isWhite
                    )
                    .frame(width: geo.size.width, height: geo.size.height)
                }
            }

            // Verdict chip on the last judged student move.
            if model.verdictSquare == name, let judgment = model.verdictJudgment {
                let style = Self.verdictStyle(judgment)
                GeometryReader { geo in
                    ZStack(alignment: .topTrailing) {
                        Color.clear
                        Text(style.symbol)
                            .font(.system(size: geo.size.width * 0.26, weight: .heavy))
                            .foregroundStyle(.white)
                            .frame(width: geo.size.width * 0.42, height: geo.size.width * 0.42)
                            .background(Circle().fill(style.color))
                            .overlay(Circle().strokeBorder(.white.opacity(0.8), lineWidth: 1))
                            .padding(1)
                    }
                }
            }

            if isLegalTarget {
                GeometryReader { geo in
                    Group {
                        if piece != nil {
                            // Capture: ring around the piece.
                            Circle()
                                .strokeBorder(Color.black.opacity(0.35),
                                              lineWidth: geo.size.width * 0.08)
                        } else {
                            Circle()
                                .fill(Color.black.opacity(0.25))
                                .padding(geo.size.width * 0.36)
                        }
                    }
                    .frame(width: geo.size.width, height: geo.size.height)
                }
            }
        }
        .contentShape(Rectangle())
        .onTapGesture { model.tap(square: name) }
        .accessibilityLabel(Text(accessibilityText(name: name, piece: piece)))
    }

    private func accessibilityText(name: String, piece: Piece?) -> String {
        guard let piece else { return name }
        let color = piece.isWhite ? "white" : "black"
        let role: String
        switch piece.kind {
        case "k": role = "king"
        case "q": role = "queen"
        case "r": role = "rook"
        case "b": role = "bishop"
        case "n": role = "knight"
        default: role = "pawn"
        }
        return "\(name), \(color) \(role)"
    }
}

/// Pulsing annotation-red highlight for a square the coach's message points
/// at (tapped piece/square mention in the feed).
private struct SpotlightSquare: View {
    var body: some View {
        GeometryReader { geo in
            ZStack {
                Rectangle().fill(Brand.annotation.opacity(0.30))
                RoundedRectangle(cornerRadius: geo.size.width * 0.12)
                    .strokeBorder(Brand.annotation, lineWidth: max(2, geo.size.width * 0.06))
                    .padding(geo.size.width * 0.04)
                    .phaseAnimator([false, true]) { view, pulsing in
                        view
                            .opacity(pulsing ? 0.35 : 1)
                            .scaleEffect(pulsing ? 1.06 : 1)
                    } animation: { _ in
                        .easeInOut(duration: 0.7)
                    }
            }
        }
    }
}

/// A chess-piece glyph with a high-contrast halo outline so pieces stay
/// legible on both light and dark squares: white pieces get a dark outline,
/// black pieces a light one. The outline is drawn as eight offset copies of
/// the glyph behind the fill — SwiftUI `Text` has no native stroke.
private struct OutlinedPieceGlyph: View {
    let glyph: String
    let size: CGFloat
    let isWhite: Bool

    private static let directions: [CGPoint] = [
        CGPoint(x: 1, y: 0), CGPoint(x: -1, y: 0),
        CGPoint(x: 0, y: 1), CGPoint(x: 0, y: -1),
        CGPoint(x: 0.7, y: 0.7), CGPoint(x: -0.7, y: 0.7),
        CGPoint(x: 0.7, y: -0.7), CGPoint(x: -0.7, y: -0.7),
    ]

    var body: some View {
        let outlineWidth = max(1, size * 0.035)
        let fill: Color = isWhite ? .white : .black
        let outline: Color = isWhite ? .black.opacity(0.85) : .white.opacity(0.8)
        ZStack {
            ForEach(0..<Self.directions.count, id: \.self) { i in
                Text(glyph)
                    .font(.system(size: size))
                    .foregroundStyle(outline)
                    .offset(
                        x: Self.directions[i].x * outlineWidth,
                        y: Self.directions[i].y * outlineWidth
                    )
            }
            Text(glyph)
                .font(.system(size: size))
                .foregroundStyle(fill)
        }
        .shadow(color: .black.opacity(0.35), radius: 1.5, y: 1)
        .minimumScaleFactor(0.5)
    }
}
