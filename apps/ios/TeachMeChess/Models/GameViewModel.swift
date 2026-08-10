import Foundation
import Observation

/// One chess piece, parsed out of the FEN board field.
struct Piece: Equatable {
    let isWhite: Bool
    /// Lowercase FEN letter: k q r b n p
    let kind: Character

    /// Filled unicode glyph (rendered in the piece's color by the view —
    /// the "white" outline glyphs are too thin at board sizes).
    var glyph: String {
        switch kind {
        case "k": return "\u{265A}"
        case "q": return "\u{265B}"
        case "r": return "\u{265C}"
        case "b": return "\u{265D}"
        case "n": return "\u{265E}"
        // U+FE0E forces text presentation: the pawn otherwise renders as
        // the color emoji on iOS, ignoring the view's tint.
        default: return "\u{265F}\u{FE0E}"
        }
    }
}

/// One entry in the coach panel's message feed (chronological, oldest
/// first).
struct CoachMessage: Identifiable, Equatable {
    /// `note` is an opponent-move observation from the commentary engine
    /// (threat warning, phase change, game-story summary) — rendered with
    /// an eye icon to set it apart from move reactions.
    enum Role { case coach, student, system, note }
    let id = UUID()
    let role: Role
    let text: String
    /// The message was produced while the student was reviewing a past
    /// position (rewind mode) — the UI tags it with a small "review" label.
    var isReview: Bool = false
    /// Position the message was talking about (FEN at creation time) —
    /// chess mentions in the text resolve against it. Empty for restored
    /// messages, which fall back to `ply`.
    var fen: String = ""
    /// Half-moves played when the message was created (mention resolution
    /// context for moves the coach just reacted to).
    var ply: Int = 0
}

/// One stored chat/feed row, decoded from the store's JSON (`ResumeReport`
/// and `gameDetail` share the shape).
struct StoredChatInfo: Decodable, Equatable {
    let atPly: UInt32
    /// student / coach / system.
    let role: String
    let isReview: Bool
    let text: String
    let createdAt: Int64

    /// The invisible grounding prefix `sendChat` adds to review-mode
    /// questions. The Rust session records the FULL outgoing text, so on
    /// resume the prefix is stripped back off (and the message re-tagged
    /// as review) to reconstruct what the student actually typed.
    static let reviewPrefixStart = "[The student is reviewing"

    /// (display text, was-review-prefixed) for a stored student message.
    var strippedReviewPrefix: (text: String, wasPrefixed: Bool) {
        guard text.hasPrefix(Self.reviewPrefixStart),
              let end = text.range(of: "] ")
        else { return (text, false) }
        return (String(text[end.upperBound...]), true)
    }
}

/// Decoded `resumeFromStore` payload — everything needed to redraw a game
/// that was in progress when the app last quit.
struct ResumeReportInfo: Decodable {
    let gameId: Int64
    let fen: String
    let historySan: [String]
    let chat: [StoredChatInfo]
    let botLevel: UInt32
}

/// Captured-material picture, decoded from `materialSummary`'s JSON.
/// Captured lists arrive most-valuable-first as piece letters (uppercase =
/// white pieces, lowercase = black); `materialDiff` is in pawn units,
/// positive = White ahead (computed board-side from the pieces still on the
/// board, so promotions count at full value).
struct MaterialSummaryInfo: Decodable, Equatable {
    let capturedByWhite: [String]
    let capturedByBlack: [String]
    let materialDiff: Int32

    static let empty = MaterialSummaryInfo(
        capturedByWhite: [], capturedByBlack: [], materialDiff: 0)

    /// Pawn-unit advantage of White (0 when not ahead).
    var whiteAdvantage: Int { max(0, Int(materialDiff)) }
    /// Pawn-unit advantage of Black (0 when not ahead).
    var blackAdvantage: Int { max(0, -Int(materialDiff)) }
}

/// Engine verdict on a student move, decoded from `judgeMove`'s JSON.
struct MoveVerdictInfo: Decodable, Equatable {
    let playedSan: String
    let judgment: String
    let cpLoss: Int32
    let allowsMateIn: Int32?
    let missedMateIn: Int32?

    /// Judgments the coach speaks up about unprompted (mirrors
    /// coach-core's `Judgment::is_notable`).
    var isNotable: Bool {
        ["inaccuracy", "mistake", "blunder"].contains(judgment)
    }
}

/// End-of-game summary, decoded from `finishGame`'s JSON.
struct GameSummaryInfo: Decodable, Equatable, Identifiable {
    var id: String { "\(movesJudged)-\(estRating)-\(acl)" }
    let acl: Double
    let accuracy: Double
    let estRating: UInt32
    let readyToAdvance: Bool
    let movesJudged: UInt32
    /// Filled in locally, not part of the Rust JSON.
    var outcomeText: String = ""
    /// Level movement this game caused (placement / auto-advance) — filled
    /// in locally by the level processing, shown as the summary's badge.
    var levelEventBadge: String?

    private enum CodingKeys: String, CodingKey {
        case acl, accuracy, estRating, readyToAdvance, movesJudged
    }
}

/// View model driving one full game against the embedded engine with live
/// coaching.
///
/// Two Rust objects cooperate:
/// - `CoachSessionHandle` (embedded Stockfish 11 analyst + opponent, LLM
///   coach) is the source of truth for the game. All its methods block, so
///   every call happens on `sessionQueue`, never the main thread.
/// - `BoardHandle` is a pure-computation mirror the UI reads synchronously
///   for selection highlighting and legal destinations. After every
///   accepted move (student or opponent) the same move is replayed onto the
///   mirror; on any error the mirror is resynced from the session's FEN.
///
/// Review mode ("rewind"): tapping a ply in the move list shows the
/// position after that ply by replaying `moves` onto a temporary
/// `BoardHandle`. The live session is NEVER mutated — review is a pure
/// display overlay, and "Back to live" simply drops it.
@Observable
final class GameViewModel {
    // MARK: Board mirror (main-thread state)

    private var board = BoardHandle()

    /// Algebraic square ("e4") -> piece for the LIVE game.
    private(set) var livePieces: [String: Piece] = [:]
    /// Currently selected square, if any.
    private(set) var selectedSquare: String?
    /// Legal destinations for the selected piece.
    private(set) var legalTargets: Set<String> = []
    /// From/to squares of the last played move in the LIVE game.
    private(set) var liveLastMoveSquares: Set<String> = []
    /// Move list in SAN.
    private(set) var moves: [String] = []
    /// Captured pieces + material balance for the LIVE game (drives the
    /// discarded-pieces trays around the board).
    private var liveMaterial: MaterialSummaryInfo = .empty
    /// Human-readable result once the game is over.
    private(set) var outcomeText: String?
    private(set) var whiteToMove = true

