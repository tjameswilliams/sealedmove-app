import Foundation

/// A `ForeignCoachModel` with canned, level-appropriate responses.
///
/// This is the Swift-implements-the-Rust-backend path (the generated
/// `ForeignCoachModel` protocol) proven end-to-end without any real model.
/// It answers the same `{system, messages, tools}` -> `{text}` JSON wire
/// contract the Rust side speaks, so swapping in a real backend is purely a
/// change of implementation.
final class CannedCoachModel: ForeignCoachModel, @unchecked Sendable {
    private let lock = NSLock()
    private var callCount = 0

    private static let openingLines = [
        "Good start! In the opening, try to control the center and develop your pieces.",
        "Nice. Remember: knights and bishops out early, castle soon, don't move the same piece twice.",
        "Solid. Keep an eye on the center squares — e4, d4, e5, d5 are prime real estate.",
    ]

    private static let middlegameLines = [
        "Interesting choice! Before every move, check: is anything of mine hanging? Is anything of theirs?",
        "Keep looking for checks, captures, and threats — in that order.",
        "Think about what your opponent wants to do next, not just your own plan.",
        "Good. Try to improve your worst-placed piece when you're not sure what to do.",
    ]

    private static let endgameLines = [
        "We're deep into the game now — activate your king, it's a fighting piece in the endgame!",
        "Passed pawns must be pushed! Look for chances to promote.",
        "In the endgame, every tempo counts. Precise moves win half-points.",
    ]

    // MARK: - ForeignCoachModel

    func supportsTools() -> Bool { false }

    func contextTokens() -> UInt32 { 4096 }

    func compactTier() -> Bool { true }

    func complete(requestJson: String) throws -> String {
        // Decode enough of the request to pick a phase-appropriate line.
        struct WireMessage: Decodable { let role: String; let content: String }
        struct WireRequest: Decodable { let system: String?; let messages: [WireMessage]? }
        let request = try? JSONDecoder().decode(WireRequest.self, from: Data(requestJson.utf8))
        let userText = request?.messages?.last?.content ?? ""

        // Rough game phase from the SAN history embedded in the prompt.
        let moveCount = userText.split(separator: " ").filter { token in
            token.first.map { "abcdefghKQRBNO".contains($0) } ?? false
        }.count

        let pool: [String]
        if moveCount <= 12 {
            pool = Self.openingLines
        } else if moveCount <= 40 {
            pool = Self.middlegameLines
        } else {
            pool = Self.endgameLines
        }

        lock.lock()
        let index = callCount % pool.count
        callCount += 1
        lock.unlock()

        let reply = ["text": pool[index]]
        let data = try JSONSerialization.data(withJSONObject: reply)
        guard let json = String(data: data, encoding: .utf8) else {
            throw FfiError.Serialization(msg: "canned reply failed to encode")
        }
        return json
    }
}
