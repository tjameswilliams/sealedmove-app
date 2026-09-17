import Foundation

/// The engine's answer to a scanned position: the principal variation
/// replayed into SAN, with everything the export sheet needs.
struct SolutionLine: Equatable {
    /// FEN the solution starts from.
    let startingFen: String
    /// The line in SAN, first move first.
    let sanMoves: [String]
    /// FEN after i plies; fens[0] is the starting position.
    let fens: [String]
    /// "34. Qg4+" / "34… Kh8" labels honouring the FEN's move number.
    let moveLabels: [String]
    /// "White mates in 3" / "White is winning by 5.2".
    let headline: String
    let depth: UInt32
    let whiteToMove: Bool

    /// Title-card subtitle for the exported video.
    var subtitle: String {
        "Scanned puzzle · \(whiteToMove ? "White" : "Black") to move"
    }
}

/// Decodes `analyzeFen`'s JSON and turns the top line into a `SolutionLine`.
/// Pure: the blocking engine call happens on the caller's queue.
enum PuzzleSolver {
    /// Mirrors coach-core's `Analysis` serialization.
    private struct Analysis: Decodable {
        let bestMove: String
        let lines: [ScoredLine]

        private enum CodingKeys: String, CodingKey {
            case bestMove = "best_move"
            case lines
        }
    }

    private struct ScoredLine: Decodable {
        let multipv: UInt32
        let depth: UInt32
        let score: Score
        let pv: [String]
    }

    /// `{"kind": "cp"|"mate", "value": N}`, from the side to move's view.
    private struct Score: Decodable {
        let kind: String
        let value: Int
    }

    enum SolveError: LocalizedError {
        case emptyAnalysis
        case badReply
        case engineUnavailable

        var errorDescription: String? {
            switch self {
            case .emptyAnalysis:
                return "The engine found no move here. The position may already be over."
            case .badReply:
                return "The engine's analysis came back in a shape I couldn't read."
            case .engineUnavailable:
                return "The engine session isn't ready yet."
            }
        }
    }

    /// A quiet line runs long past the point anyone watches — cap it. A
    /// mate line always plays out in full.
    private static let quietLineCap = 10

    static func solution(fromAnalysisJson json: String, fen: String) throws -> SolutionLine {
        guard let first = try solutions(fromAnalysisJson: json, fen: fen).first else {
            throw SolveError.emptyAnalysis
        }
        return first
    }

    /// All MultiPV lines of an analysis, best first. Lines that fail to
    /// replay are dropped rather than failing the batch.
    static func solutions(fromAnalysisJson json: String, fen: String) throws -> [SolutionLine] {
        let decoder = JSONDecoder()
        guard let analysis = try? decoder.decode(Analysis.self, from: Data(json.utf8)) else {
            throw SolveError.badReply
        }
        guard !analysis.lines.isEmpty else { throw SolveError.emptyAnalysis }
        return analysis.lines
            .sorted { $0.multipv < $1.multipv }
            .compactMap { try? line($0, fen: fen) }
    }

    private static func line(_ top: ScoredLine, fen: String) throws -> SolutionLine {
        guard !top.pv.isEmpty else { throw SolveError.emptyAnalysis }
        guard let board = try? BoardHandle.fromFen(fen: fen) else {
            throw SolveError.badReply
        }

        let whiteToMove = GameViewModel.fenIsWhiteToMove(fen)
        let isMate = top.score.kind == "mate"
        let cappedPv = isMate ? top.pv : Array(top.pv.prefix(quietLineCap))

        var sanMoves: [String] = []
        var fens = [fen]
        var moveLabels: [String] = []
        var moverIsWhite = whiteToMove
        var moveNumber = Self.fullmoveNumber(of: fen)
        for uci in cappedPv {
            // A PV move the board rejects truncates the line (promotion
            // encodings and engine quirks) — what replayed still stands.
            guard let san = try? board.playUci(uci: uci) else { break }
            sanMoves.append(san)
            fens.append(board.fen())
            moveLabels.append(moverIsWhite ? "\(moveNumber). \(san)" : "\(moveNumber)… \(san)")
            if !moverIsWhite { moveNumber += 1 }
            moverIsWhite.toggle()
        }
        guard !sanMoves.isEmpty else { throw SolveError.emptyAnalysis }

        return SolutionLine(
            startingFen: fen,
            sanMoves: sanMoves,
            fens: fens,
            moveLabels: moveLabels,
            headline: headline(score: top.score, whiteToMove: whiteToMove),
            depth: top.depth,
            whiteToMove: whiteToMove)
    }

    private static func fullmoveNumber(of fen: String) -> Int {
        let fields = fen.split(separator: " ")
        guard fields.count >= 6, let n = Int(fields[5]), n >= 1 else { return 1 }
        return n
    }

    /// The score arrives from the side to move's perspective; the headline
    /// speaks in colors.
    private static func headline(score: Score, whiteToMove: Bool) -> String {
        let mover = whiteToMove ? "White" : "Black"
        let other = whiteToMove ? "Black" : "White"
        if score.kind == "mate" {
            return score.value > 0
                ? "\(mover) mates in \(score.value)"
                : "\(other) mates in \(-score.value)"
        }
        let whiteCp = whiteToMove ? score.value : -score.value
        let pawns = Double(abs(whiteCp)) / 100.0
        let leader = whiteCp >= 0 ? "White" : "Black"
        if pawns < 0.6 { return "A level position" }
        let verb = pawns >= 3 ? "is winning" : "is better"
        return String(format: "%@ %@ by %.1f", leader, verb, pawns)
    }
}