    // MARK: Review mode (rewind)

    /// 1-based ply currently under review; nil = live.
    private(set) var reviewPly: Int?
    private var reviewPieces: [String: Piece] = [:]
    private var reviewLastMoveSquares: Set<String> = []
    private var reviewMaterial: MaterialSummaryInfo = .empty
    /// FEN of the reviewed position (chat grounding).
    private(set) var reviewFen: String = ""

    var isReviewing: Bool { reviewPly != nil }

    /// "3. Nf3" / "3… Nc6" label for a 1-based ply.
    func plyLabel(_ ply: Int) -> String {
        guard ply >= 1, ply <= moves.count else { return "move \(ply)" }
        let number = (ply + 1) / 2
        let separator = ply.isMultiple(of: 2) ? "… " : ". "
        return "\(number)\(separator)\(moves[ply - 1])"
    }

    // MARK: Displayed board (live or reviewed)

    var pieces: [String: Piece] { isReviewing ? reviewPieces : livePieces }
    var lastMoveSquares: Set<String> { isReviewing ? reviewLastMoveSquares : liveLastMoveSquares }
    /// Captured pieces + balance for whatever position the board shows.
    var material: MaterialSummaryInfo { isReviewing ? reviewMaterial : liveMaterial }
    /// Verdict chip belongs to the live position — hidden while reviewing.
    var verdictSquare: String? { isReviewing ? nil : liveVerdictSquare }
    var verdictJudgment: String? { isReviewing ? nil : liveVerdictJudgment }

    // MARK: Session

    static let opponentMovetimeMs: UInt32 = 300

    /// Current opponent level (band), mirrored from `LevelProgress` for
    /// synchronous view reads — updated on placement, auto-advance, and
    /// Settings overrides.
    private(set) var opponentLevel = LevelProgress.current

    private var session: CoachSessionHandle?
    private let coach: ForeignCoachModel
    /// Which on-device coach backend is available (FM or canned).
    let onDeviceBackendName: String
    /// Active backend/engine/opponent summary. No longer shown under the
    /// coach header (vertical space) — it appears in Settings and is
    /// posted to the feed as a system line on session start and on every
    /// backend switch.
    private(set) var statusLine: String
    private(set) var sessionReady = false
    /// The provider currently answering (drives the settings sheet).
    private(set) var activeProvider: CoachProvider = .onDevice

    private let sessionQueue = DispatchQueue(label: "dev.teachmechess.session", qos: .userInitiated)

    // MARK: Coaching UI state

    /// Destination square of the last judged student move (verdict chip).
    private(set) var liveVerdictSquare: String?
    /// Judgment for the chip: best/excellent/good/inaccuracy/mistake/blunder.
    private(set) var liveVerdictJudgment: String?
    /// Coach panel feed, chronological (oldest first, bounded).
    private(set) var coachFeed: [CoachMessage] = []
    /// The LLM is composing a reaction or chat reply.
    private(set) var isCoachThinking = false
    /// The opponent engine is choosing its move.
    private(set) var isBotThinking = false
    /// A judge/react/reply pipeline is in flight — board input is locked.
    private(set) var isPipelineRunning = false
    /// Draft text for the chat field.
    var chatDraft = ""
    /// Set when the game ends; drives the summary sheet.
    var gameSummary: GameSummaryInfo?
    /// The Pro Coach trial has lapsed (proxy rejection, or detected at
    /// launch) — the game screen presents the trial-expiry sheet offering
    /// the subscription or the free on-device coach.
    var showTrialExpiry = false
    /// Pro Coach was selected but the cloud-AI disclosure hasn't been
    /// agreed to yet — the game screen presents the consent sheet, which
    /// either records consent and re-applies or reverts to on-device.
    var showProCoachConsent = false

    // MARK: Coach spotlight (tapped chess mentions)

    /// Squares pulsing under the coach's spotlight (tapped piece/square
    /// mentions in the feed).
    private(set) var spotlightSquares: Set<String> = []
    /// Animated arrows for move mentions.
    private(set) var spotlightArrows: [BoardArrow] = []
    /// URL of the mention that produced the current spotlight — tapping
    /// the same mention again toggles it off.
    private var spotlightKey: String?
    private var spotlightExpiry: DispatchWorkItem?
    /// Lexicon entry being shown in the term sheet.
    var presentedLexicon: LexiconEntry?

    // MARK: - Init

    init() {
        if FoundationModelsCoach.isAvailable {
            coach = FoundationModelsCoach()
            onDeviceBackendName = "Apple Foundation Models"
        } else {
            coach = CannedCoachModel()
            onDeviceBackendName = "Canned coach"
        }
        statusLine = "Starting embedded engine…"
        refresh()
        startSession()
    }

    private func startSession() {
        let coach = self.coach
        let level = LevelProgress.current
        sessionQueue.async { [weak self] in
            // The app's chosen band is patched into the profile handed to
            // the session (or a fresh default profile at that band), so the
            // Rust side plays and records at the level the student picked.
            let profileJson = LevelProgress.sessionProfileJson(saved: Self.loadProfileJson())
            var handle: CoachSessionHandle?
            var failure: String?
            do {
                handle = try CoachSessionHandle.newWithForeignModelEmbedded(
                    model: coach, fen: nil, profileJson: profileJson, voice: false)
            } catch {
                // A corrupt saved profile must not brick the app — retry
                // once with a fresh profile before giving up.
                handle = try? CoachSessionHandle.newWithForeignModelEmbedded(
                    model: coach, fen: nil,
                    profileJson: LevelProgress.sessionProfileJson(saved: nil), voice: false)
                if handle == nil {
                    failure = "\(error.localizedDescription)"
                }
            }
            handle?.attachOpponentAnalyst(
                skillLevel: level.engineSkill,
                movetimeMs: Self.opponentMovetimeMs)

            // Attach SQLite persistence and resume any game that was in
            // progress when the app last quit. A store failure degrades to
            // an unpersisted session, never a crash.
            var resume: ResumeReportInfo?
            if let handle {
                do {
                    if let json = try handle.resumeFromStore(dbPath: Self.dbPath) {
                        resume = try? Self.snakeDecoder()
                            .decode(ResumeReportInfo.self, from: Data(json.utf8))
                    }
                } catch {
                    // resumeFromStore also attaches; only retry the plain
                    // attach if the whole call failed.
                    try? handle.attachStore(dbPath: Self.dbPath)
                }
            }

            // Apply the persisted chattiness once the session (fresh or
            // resumed) is up — the Rust policy defaults to Balanced, the
            // app's default is Chatty.
            handle?.setCommentaryStyle(style: CoachChattiness.load().ffiStyle)

            DispatchQueue.main.async {
                guard let self else { return }
                if let handle {
                    self.session = handle
                    self.sessionReady = true
                    self.statusLine = Self.statusLine(backend: self.onDeviceBackendName)
                    if let resume {
                        self.restoreFromResume(resume)
                    } else {
                        self.pushCoach("Ready when you are — you're White. I'll check every move with the engine.")
                        self.persistFeed(role: "coach", text: "Ready when you are — you're White. I'll check every move with the engine.")
                        self.announcePlacementIfPending()
                    }
                    // One-line session-start summary (backend, engine,
                    // opponent level) — this info left the panel header,
                    // so the feed carries it once per session. Transient:
                    // not persisted, or every relaunch would append one.
                    self.pushMessage(role: .system, text: self.statusLine)
                    self.restoreSavedBackend()
                } else {
                    self.statusLine = "Engine unavailable: \(failure ?? "unknown error") — free board only"
                    self.pushCoach("I couldn't start the engine, so it's a free board for now.")
                }
            }
        }
    }

