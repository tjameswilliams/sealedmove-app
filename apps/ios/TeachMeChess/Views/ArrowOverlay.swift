import SwiftUI

/// Animated move arrows drawn over a board. Sized by the enclosing board
/// frame (assumed square, 8x8); each arrow springs from its source square
/// toward its destination when it first appears.
struct ArrowOverlay: View {
    let arrows: [BoardArrow]
    var flipped = false

    var body: some View {
        GeometryReader { geo in
            let square = geo.size.width / 8
            ForEach(arrows) { arrow in
                if let from = Self.center(of: arrow.from, square: square, flipped: flipped),
                   let to = Self.center(of: arrow.to, square: square, flipped: flipped) {
                    AnimatedArrow(from: from, to: to, squareSize: square)
                }
            }
        }
        .allowsHitTesting(false)
    }

    /// Center point of an algebraic square in board coordinates.
    static func center(of name: String, square: CGFloat, flipped: Bool) -> CGPoint? {
        guard name.count == 2,
              let fileChar = name.first, let rankChar = name.last,
              let file = "abcdefgh".firstIndex(of: fileChar)
                  .map({ "abcdefgh".distance(from: "abcdefgh".startIndex, to: $0) }),
              let rank = rankChar.wholeNumberValue, (1...8).contains(rank)
        else { return nil }
        let col = flipped ? 7 - file : file
        let row = flipped ? rank - 1 : 8 - rank
        return CGPoint(
            x: (CGFloat(col) + 0.5) * square,
            y: (CGFloat(row) + 0.5) * square)
    }
}

/// One arrow that draws itself in (grows from source to destination with a
/// slight spring) on appear.
private struct AnimatedArrow: View {
    let from: CGPoint
    let to: CGPoint
    let squareSize: CGFloat

    @State private var progress: CGFloat = 0

    var body: some View {
        ArrowShape(progress: progress, from: from, to: to, squareSize: squareSize)
            .fill(Brand.annotation.opacity(0.88))
            .overlay(
                ArrowShape(progress: progress, from: from, to: to, squareSize: squareSize)
                    .stroke(Color.white.opacity(0.9), lineWidth: 1.5)
            )
            .shadow(color: .black.opacity(0.3), radius: 2, y: 1)
            .onAppear {
                withAnimation(.spring(response: 0.45, dampingFraction: 0.75)) {
                    progress = 1
                }
            }
    }
}

/// Chess-board arrow polygon: a shaft plus a triangular head, growing along
/// the from->to vector as `progress` animates 0 -> 1.
struct ArrowShape: Shape {
    var progress: CGFloat
    let from: CGPoint
    let to: CGPoint
    let squareSize: CGFloat

    var animatableData: CGFloat {
        get { progress }
        set { progress = newValue }
    }

    func path(in _: CGRect) -> Path {
        var path = Path()
        let dx = to.x - from.x
        let dy = to.y - from.y
        let fullLength = sqrt(dx * dx + dy * dy)
        guard fullLength > 1, progress > 0.01 else { return path }

        // Start slightly off-center so the piece glyph stays readable.
        let unit = CGVector(dx: dx / fullLength, dy: dy / fullLength)
        let normal = CGVector(dx: -unit.dy, dy: unit.dx)
        let startInset = squareSize * 0.32
        let start = CGPoint(x: from.x + unit.dx * startInset,
                            y: from.y + unit.dy * startInset)
        let drawnLength = max((fullLength - startInset) * progress, 0.1)
        let tip = CGPoint(x: start.x + unit.dx * drawnLength,
                          y: start.y + unit.dy * drawnLength)

        let headLength = min(squareSize * 0.38, drawnLength)
        let headHalfWidth = squareSize * 0.24
        let shaftHalfWidth = squareSize * 0.11
        let headBase = CGPoint(x: tip.x - unit.dx * headLength,
                               y: tip.y - unit.dy * headLength)

        func offset(_ p: CGPoint, _ scale: CGFloat) -> CGPoint {
            CGPoint(x: p.x + normal.dx * scale, y: p.y + normal.dy * scale)
        }

        path.move(to: offset(start, shaftHalfWidth))
        path.addLine(to: offset(headBase, shaftHalfWidth))
        path.addLine(to: offset(headBase, headHalfWidth))
        path.addLine(to: tip)
        path.addLine(to: offset(headBase, -headHalfWidth))
        path.addLine(to: offset(headBase, -shaftHalfWidth))
        path.addLine(to: offset(start, -shaftHalfWidth))
        path.closeSubpath()
        return path
    }
}
