import Foundation

/// One rung of the opponent ladder — the app-side mirror of the Rust
/// profile's Maia bands (1100…1900 in steps of 100). The band number IS the
/// level's identity everywhere: in the student profile JSON, in the store's
/// game rows, and in the UserDefaults override.
struct OpponentLevel: Equatable, Identifiable, Hashable {
    /// Maia rating band, 1100…1900.
    let band: Int

    var id: Int { band }

    static let minBand = 1100
    static let maxBand = 1900
    static let bandStep = 100

    /// Every selectable level, weakest first.
    static let all: [OpponentLevel] =
        stride(from: minBand, through: maxBand, by: bandStep).map(OpponentLevel.init)

    static func clamped(_ band: Int) -> OpponentLevel {
        let snapped = (band + bandStep / 2) / bandStep * bandStep
        return OpponentLevel(band: min(max(snapped, minBand), maxBand))
    }

    /// 1-based ladder position (Level 1…9) — friendlier than a raw rating
    /// for students who don't know what an Elo number means yet.
    var ordinal: Int { (band - Self.minBand) / Self.bandStep + 1 }

    /// Club-flavored name per band, in the Sealed Move voice.
    var displayName: String {
        switch band {
        case 1100: return "Newcomer"
        case 1200: return "Learner"
        case 1300: return "Improver"
        case 1400: return "Club Casual"
        case 1500: return "Club Regular"
        case 1600: return "Club Strong"
        case 1700: return "Tournament"
        case 1800: return "Candidate"
        default: return "Master Track"
        }
    }

    /// "Level 3 · Improver (~1300)" — the full label for pickers.
    var fullLabel: String { "Level \(ordinal) · \(displayName) (~\(band))" }

    /// "Level 3 (~1300)" — the compact label for status lines.
    var shortLabel: String { "Level \(ordinal) (~\(band))" }

    /// Embedded-Stockfish "Skill Level" (0…20) approximating this band at
    /// the session's short movetime. Linear on purpose: the exact Elo of a
    /// skill step varies with time control, and the placement/advancement
    /// loop corrects any mismatch by moving the band, not the mapping.
    var engineSkill: UInt8 { UInt8((band - Self.minBand) / Self.bandStep * 2) }
}

/// Where the student's level lives and how it moves: persistence
/// (UserDefaults), the placement flow ("evaluate me"), the auto-advance
/// rule, and the patch that keeps the Rust profile JSON agreeing with the
/// app's choice.
///
/// The app is the authority for the level. The Rust session's in-profile
/// `bot_level` is made to follow: the profile JSON is patched with the
/// app's band both when it's handed to a new session and when it's saved
/// after a game — so a Settings override can never be clobbered by the
/// session writing back a stale level.
enum LevelProgress {
    private enum Keys {
        static let band = "level.band"
        static let autoAdvance = "level.autoAdvance"
        static let placementPending = "level.placementPending"
        static let winsAtBand = "level.winsAtBand"
    }

    /// Band a fresh "evaluate me" run starts against — middle-low, so the
    /// placement game is neither a stomp nor a massacre for most players.
    static let placementStartBand = 1300
    /// Judged moves below which one game carries too little signal to
    /// place anyone (mirrors the Rust core's aborted-game threshold).
    static let placementMinMoves: UInt32 = 6
    /// Wins at a band needed before an auto-advance (the Rust rule).
    static let winsToAdvance = 3

    static var current: OpponentLevel {
        let stored = UserDefaults.standard.integer(forKey: Keys.band)
        guard stored != 0 else { return OpponentLevel(band: OpponentLevel.minBand) }
        return OpponentLevel.clamped(stored)
    }

    /// Auto level-up after repeated wins. Default on; the Settings toggle
    /// exists for students who'd rather climb by hand.
    static var autoAdvance: Bool {
        get { UserDefaults.standard.object(forKey: Keys.autoAdvance) as? Bool ?? true }
        set { UserDefaults.standard.set(newValue, forKey: Keys.autoAdvance) }
    }

    /// An "evaluate me" placement game hasn't finished yet.
    static var isPlacementPending: Bool {
        UserDefaults.standard.bool(forKey: Keys.placementPending)
    }

    private static var winsAtBand: Int {
        get { UserDefaults.standard.integer(forKey: Keys.winsAtBand) }
        set { UserDefaults.standard.set(newValue, forKey: Keys.winsAtBand) }
    }

    /// Manual level choice (onboarding pick or Settings override). Clears
    /// any pending placement and restarts the win counter at the new band.
    static func setBand(_ band: Int) {
        UserDefaults.standard.set(OpponentLevel.clamped(band).band, forKey: Keys.band)
        UserDefaults.standard.set(false, forKey: Keys.placementPending)
        winsAtBand = 0
    }