    /// Redraw a resumed game: replay its SAN history onto a fresh mirror,
    /// rebuild the coach feed from the stored chat, and post a subtle
    /// system line. Runs on the main thread.
    private func restoreFromResume(_ report: ResumeReportInfo) {
        let replay = BoardHandle()
        var replayed = true
        for san in report.historySan where (try? replay.playSan(san: san)) == nil {
            replayed = false
            break
        }
        if replayed {
            board = replay
        } else if let synced = try? BoardHandle.fromFen(fen: report.fen) {
            // The mirror lost the move list but the position is right.
            board = synced
        }
        refresh()
        if !replayed { moves = report.historySan }
        liveLastMoveSquares = []

        coachFeed = report.chat.map { entry in
            let ply = Int(entry.atPly)
            switch entry.role {
            case "student":
                let (text, wasPrefixed) = entry.strippedReviewPrefix
                return CoachMessage(role: .student, text: text,
                                    isReview: entry.isReview || wasPrefixed, ply: ply)
            case "system":
                return CoachMessage(role: .system, text: entry.text,
                                    isReview: entry.isReview, ply: ply)
            default:
                return CoachMessage(role: .coach, text: entry.text,
                                    isReview: entry.isReview, ply: ply)
            }
        }
        let count = report.historySan.count
        if count > 0 {
            let line = "Resumed game in progress (\(count) \(count == 1 ? "move" : "moves"))"
            coachFeed.append(CoachMessage(role: .system, text: line))
            persistFeed(role: "system", text: line)
        }
        coachFeed = Array(coachFeed.suffix(Self.feedLimit))

        // If the app died between the student's move and the opponent's
        // reply, it is Black's turn on resume — let the opponent move, or
        // the game would be stuck (input is White-only).
        if outcomeText == nil && !whiteToMove {
            runOpponentTurn()
        }
    }

    /// Opponent-only half of the pipeline (used after a resume that landed
    /// on Black's turn).
    private func runOpponentTurn() {
        guard let session else { return }
        isPipelineRunning = true
        isBotThinking = true
        sessionQueue.async { [weak self] in
            let reply = try? session.opponentReply()
            let fenAfterReply = session.fen()
            // Opponent commentary (auto-recorded by the Rust session).
            let note = (reply ?? nil) != nil
                ? (try? session.reactToOpponentMove()) ?? nil : nil
            let over = session.isGameOver()
            if over { self?.finishGameOnQueue(session: session) }
            DispatchQueue.main.async {
                guard let self else { return }
                self.isBotThinking = false
                self.isPipelineRunning = false
                if let botSan = reply ?? nil {
                    self.applyOpponentMove(botSan, sessionFen: fenAfterReply)
                }
                if let note {
                    self.pushMessage(role: .note, text: note)
                }
            }
        }
    }

    private static func statusLine(backend: String) -> String {
        "\(backend) · embedded Stockfish 11 · opponent \(LevelProgress.current.shortLabel)"
    }

    /// Backend part of the current status line — remembered so level
    /// changes can rebuild the line without re-deriving the provider.
    private var activeBackendLabel: String {
        activeProvider == .proCoach ? "Pro Coach" : onDeviceBackendName
    }

    /// Coach line explaining placement mode, pushed at the start of any
    /// game played while an "evaluate me" placement is pending.
    private func announcePlacementIfPending() {
        guard LevelProgress.isPlacementPending else { return }
        let line = "This one's a placement game — play naturally and I'll "
            + "work out your level from how it goes."
        pushCoach(line)
        persistFeed(role: "coach", text: line)
    }

    // MARK: - Backend switching

    /// Re-apply the provider saved in UserDefaults on launch. On-device is
    /// already active (the session was constructed with the foreign model),
    /// so only Pro Coach needs restoring.
    private func restoreSavedBackend() {
        let settings = BackendSettings.load()
        guard settings.provider != .onDevice else { return }
        applyBackend(settings, announce: false)
    }

    /// Swap the live session's LLM backend (the embedded engine is a
    /// singleton, so the session itself must survive the switch).
    func applyBackend(_ settings: BackendSettings, announce: Bool = true) {
        guard let session else {
            statusLine = "Engine unavailable — cannot switch coach backend"
            return
        }
        // Pro Coach must register with the proxy first (a network call) —
        // it takes its own async path and switches only once a device JWT
        // is in hand.
        if settings.provider == .proCoach {
            applyProCoachBackend(session: session, announce: announce)
            return
        }
        let coach = self.coach
        let backendLabel = settings.backendLabel(onDeviceName: onDeviceBackendName)

        sessionQueue.async { [weak self] in
            session.setBackendForeign(model: coach)
            DispatchQueue.main.async {
                guard let self else { return }
                self.activeProvider = settings.provider
                self.statusLine = Self.statusLine(backend: backendLabel)
                if announce {
                    // Carries the full picture (backend, engine, opponent
                    // level) since the panel header no longer shows it.
                    let line = "Coach backend switched to \(backendLabel) · "
                        + "embedded Stockfish 11 · opponent \(LevelProgress.current.shortLabel)."
                    self.pushMessage(role: .system, text: line)
                    self.persistFeed(role: "system", text: line)
                }
            }
        }
    }

