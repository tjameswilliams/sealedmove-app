import Foundation

/// A named opening matched against the game so far, decoded from
/// `currentOpening` / a review's `opening` field.
struct OpeningInfo: Decodable, Equatable {
    let eco: String
    let name: String
    /// How many plies of the game the book line covers.
    let matchedPlies: Int

    /// Board-title form: the family name without the variation tail, so the
    /// status strip stays one line ("Sicilian Defense", not "Sicilian
    /// Defense: Najdorf Variation, English Attack").
    var shortName: String {
        name.split(separator: ":").first.map(String.init) ?? name
    }
}

/// Why a review moment earned its place. Mirrors coach-core's `MomentKind`.
enum ReviewMomentKind: String, Decodable {
    case turningPoint = "turning_point"
    case slip
    case strength
    case gift

    var label: String {
        switch self {
        case .turningPoint: return "Turning point"
        case .slip: return "Slip"
        case .strength: return "Strength"
        case .gift: return "Their slip"
        }
    }

    var symbol: String {
        switch self {
        case .turningPoint: return "arrow.triangle.branch"
        case .slip: return "exclamationmark.triangle"
        case .strength: return "star"
        case .gift: return "gift"
        }
    }
}

/// One anchored note in a game review. Every field but `note` is engine
/// output; the coach only writes the prose.
struct ReviewMomentInfo: Decodable, Identifiable, Equatable {
    /// 0-based ply of the move this note is about.
    let ply: Int
    let san: String
    let byStudent: Bool
    let kind: ReviewMomentKind
    /// Position before the move, so the sheet can replay into it.
    let fenBefore: String
    /// Position after the move: where the board lands.
    let fenAfter: String
    let evalBeforeCp: Int
    let evalAfterCp: Int
    let cpLoss: Int
    let judgment: String?
    let bestSan: String?
    let bestLineSan: [String]
    let motifs: [String]
    let headline: String
    let note: String

    var id: Int { ply }

    /// 1-based ply, matching how the rest of the app counts moves.
    var displayPly: Int { ply + 1 }

    /// "12. Nf3" / "12… Nc6".
    var moveLabel: String {
        let number = ply / 2 + 1
        return ply.isMultiple(of: 2) ? "\(number). \(san)" : "\(number)… \(san)"
    }

    /// Eval in pawns, from the student's side, signed.
    static func pawns(_ cp: Int) -> String {
        String(format: "%+.2f", Double(cp) / 100.0)
    }
}

/// A finished whole-game review, decoded from `reviewGame` / `reviewMoves`.
struct GameReviewInfo: Decodable, Equatable {
    let headline: String
    let summary: String
    let strengths: [String]
    let weaknesses: [String]
    let accuracy: Double
    let acl: Double
    let estRating: UInt32
    let movesJudged: UInt32
    let opening: OpeningInfo?
    let bookPlies: Int
    let moments: [ReviewMomentInfo]
    /// False when the coach model was unavailable and every line is the
    /// engine's own deterministic wording.
    let narrated: Bool

    /// The move list the review was built from — not part of the Rust
    /// payload, filled in by whoever requested the review so the sheet can
    /// replay the game.
    var moves: [String] = []

    private enum CodingKeys: String, CodingKey {
        case headline, summary, strengths, weaknesses, accuracy, acl
        case estRating, movesJudged, opening, bookPlies, moments, narrated
    }
}
