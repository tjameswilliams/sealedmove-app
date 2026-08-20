import Foundation

/// The App Store rating ask.
///
/// `requestReview` is throttled by the system (roughly three prompts per
/// year, and the system decides whether anything is drawn at all), so the
/// quota is only spent at moments where the student has just got
/// something out of the app: a game they finished without losing, or a
/// review walkthrough they sat through. Asking straight after a loss
/// farms one-star ratings.
enum RatingPrompt {
    /// Games finished, any result. Nobody is asked in their first sitting.
    private static let gamesKey = "ratingPrompt.completedGames"
    /// Short version string we last asked on, so a student sees the ask at
    /// most once per release even where the system would allow more.
    private static let askedVersionKey = "ratingPrompt.askedVersion"

    /// Games a student has to finish before the ask is on the table.
    private static let minimumGames = 2

    static var completedGames: Int {
        UserDefaults.standard.integer(forKey: gamesKey)
    }

    /// Call once per finished game, whatever the result.
    static func recordCompletedGame() {
        UserDefaults.standard.set(completedGames + 1, forKey: gamesKey)
    }

    /// True when an ask here would be well timed and not a repeat. The
    /// caller owns the "was this a good moment" half of the decision.
    static var isEligible: Bool {
        guard completedGames >= minimumGames, let version = currentVersion else {
            return false
        }
        return UserDefaults.standard.string(forKey: askedVersionKey) != version
    }

    /// Record that we asked. The system decides whether a prompt actually
    /// appears; either way we do not come back this release.
    static func markAsked() {
        guard let version = currentVersion else { return }
        UserDefaults.standard.set(version, forKey: askedVersionKey)
    }

    private static var currentVersion: String? {
        Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String
    }
}