    /// Pro Coach: make sure this device holds a valid proxy JWT (register /
    /// refresh over the network), then swap the live session's backend to
    /// the proxy's OpenAI-compatible endpoint with that JWT as the bearer
    /// key. Any failure — network, trial ended, not entitled — leaves the
    /// current backend untouched and surfaces as a feed line, mirroring
    /// how a missing API key is handled on launch.
    private func applyProCoachBackend(session: CoachSessionHandle, announce: Bool) {
        // Explicit cloud-AI consent comes first (guideline 5.1.2(i)):
        // no registration-to-coach handoff, no game content leaves the
        // device until the student has agreed. The consent sheet re-runs
        // the apply on agreement or reverts the provider on decline.
        guard ProCoachAccount.hasCloudConsent else {
            showProCoachConsent = true
            return
        }
        Task { [weak self] in
            let status: ProCoachStatus
            do {
                status = try await ProCoachAccount.ensureRegistered()
            } catch {
                // A trial-over rejection is a purchase moment, not a dead
                // end — offer the paywall, unless StoreKit already shows an
                // active subscription (claim still syncing to the server).
                let locallyEntitled = await ProCoachStore.shared.hasLocalEntitlement()
                let offerPaywall = Self.isPaywallableRejection(error) && !locallyEntitled
                DispatchQueue.main.async {
                    guard let self else { return }
                    let line = "Pro Coach isn't available right now "
                        + "(\(error.localizedDescription)) — keeping the current coach."
                    self.pushCoach(line)
                    self.persistFeed(role: "coach", text: line)
                    if offerPaywall { self.presentTrialExpiry() }
                }
                return
            }
            guard status.tier != .free, let token = ProCoachAccount.storedToken() else {
                // Tier free = the trial ended and no subscription is linked
                // — same purchase moment as above.
                let locallyEntitled = await ProCoachStore.shared.hasLocalEntitlement()
                let offerPaywall = status.tier == .free && !locallyEntitled
                DispatchQueue.main.async {
                    guard let self else { return }
                    let line = status.tier == .free
                        ? "Your Pro Coach trial has ended — keeping the current coach."
                        : "Pro Coach registration didn't return a token — keeping the current coach."
                    self.pushCoach(line)
                    self.persistFeed(role: "coach", text: line)
                    if offerPaywall { self.presentTrialExpiry() }
                }
                return
            }
            guard let self else { return }
            self.sessionQueue.async {
                // The JWT lives ~30 days and there is no mid-session 401
                // hook yet: if it expires while playing, coach replies fail
                // until the student re-applies Pro Coach in Settings
                // (ensureRegistered() is cheap and refreshes the token).
                session.setBackendOpenai(
                    baseUrl: ProCoachAccount.baseURL + "/v1",
                    apiKey: token,
                    model: ProCoachAccount.model)
                DispatchQueue.main.async {
                    self.activeProvider = .proCoach
                    // No model name here on purpose — which model backs the
                    // Pro Coach is the proxy's business and changes freely.
                    self.statusLine = Self.statusLine(backend: "Pro Coach")
                    if announce {
                        var line = "Coach backend switched to Pro Coach · "
                            + "embedded Stockfish 11 · opponent \(LevelProgress.current.shortLabel)."
                        if status.tier == .trial {
                            line += " \(status.summary)."
                        }
                        self.pushMessage(role: .system, text: line)
                        self.persistFeed(role: "system", text: line)
                    }
                }
            }
        }
    }

    /// The student agreed to the cloud-AI disclosure: record it and run
    /// the Pro Coach switch that was waiting on it.
    func acceptProCoachConsent() {
        ProCoachAccount.recordCloudConsent()
        showProCoachConsent = false
        applyBackend(BackendSettings.load())
    }

    /// The student declined: revert the saved provider to on-device so
    /// the next launch doesn't re-prompt, and say so in the feed.
    func declineProCoachConsent() {
        showProCoachConsent = false
        var settings = BackendSettings.load()
        settings.provider = .onDevice
        settings.save()
        let line = "Staying with the on-device coach — Pro Coach is in Settings whenever you want it."
        pushCoach(line)
        persistFeed(role: "coach", text: line)
    }

    /// Proxy rejections a subscription would fix — the paywall triggers.
    /// (`trial_expired` / `trial_exhausted` come back as 403s once the
    /// trial ends; `not_entitled` is the tier-free rejection.)
    private static func isPaywallableRejection(_ error: Error) -> Bool {
        guard case ProCoachError.server(let code, _) = error else { return false }
        return ["trial_expired", "trial_exhausted", "not_entitled"].contains(code)
    }

    // MARK: - Trial expiry

    /// The proactive launch prompt fires once per lapsed trial; reactive
    /// triggers (the student actively selecting Pro Coach) always show.
    private static let trialExpiryPromptedKey = "procoach.trialExpiryPrompted"

    private func presentTrialExpiry() {
        UserDefaults.standard.set(true, forKey: Self.trialExpiryPromptedKey)
        showTrialExpiry = true
    }

    /// Launch/foreground check: if this device rode the Pro Coach trial
    /// and it has lapsed, surface the convert-or-downgrade sheet — once,
    /// without waiting for the student to bump into a rejected request.
    /// Expiry is the server's call: a locally-lapsed-looking trial is
    /// confirmed with a forced re-register before prompting.
    @MainActor
    func checkTrialExpiry() async {
        guard BackendSettings.load().provider == .proCoach,
              var status = ProCoachAccount.storedStatus()
        else { return }
        if status.tier == .pro {
            // Subscribed — a future lapse (cancellation) is a fresh prompt.
            UserDefaults.standard.set(false, forKey: Self.trialExpiryPromptedKey)
            return
        }
        if status.tier == .trial {
            let lapsed = (status.trialExpiresAt.map { $0 <= Date() } ?? false)
                || status.trialRequestsRemaining == 0
            guard lapsed,
                  let fresh = try? await ProCoachAccount.ensureRegistered(forceRefresh: true)
            else { return }
            status = fresh
        }
        guard status.tier == .free,
              !UserDefaults.standard.bool(forKey: Self.trialExpiryPromptedKey)
        else { return }
        // An active StoreKit entitlement means the claim just hasn't
        // synced — not a lapse; the claim path handles it.
        guard !(await ProCoachStore.shared.hasLocalEntitlement()) else { return }
        presentTrialExpiry()
    }

