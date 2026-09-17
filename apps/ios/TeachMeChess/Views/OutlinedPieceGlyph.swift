import SwiftUI

/// A chess-piece glyph with a high-contrast halo outline so pieces stay
/// legible on both light and dark squares: white pieces get a dark outline,
/// black pieces a light one. The outline is drawn as eight offset copies of
/// the glyph behind the fill — SwiftUI `Text` has no native stroke.
///
/// Shared by the live board, every replay surface, and the video exporter's
/// frames, so pieces read the same everywhere.
struct OutlinedPieceGlyph: View {
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