    /// Enter "evaluate me" mode: the next finished game with enough judged
    /// moves places the student at their estimated band.
    static func beginPlacement() {
        UserDefaults.standard.set(placementStartBand, forKey: Keys.band)
        UserDefaults.standard.set(true, forKey: Keys.placementPending)
        winsAtBand = 0
    }

    // MARK: - Game-end processing

    /// What a finished game did to the level.
    enum Event: Equatable {
        /// "Evaluate me" concluded: placed at `level` off one game's
        /// estimated rating.
        case placed(OpponentLevel, estRating: Int)
        /// Auto-advance fired: enough wins at `from`, promoted to `to`.
        case advanced(from: OpponentLevel, to: OpponentLevel)

        /// The level the event landed on.
        var level: OpponentLevel {
            switch self {
            case .placed(let level, _): return level
            case .advanced(_, let to): return to
            }
        }

        /// Coach-voiced feed line announcing the change.
        var feedLine: String {
            switch self {
            case .placed(let level, let est):
                return "Placement done — that game plays like ~\(est). "
                    + "I'm starting you at \(level.fullLabel). "
                    + "You can adjust this anytime in Settings."
            case .advanced(let from, let to):
                return "That's \(winsToAdvance) wins at \(from.shortLabel) — "
                    + "you've earned the step up. Next game you face \(to.fullLabel)."
            }
        }

        /// Short badge for the game-summary sheet.
        var summaryBadge: String {
            switch self {
            case .placed(let level, _):
                return "Placed at \(level.fullLabel)"
            case .advanced(_, let to):
                return "Level up! Now \(to.fullLabel)"
            }
        }
    }

    /// Fold a finished game into the level state: resolve a pending
    /// placement (given enough judged moves), or count the win and check
    /// the auto-advance rule — 3 wins at the band AND a profile rating
    /// estimate within 100 of it, mirroring the Rust `ready_to_advance`.
    /// Returns the profile JSON patched with the (possibly new) band, plus
    /// the event when the level moved.
    static func processGameEnd(
        profileJson: String, summary: GameSummaryInfo?, result: FfiGameResult
    ) -> (profileJson: String, event: Event?) {
        var event: Event?

        if isPlacementPending {
            if let summary, summary.movesJudged >= placementMinMoves {
                let level = OpponentLevel.clamped(Int(summary.estRating))
                UserDefaults.standard.set(level.band, forKey: Keys.band)
                UserDefaults.standard.set(false, forKey: Keys.placementPending)
                winsAtBand = 0
                event = .placed(level, estRating: Int(summary.estRating))
            }
            // Too short to judge: stay in placement — the next game decides.
        } else {
            if result == .win { winsAtBand += 1 }
            let level = current
            if autoAdvance,
               winsAtBand >= winsToAdvance,
               level.band < OpponentLevel.maxBand,
               let rating = ratingEstimate(in: profileJson),
               rating + 100 >= level.band {
                let next = OpponentLevel(band: level.band + OpponentLevel.bandStep)
                UserDefaults.standard.set(next.band, forKey: Keys.band)
                winsAtBand = 0
                event = .advanced(from: level, to: next)
            }
        }

        return (patchedProfileJson(profileJson), event)
    }

    // MARK: - Profile JSON patching

    /// Rewrite `bot_level` (and the advancement win counter) inside a Rust
    /// `StudentProfile` JSON so the session and the store agree with the
    /// app's chosen band. Falls back to the input on any parse trouble —
    /// a malformed profile is the session's problem to recover, not ours
    /// to amplify.
    static func patchedProfileJson(_ json: String) -> String {
        guard var profile = (try? JSONSerialization.jsonObject(
                with: Data(json.utf8))) as? [String: Any]
        else { return json }
        profile["bot_level"] = current.band
        profile["wins_at_current_level"] = winsAtBand
        guard let data = try? JSONSerialization.data(withJSONObject: profile),
              let patched = String(data: data, encoding: .utf8)
        else { return json }
        return patched
    }

    /// Profile JSON to hand a NEW session: the saved profile patched with
    /// the app's band, or a fresh default profile at that band when no
    /// save exists yet (first launch after onboarding).
    static func sessionProfileJson(saved: String?) -> String {
        if let saved { return patchedProfileJson(saved) }
        return """
        {"rating_estimate":800,"bot_level":\(current.band),"games_played":0,\
        "wins_at_current_level":0,"concepts":{},"notes":[],"game_history":[]}
        """
    }

    /// `rating_estimate` out of a profile JSON (the EMA the Rust core
    /// maintains) — the advancement check's second input.
    private static func ratingEstimate(in json: String) -> Int? {
        guard let profile = (try? JSONSerialization.jsonObject(
                with: Data(json.utf8))) as? [String: Any]
        else { return nil }
        return (profile["rating_estimate"] as? NSNumber)?.intValue
    }
}