    /// The student chose the free coach over subscribing (trial-expiry
    /// sheet's downgrade path): persist the provider and switch the live
    /// session back to on-device.
    func switchToFreeCoach() {
        var settings = BackendSettings.load()
        settings.provider = .onDevice
        settings.save()
        applyBackend(settings)
    }

    // MARK: - Opponent level

    /// Settings override: persist the chosen band, re-aim the live
    /// opponent engine, and keep the on-disk profile in step.
    func applyOpponentLevel(_ band: Int) {
        guard band != LevelProgress.current.band || LevelProgress.isPlacementPending else { return }
        LevelProgress.setBand(band)
        levelStateChanged(
            line: "Opponent set to \(LevelProgress.current.fullLabel) — applies from the next opponent move.")
    }

    /// Settings "re-evaluate me": back to placement mode — the next
    /// finished game with enough judged moves places the student anew.
    func beginPlacement() {
        LevelProgress.beginPlacement()
        levelStateChanged(
            line: "Let's re-evaluate. Play the next game naturally and I'll place you from it.")
        announcePlacementIfPending()
    }

    /// Shared tail of every level change made OUTSIDE the game-end flow:
    /// mirror the new state, patch the saved profile so the next session
    /// launches at the right band, retune the live opponent, and say so.
    private func levelStateChanged(line: String) {
        opponentLevel = LevelProgress.current
        if let saved = Self.loadProfileJson() {
            Self.saveProfileJson(LevelProgress.patchedProfileJson(saved))
        }
        statusLine = Self.statusLine(backend: activeBackendLabel)
        pushMessage(role: .system, text: line)
        persistFeed(role: "system", text: line)
        guard let session else { return }
        let skill = opponentLevel.engineSkill
        sessionQueue.async {
            session.attachOpponentAnalyst(
                skillLevel: skill, movetimeMs: Self.opponentMovetimeMs)
        }
    }

    /// Main-thread tail of a game-end level event (placement resolved or
    /// auto-advance fired): mirror the state and announce it in the feed.
    /// The queue side already re-aimed the engine and patched the profile.
    private func applyLevelEvent(_ event: LevelProgress.Event?) {
        guard let event else { return }
        opponentLevel = LevelProgress.current
        statusLine = Self.statusLine(backend: activeBackendLabel)
        pushCoach(event.feedLine)
    }

    // MARK: - Commentary style

    /// Apply a chattiness level to the live session (the Rust commentary
    /// policy decides WHEN the coach speaks). The caller persists the
    /// choice; this posts a system feed line when announcing.
    func applyChattiness(_ style: CoachChattiness, announce: Bool = true) {
        guard let session else { return }
        sessionQueue.async {
            session.setCommentaryStyle(style: style.ffiStyle)
        }
        if announce {
            let line = "Coach chattiness set to \(style.displayName)"
            pushMessage(role: .system, text: line)
            persistFeed(role: "system", text: line)
        }
    }

    // MARK: - Intents

    func newGame() {
        guard !isPipelineRunning else { return }
        gameSummary = nil
        exitReview()
        guard let session else {
            resetLocalBoard()
            return
        }
        isPipelineRunning = true
        sessionQueue.async { [weak self] in
            let error: String? = {
                do {
                    try session.resetGame(fen: nil)
                    return nil
                } catch {
                    return error.localizedDescription
                }
            }()
            DispatchQueue.main.async {
                guard let self else { return }
                self.isPipelineRunning = false
                if let error {
                    let line = "Couldn't reset the game: \(error)"
                    self.pushCoach(line)
                    self.persistFeed(role: "coach", text: line)
                    return
                }
                self.resetLocalBoard()
            }
        }
    }

    /// Resign the current game: counts as a loss, finalizes the stored game
    /// row, and shows the summary sheet. Available once at least one move
    /// has been played.
    func resign() {
        guard let session, outcomeText == nil, !moves.isEmpty, !isPipelineRunning else { return }
        exitReview()
        isPipelineRunning = true
        sessionQueue.async { [weak self] in
            guard let self else { return }
            let (summary, event) = self.finalizeGameOnQueue(
                session: session, result: .loss, outcome: "You resigned — 0-1")
            DispatchQueue.main.async {
                self.isPipelineRunning = false
                self.outcomeText = "You resigned — 0-1"
                self.gameSummary = summary
                self.pushCoach(Self.wrapUpLine(result: .loss, summary: summary))
                self.applyLevelEvent(event)
            }
        }
    }

    /// Ask the LIVE session's coach about a position in a PAST, finished
    /// game (the History detail screen). Same grounding pattern as
    /// review-mode chat: an invisible context prefix carries the game, move
    /// number, and FEN. The live game is never mutated.
    func askCoachAboutPastGame(
        gameId: Int64, ply: Int, moveLabel: String, fen: String,
        question: String, completion: @escaping (String) -> Void
    ) {
        guard let session, sessionReady else {
            completion("The coach session isn't available right now.")
            return
        }
        let outgoing = "[The student is reviewing a PAST, finished game (game #\(gameId)) "
            + "in their history — NOT the current game. The position is after move \(ply) "
            + "(\(moveLabel)), FEN: \(fen). Answer about THAT position from that past game.] "
            + question
        sessionQueue.async {
            let reply: String
            do {
                reply = try session.chat(text: outgoing)
            } catch {
                reply = "Sorry, I couldn't answer that: \(error.localizedDescription)"
                session.logFeed(role: "coach", isReview: true, text: reply)
            }
            DispatchQueue.main.async { completion(reply) }
        }
    }

    func tap(square: String) {
        clearSpotlight()
        guard outcomeText == nil, !isPipelineRunning, !isReviewing else { return }
        // With a live session the student plays White; the pipeline moves
        // for Black. Without one, the board is free-play for both sides.
        if sessionReady && !whiteToMove { return }

        if let from = selectedSquare {
            if square == from {
                clearSelection()
                return
            }
            if legalTargets.contains(square) {
                playMove(from: from, to: square)
                return
            }
        }
        // (Re)select if the tapped square holds a piece of the side to move.
        if let piece = livePieces[square], piece.isWhite == whiteToMove {
            selectedSquare = square
            legalTargets = Set(board.legalDestinations(from: square))
        } else {
            clearSelection()
        }
    }

    /// Enter review mode: show the position after the 1-based `ply` by
    /// replaying the SAN history onto a temporary board. The live session
    /// is untouched.
    func enterReview(ply: Int) {
        guard ply >= 1, ply <= moves.count else { return }
        let replay = BoardHandle()
        var before: [String: Piece] = Self.parseFenBoard(replay.fen())
        for (i, san) in moves.prefix(ply).enumerated() {
            if i == ply - 1 { before = Self.parseFenBoard(replay.fen()) }
            guard (try? replay.playSan(san: san)) != nil else { return }
        }
        clearSelection()
        clearSpotlight()
        let after = Self.parseFenBoard(replay.fen())
        let changed = Set(before.keys).union(after.keys).filter { before[$0] != after[$0] }
        reviewPieces = after
        reviewLastMoveSquares = Set(changed)
        reviewFen = replay.fen()
        reviewMaterial = Self.decodeMaterial(replay.materialSummary())
        reviewPly = ply
    }

    /// Leave review mode and show the live game again.
    func exitReview() {
        clearSpotlight()
        reviewPly = nil
        reviewPieces = [:]
        reviewLastMoveSquares = []
        reviewFen = ""
        reviewMaterial = .empty
    }

    func sendChat() {
        let text = chatDraft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty, let session, !isCoachThinking else { return }
        chatDraft = ""

        // While reviewing, ground the model in the rewound position via an
        // invisible prefix — the feed shows only what the student typed.
        let reviewing = isReviewing
        let outgoing: String
        if let ply = reviewPly {
            outgoing = "[The student is reviewing the position after move \(ply) "
                + "(\(plyLabel(ply))), FEN: \(reviewFen). Answer about THAT position, "
                + "not the current game position.] \(text)"
        } else {
            outgoing = text
        }

        pushStudent(text, isReview: reviewing)
        isCoachThinking = true
        sessionQueue.async { [weak self] in
            let reply: String
            do {
                // Question and reply are auto-recorded by the Rust session.
                reply = try session.chat(text: outgoing)
            } catch {
                reply = "Sorry, I couldn't answer that: \(error.localizedDescription)"
                session.logFeed(role: "coach", isReview: reviewing, text: reply)
            }
            DispatchQueue.main.async {
                self?.isCoachThinking = false
                self?.pushCoach(reply, isReview: reviewing)
            }
        }
    }

    // MARK: - Coach spotlight

    /// Handle a tap on a chess mention inside a coach message: highlight
    /// the piece/square on the board, draw arrows for move mentions, or
    /// open the lexicon sheet for a named opening/concept.
    func handleCoachLink(_ url: URL, in message: CoachMessage) {
        guard let mention = ChessMention.from(url) else { return }
        if case .term(let slug) = mention {
            presentedLexicon = Lexicon.shared?.entry(slug: slug)
            return
        }
        // Same mention tapped again -> toggle the spotlight off.
        if spotlightKey == url.absoluteString, !spotlightSquares.isEmpty || !spotlightArrows.isEmpty {
            clearSpotlight()
            return
        }
        let key = url.absoluteString
        switch mention {
        case .move(let san):
            spotlightMove(san: san, message: message, key: key)
        case .piece(let square):
            spotlightPiece(on: square, message: message, key: key)
        case .square(let square):
            // Ambiguous token: an occupied square reads as its piece; an
            // empty one that parses as a legal pawn move reads as the move.
            if pieces[square] != nil {
                spotlightPiece(on: square, message: message, key: key)
            } else if resolveMention(san: square, message: message) != nil {
                spotlightMove(san: square, message: message, key: key)
            } else {
                setSpotlight(squares: [square], arrows: [], key: key)
            }
        case .term:
            break
        }
    }

    func clearSpotlight() {
        spotlightExpiry?.cancel()
        spotlightExpiry = nil
        spotlightSquares = []
        spotlightArrows = []
        spotlightKey = nil
    }

    /// Highlight the piece on `square`, plus arrows for every move the
    /// same message mentions FOR that piece (moves originating there).
    private func spotlightPiece(on square: String, message: CoachMessage, key: String) {
        let arrows = ChessMentionScanner.matches(in: message.text)
            .compactMap { match -> BoardArrow? in
                guard case .move(let san) = match.mention else { return nil }
                return resolveMention(san: san, message: message)
            }
            .filter { $0.from == square }
        var seen = Set<String>()
        let unique = arrows.filter { seen.insert($0.id).inserted }
        setSpotlight(squares: [square], arrows: unique, key: key)
    }

    /// Arrow for one move mention; falls back to highlighting the
    /// destination square when the move resolves in no known position.
    private func spotlightMove(san: String, message: CoachMessage, key: String) {
        if let arrow = resolveMention(san: san, message: message) {
            setSpotlight(squares: [arrow.from], arrows: [arrow], key: key)
        } else if let dest = MoveResolver.destinationSquare(of: san) {
            setSpotlight(squares: [dest], arrows: [], key: key)
        }
    }

    private func setSpotlight(squares: Set<String>, arrows: [BoardArrow], key: String) {
        spotlightSquares = squares
        spotlightArrows = arrows
        spotlightKey = key
        // Fade out on its own so stale hints never linger over live play.
        spotlightExpiry?.cancel()
        let expire = DispatchWorkItem { [weak self] in self?.clearSpotlight() }
        spotlightExpiry = expire
        DispatchQueue.main.asyncAfter(deadline: .now() + 30, execute: expire)
    }

    /// Resolve a SAN mention to from/to squares, trying positions in
    /// order: the moves actually played around the message's ply (the
    /// coach usually reacts to what was just played), the position the
    /// message was created in, and the currently displayed position.
    private func resolveMention(san: String, message: CoachMessage) -> BoardArrow? {
        let clean = san.strippedSanDecorations
        // Was this one of the two half-moves leading up to the message?
        for ply in [message.ply, message.ply - 1] where ply >= 1 && ply <= moves.count {
            if moves[ply - 1].strippedSanDecorations == clean,
               let arrow = MoveResolver.resolve(san: clean, inFen: fenAfter(plies: ply - 1)) {
                return arrow
            }
        }
        if !message.fen.isEmpty, let arrow = MoveResolver.resolve(san: clean, inFen: message.fen) {
            return arrow
        }
        let displayedFen = isReviewing ? reviewFen : board.fen()
        return MoveResolver.resolve(san: clean, inFen: displayedFen)
    }

    /// FEN after the first `plies` half-moves of the live game.
    private func fenAfter(plies: Int) -> String {
        let replay = BoardHandle()
        for san in moves.prefix(plies) {
            guard (try? replay.playSan(san: san)) != nil else { break }
        }
        return replay.fen()
    }

    // MARK: - Move pipeline

    private func playMove(from: String, to: String) {
        let uci = from + to + promotionSuffix(from: from, to: to)
        var playedSan: String?
        do {
            playedSan = try board.playUci(uci: uci)
        } catch {
            // Fallback to SAN for encodings UCI rejects. In practice the
            // only candidate is castling (already standard-encoded, but be
            // defensive): king tapped two files sideways -> castling SAN.
            if livePieces[from]?.kind == "k" {
                let san: String? = to.hasPrefix("g") ? "O-O" : (to.hasPrefix("c") ? "O-O-O" : nil)
                if let san, (try? board.playSan(san: san)) != nil {
                    playedSan = san
                }
            }
        }
        guard let san = playedSan else {
            clearSelection()
            return
        }
        // Optimistically show the student's move.
        liveLastMoveSquares = [from, to]
        liveVerdictSquare = nil
        liveVerdictJudgment = nil
        refresh()
        runPipeline(studentSan: san, to: to)
    }

    /// judge -> verdict chip -> commentary -> opponent reply -> game over.
    /// Runs entirely on `sessionQueue`; every UI mutation hops to main.
    private func runPipeline(studentSan san: String, to toSquare: String) {
        guard let session else { return }
        isPipelineRunning = true

        sessionQueue.async { [weak self] in
            func onMain(_ work: @escaping (GameViewModel) -> Void) {
                DispatchQueue.main.async {
                    if let model = self { work(model) }
                }
            }

            // 1. Engine judgment of the student move (plays it on the
            //    session's board — the mirror already played it).
            let verdict: MoveVerdictInfo?
            do {
                verdict = Self.decodeVerdict(try session.judgeMove(san: san))
            } catch {
                // The session rejected a move the mirror accepted (drift or
                // engine failure). Resync the mirror from the session and
                // unlock — the session's board is the truth.
                let fen = session.fen()
                let line = "Engine hiccup on \(san): \(error.localizedDescription)"
                session.logFeed(role: "coach", isReview: false, text: line)
                onMain { model in
                    model.resyncMirror(fromFen: fen)
                    model.isPipelineRunning = false
                    model.pushCoach(line)
                }
                return
            }
            if let verdict {
                onMain { model in
                    model.liveVerdictSquare = toSquare
                    model.liveVerdictJudgment = verdict.judgment
                }
            }

            // 2. Commentary: the Rust commentary policy owns the cadence —
            //    canned line or full LLM reaction, per the chattiness
            //    setting. The reply auto-records to the store; do NOT
            //    logFeed it again. The "Coach is thinking…" indicator only
            //    appears when the reply takes >300ms, so canned lines feel
            //    instant. LLM failures degrade to the engine-verdict line
            //    plus a friendly note — never a crash.
            let showThinking = DispatchWorkItem { [weak self] in
                self?.isCoachThinking = true
            }
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.3, execute: showThinking)
            var text: String
            var failureNote: String?
            do {
                text = try session.reactToStudentMove()
            } catch {
                text = session.briefReaction()
                failureNote = "(The coach model couldn't respond — showing the engine's take instead. \(error.localizedDescription))"
                session.logFeed(role: "coach", isReview: false, text: text)
                session.logFeed(role: "system", isReview: false, text: failureNote!)
            }
            showThinking.cancel()
            onMain { model in
                model.isCoachThinking = false
                model.pushCoach(text)
                if let failureNote {
                    model.pushCoach(failureNote)
                }
            }

            // 3. Did the student's move end the game?
            if session.isGameOver() {
                self?.finishGameOnQueue(session: session)
                onMain { $0.isPipelineRunning = false }
                return
            }

            // 4. Opponent's turn.
            onMain { $0.isBotThinking = true }
            let reply = try? session.opponentReply()
            let fenAfterReply = session.fen()
            onMain { model in
                model.isBotThinking = false
                if let botSan = reply ?? nil {
                    model.applyOpponentMove(botSan, sessionFen: fenAfterReply)
                }
            }

            // 4b. Opponent commentary: the policy speaks only when there is
            //     something worth flagging (threat, phase change, game
            //     summary) — nil means silence. Auto-recorded by the Rust
            //     session; rendered as a distinct "watch out" note.
            if (reply ?? nil) != nil, let note = (try? session.reactToOpponentMove()) ?? nil {
                onMain { $0.pushMessage(role: .note, text: note) }
            }

            // 5. Did the opponent's move end the game?
            if session.isGameOver() {
                self?.finishGameOnQueue(session: session)
            }
            onMain { $0.isPipelineRunning = false }
        }
    }

    /// Replay the opponent's SAN onto the mirror; on any disagreement,
    /// resync from the session's FEN. Highlights every changed square
    /// (covers castling's two-piece move).
    private func applyOpponentMove(_ san: String, sessionFen: String) {
        let before = livePieces
        do {
            try board.playSan(san: san)
        } catch {
            resyncMirror(fromFen: sessionFen)
        }
        refresh()
        let changed = Set(before.keys).union(livePieces.keys).filter { before[$0] != livePieces[$0] }
        liveLastMoveSquares = Set(changed)
    }

    /// Called on `sessionQueue` once the session reports game over: map the
    /// result (student is White), close out the game, persist the profile,
    /// and surface the summary sheet.
    private func finishGameOnQueue(session: CoachSessionHandle) {
        let outcome = (try? BoardHandle.fromFen(fen: session.fen()))?
            .outcomeText() ?? "Game over"
        let result: FfiGameResult
        if outcome.contains("1-0") {
            result = .win
        } else if outcome.contains("0-1") {
            result = .loss
        } else {
            result = .draw
        }
        let (summary, event) = finalizeGameOnQueue(
            session: session, result: result, outcome: outcome)

        DispatchQueue.main.async { [weak self] in
            guard let self else { return }
            self.gameSummary = summary
            self.pushCoach(Self.wrapUpLine(result: result, summary: summary))
            self.applyLevelEvent(event)
        }
    }

    /// Runs on `sessionQueue`: close out the game with the Rust session,
    /// fold the result into the level state (placement / auto-advance),
    /// persist the profile patched with the (possibly new) band, and
    /// retune the opponent engine if the level moved. Returns the
    /// decorated summary plus the event for the main-thread announcement.
    private func finalizeGameOnQueue(
        session: CoachSessionHandle, result: FfiGameResult, outcome: String
    ) -> (summary: GameSummaryInfo?, event: LevelProgress.Event?) {
        let summaryJson = session.finishGame(result: result)
        var summary = Self.decodeSummary(summaryJson)
        summary?.outcomeText = outcome
        let processed = LevelProgress.processGameEnd(
            profileJson: session.profileJson(), summary: summary, result: result)
        Self.saveProfileJson(processed.profileJson)
        if let event = processed.event {
            // The new strength applies from the next game's first move.
            session.attachOpponentAnalyst(
                skillLevel: event.level.engineSkill, movetimeMs: Self.opponentMovetimeMs)
            session.logFeed(role: "coach", isReview: false, text: event.feedLine)
            summary?.levelEventBadge = event.summaryBadge
        }
        return (summary, processed.event)
    }

    // MARK: - Local board helpers

    private func resetLocalBoard() {
        board = BoardHandle()
        liveLastMoveSquares = []
        liveVerdictSquare = nil
        liveVerdictJudgment = nil
        exitReview()
        coachFeed = []
        pushCoach("Fresh board — you're White. Your move!")
        persistFeed(role: "coach", text: "Fresh board — you're White. Your move!")
        announcePlacementIfPending()
        refresh()
    }

    private func resyncMirror(fromFen fen: String) {
        if let synced = try? BoardHandle.fromFen(fen: fen) {
            board = synced
        }
        refresh()
    }

    private func promotionSuffix(from: String, to: String) -> String {
        guard let piece = livePieces[from], piece.kind == "p" else { return "" }
        let targetRank = piece.isWhite ? "8" : "1"
        return to.hasSuffix(targetRank) ? "q" : ""
    }

    private func clearSelection() {
        selectedSquare = nil
        legalTargets = []
    }

    private func refresh() {
        clearSelection()
        clearSpotlight()
        livePieces = Self.parseFenBoard(board.fen())
        moves = board.historySan()
        outcomeText = board.outcomeText()
        whiteToMove = board.turnWhite()
        liveMaterial = Self.decodeMaterial(board.materialSummary())
    }

    // MARK: - Feed

    private static let feedLimit = 200

    /// Position context stamped onto new feed messages, so chess mentions
    /// in them resolve against what the coach was actually looking at.
    private var messageContext: (fen: String, ply: Int) {
        isReviewing ? (reviewFen, reviewPly ?? 0) : (board.fen(), moves.count)
    }

    private func pushCoach(_ text: String, isReview: Bool = false) {
        let ctx = messageContext
        coachFeed.append(CoachMessage(role: .coach, text: text, isReview: isReview,
                                      fen: ctx.fen, ply: ctx.ply))
        coachFeed = Array(coachFeed.suffix(Self.feedLimit))
    }

    fileprivate func pushMessage(role: CoachMessage.Role, text: String) {
        let ctx = messageContext
        coachFeed.append(CoachMessage(role: role, text: text,
                                      fen: ctx.fen, ply: ctx.ply))
        coachFeed = Array(coachFeed.suffix(Self.feedLimit))
    }

    private func pushStudent(_ text: String, isReview: Bool = false) {
        let ctx = messageContext
        coachFeed.append(CoachMessage(role: .student, text: text, isReview: isReview,
                                      fen: ctx.fen, ply: ctx.ply))
        coachFeed = Array(coachFeed.suffix(Self.feedLimit))
    }

    /// Persist a locally generated feed message into the store's chat log
    /// (the Rust session auto-records only what flows through it — LLM
    /// replies and student `chat()` text; canned greetings, notices, and
    /// `briefReaction` lines are the app's to log). Best-effort.
    private func persistFeed(role: String, text: String, isReview: Bool = false) {
        guard let session else { return }
        sessionQueue.async {
            session.logFeed(role: role, isReview: isReview, text: text)
        }
    }

    private static func wrapUpLine(result: FfiGameResult, summary: GameSummaryInfo?) -> String {
        let opener: String
        switch result {
        case .win: opener = "You won — well played!"
        case .loss: opener = "Tough one, but every loss is a lesson."
        default: opener = "A draw — hard-fought."
        }
        guard let summary else { return opener }
        return "\(opener) Accuracy \(String(format: "%.0f", summary.accuracy))% over \(summary.movesJudged) judged moves."
    }

    // MARK: - JSON decoding

    private static func snakeDecoder() -> JSONDecoder {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return decoder
    }

    static func decodeVerdict(_ json: String) -> MoveVerdictInfo? {
        try? snakeDecoder().decode(MoveVerdictInfo.self, from: Data(json.utf8))
    }

    static func decodeSummary(_ json: String) -> GameSummaryInfo? {
        try? snakeDecoder().decode(GameSummaryInfo.self, from: Data(json.utf8))
    }

    static func decodeMaterial(_ json: String) -> MaterialSummaryInfo {
        (try? snakeDecoder().decode(MaterialSummaryInfo.self, from: Data(json.utf8))) ?? .empty
    }

    // MARK: - Profile persistence

    /// SQLite store for games/moves/chat, in the app's Documents directory.
    static var dbPath: String {
        FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("coach.db").path
    }

    private static var profileURL: URL {
        FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("student_profile.json")
    }

    /// Whether any student profile has ever been saved — the app root uses
    /// this to skip onboarding for installs that predate the flow.
    static var hasSavedProfile: Bool {
        FileManager.default.fileExists(atPath: profileURL.path)
    }

    private static func loadProfileJson() -> String? {
        try? String(contentsOf: profileURL, encoding: .utf8)
    }

    private static func saveProfileJson(_ json: String) {
        try? json.write(to: profileURL, atomically: true, encoding: .utf8)
    }

    // MARK: - FEN parsing

    /// FEN board field -> square-name-keyed piece map.
    static func parseFenBoard(_ fen: String) -> [String: Piece] {
        var result: [String: Piece] = [:]
        guard let boardField = fen.split(separator: " ").first else { return result }
        let ranks = boardField.split(separator: "/")
        for (i, rankStr) in ranks.enumerated() {
            let rank = 8 - i
            var file = 0
            for ch in rankStr {
                if let skip = ch.wholeNumberValue, (1...8).contains(skip) {
                    file += skip
                } else if file < 8 {
                    let files = "abcdefgh"
                    let name = "\(files[files.index(files.startIndex, offsetBy: file)])\(rank)"
                    result[name] = Piece(
                        isWhite: ch.isUppercase,
                        kind: Character(ch.lowercased())
                    )
                    file += 1
                }
            }
        }
        return result
    }
}
