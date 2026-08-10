import SwiftUI

// MARK: - Decoded store JSON

/// One finished-game row from `historyList` / `gameDetail`.
struct GameRowInfo: Decodable, Identifiable, Equatable {
    let id: Int64
    let startedAt: Int64
    let endedAt: Int64?
    /// win/loss/draw/aborted; nil while the game is still open.
    let result: String?
    let startingFen: String?
    let botLevel: UInt32
    let opponentKind: String
    let eco: String?
    let opening: String?
    let acl: Double?
    let accuracy: Double?
    let estRating: Int64?
    let movesJudged: Int64?
    let moveCount: UInt32

    var startedDate: Date { Date(timeIntervalSince1970: TimeInterval(startedAt)) }
}

/// One stored move from `gameDetail`.
struct StoredMoveInfo: Decodable, Equatable {
    let ply: UInt32
    let san: String
    let uci: String?
    let byStudent: Bool
    let judgment: String?
    let cpLoss: Int32?
    let allowsMateIn: Int32?
    let missedMateIn: Int32?
    let createdAt: Int64
}

/// `gameDetail` payload.
struct GameDetailInfo: Decodable {
    let game: GameRowInfo
    let moves: [StoredMoveInfo]
    let chat: [StoredChatInfo]
}

/// `storeStats` payload.
struct StoreStatsInfo: Decodable, Equatable {
    let games: UInt32
    let wins: UInt32
    let losses: UInt32
    let draws: UInt32
    let aborted: UInt32
    let bestAccuracy: Double?
    let currentBotLevel: UInt32?
}

private func storeDecoder() -> JSONDecoder {
    let decoder = JSONDecoder()
    decoder.keyDecodingStrategy = .convertFromSnakeCase
    return decoder
}

// MARK: - History list

/// Finished games, newest first, with store-wide stats in the header.
/// Everything on this screen reads the SQLite store through short-lived
/// read-only connections (the FFI free functions) — the live session is
/// never touched.
struct HistoryView: View {
    let model: GameViewModel
    @Environment(\.dismiss) private var dismiss

    @State private var stats: StoreStatsInfo?
    @State private var games: [GameRowInfo] = []
    @State private var canLoadMore = false
    @State private var isLoading = false
    @State private var loadedOnce = false

    private static let pageSize: UInt32 = 20

    var body: some View {
        NavigationStack {
            List {
                if let stats {
                    statsHeader(stats)
                }
                Section {
                    if games.isEmpty && loadedOnce && !isLoading {
                        Text("No finished games yet — finish (or resign) a game and it will appear here.")
                            .font(.callout)
                            .foregroundStyle(.secondary)
                    }
                    ForEach(games) { game in
                        NavigationLink(value: game.id) {
                            gameRow(game)
                        }
                    }
                    if canLoadMore {
                        Button {
                            loadPage()
                        } label: {
                            if isLoading {
                                ProgressView().frame(maxWidth: .infinity)
                            } else {
                                Text("Load more").frame(maxWidth: .infinity)
                            }
                        }
                    }
                }
            }
            .navigationTitle("History")
            .navigationBarTitleDisplayMode(.inline)
            .navigationDestination(for: Int64.self) { gameId in
                GameDetailScreen(model: model, gameId: gameId)
            }
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") { dismiss() }
                }
            }
            .onAppear {
                if !loadedOnce { loadPage() }
            }
        }
    }

    @ViewBuilder
    private func statsHeader(_ stats: StoreStatsInfo) -> some View {
        Section {
            HStack(spacing: 0) {
                statCell("Games", "\(stats.games)")
                statCell("W-L-D", "\(stats.wins)-\(stats.losses)-\(stats.draws)")
                statCell("Best acc.",
                         stats.bestAccuracy.map { String(format: "%.0f%%", $0) } ?? "—")
            }
        }
    }

    @ViewBuilder
    private func statCell(_ title: String, _ value: String) -> some View {
        VStack(spacing: 2) {
            Text(value)
                .font(.headline.monospacedDigit())
            Text(title)
                .font(.caption2)
                .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity)
    }

    @ViewBuilder
    private func gameRow(_ game: GameRowInfo) -> some View {
        HStack(spacing: 12) {
            ResultBadge(result: game.result)
            VStack(alignment: .leading, spacing: 2) {
                Text(game.opening ?? "—")
                    .font(.subheadline)
                    .lineLimit(1)
                Text(game.startedDate.formatted(date: .abbreviated, time: .shortened))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            VStack(alignment: .trailing, spacing: 2) {
                Text(game.accuracy.map { String(format: "%.0f%%", $0) } ?? "—")
                    .font(.subheadline.monospacedDigit())
                Text("\(game.estRating.map { "~\($0)" } ?? "—") · \(game.moveCount) mv")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
    }

    private func loadPage() {
        guard !isLoading else { return }
        isLoading = true
        let offset = UInt32(games.count)
        let dbPath = GameViewModel.dbPath
        DispatchQueue.global(qos: .userInitiated).async {
            let statsInfo = (try? storeStats(dbPath: dbPath))
                .flatMap { try? storeDecoder().decode(StoreStatsInfo.self, from: Data($0.utf8)) }
            let page = (try? historyList(dbPath: dbPath, limit: Self.pageSize, offset: offset))
                .flatMap { try? storeDecoder().decode([GameRowInfo].self, from: Data($0.utf8)) }
                ?? []
            DispatchQueue.main.async {
                if let statsInfo { stats = statsInfo }
                games.append(contentsOf: page)
                canLoadMore = page.count == Int(Self.pageSize)
                isLoading = false
                loadedOnce = true
            }
        }
    }
}

/// W green / L red / D gray / aborted dim.
private struct ResultBadge: View {
    let result: String?

    private var style: (label: String, color: Color, dim: Bool) {
        switch result {
        case "win": return ("W", .green, false)
        case "loss": return ("L", .red, false)
        case "draw": return ("D", .gray, false)
        default: return ("A", .gray, true)
        }
    }

    var body: some View {
        Text(style.label)
            .font(.subheadline.bold())
            .foregroundStyle(.white)
            .frame(width: 28, height: 28)
            .background(Circle().fill(style.color))
            .opacity(style.dim ? 0.35 : 1)
            .accessibilityLabel(result ?? "unknown result")
    }
}

// MARK: - Game detail (finished-game replay)

/// One past game: a read-only board stepped with ◀ ▶ (the stored SAN moves
/// replayed onto a fresh `BoardHandle`), a tappable move list, the game's
/// chat log, and an "ask the coach" field that queries the LIVE session's
/// coach about the stepped position (review-tagged, past-game-grounded).
struct GameDetailScreen: View {
    let model: GameViewModel
    let gameId: Int64

    @State private var detail: GameDetailInfo?
    @State private var missing = false
    /// FEN after i plies; `fens[0]` is the starting position.
    @State private var fens: [String] = []
    /// Captured pieces + balance after i plies (parallel to `fens`).
    @State private var materials: [MaterialSummaryInfo] = []
    /// Shown position = after `ply` moves (0...fens.count-1).
    @State private var ply = 0
    @State private var question = ""
    @State private var askFeed: [CoachMessage] = []
    @State private var isAsking = false
    @FocusState private var askFocused: Bool

    var body: some View {
        Group {
            if let detail {
                loadedBody(detail)
            } else if missing {
                ContentUnavailableView("Game not found", systemImage: "questionmark.circle")
            } else {
                ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .navigationTitle(detail.map { $0.game.opening ?? "Game" } ?? "Game")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear(perform: load)
    }

    @ViewBuilder
    private func loadedBody(_ detail: GameDetailInfo) -> some View {
        GeometryReader { geo in
            VStack(spacing: 8) {
                ScrollView {
                    VStack(spacing: 10) {
                        header(detail.game)
                        // Same captured-pieces trays as the live board:
                        // Black's captures above, the student's below,
                        // stepping along with ◀ ▶.
                        VStack(spacing: 2) {
                            CapturedTrayRow(
                                letters: currentMaterial.capturedByBlack,
                                advantage: currentMaterial.blackAdvantage)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .padding(.horizontal, 2)
                            ReplayBoardView(
                                pieces: currentPieces,
                                lastMoveSquares: lastMoveSquares,
                                verdictSquare: verdictSquare,
                                verdictJudgment: verdictJudgment)
                                .frame(width: geo.size.width - 24, height: geo.size.width - 24)
                            CapturedTrayRow(
                                letters: currentMaterial.capturedByWhite,
                                advantage: currentMaterial.whiteAdvantage)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .padding(.horizontal, 2)
                        }
                        stepper(detail)
                        moveGrid(detail)
                        chatLog(detail)
                        askSection
                    }
                    .padding(.horizontal, 12)
                    .padding(.bottom, 8)
                }
                .scrollDismissesKeyboard(.interactively)
                askInput
            }
        }
    }

    // MARK: Header / stepper

    @ViewBuilder
    private func header(_ game: GameRowInfo) -> some View {
        HStack(spacing: 10) {
            ResultBadge(result: game.result)
            VStack(alignment: .leading, spacing: 2) {
                Text(game.opening ?? "Unknown opening")
                    .font(.subheadline.bold())
                    .lineLimit(1)
                Text(game.startedDate.formatted(date: .abbreviated, time: .shortened)
                     + (game.accuracy.map { String(format: " · %.0f%% accuracy", $0) } ?? "")
                     + (game.estRating.map { " · ~\($0)" } ?? ""))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer()
        }
    }

    @ViewBuilder
    private func stepper(_ detail: GameDetailInfo) -> some View {
        HStack(spacing: 24) {
            Button {
                ply = max(0, ply - 1)
            } label: {
                Image(systemName: "chevron.backward.circle.fill").font(.title2)
            }
            .disabled(ply == 0)
            Text(ply == 0 ? "Start" : plyLabel(ply))
                .font(.system(.subheadline, design: .monospaced))
                .frame(minWidth: 90)
            Button {
                ply = min(fens.count - 1, ply + 1)
            } label: {
                Image(systemName: "chevron.forward.circle.fill").font(.title2)
            }
            .disabled(ply >= fens.count - 1)
        }
        .buttonStyle(.plain)
        .foregroundStyle(.primary)
    }

    // MARK: Move list

    @ViewBuilder
    private func moveGrid(_ detail: GameDetailInfo) -> some View {
        let sans = replayableMoves(detail)
        if !sans.isEmpty {
            VStack(alignment: .leading, spacing: 6) {
                Text("Moves")
                    .font(.caption.bold())
                    .foregroundStyle(.secondary)
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 64), spacing: 4)], spacing: 4) {
                    ForEach(1...sans.count, id: \.self) { p in
                        Button {
                            ply = p
                        } label: {
                            Text(shortPlyLabel(p))
                                .font(.system(.caption, design: .monospaced))
                                .lineLimit(1)
                                .padding(.horizontal, 4)
                                .padding(.vertical, 4)
                                .frame(maxWidth: .infinity)
                                .background(ply == p ? Color.orange.opacity(0.25)
                                                     : Color(.secondarySystemBackground),
                                            in: RoundedRectangle(cornerRadius: 6))
                        }
                        .buttonStyle(.plain)
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    // MARK: Chat log

    @ViewBuilder
    private func chatLog(_ detail: GameDetailInfo) -> some View {
        if !detail.chat.isEmpty {
            VStack(alignment: .leading, spacing: 8) {
                Text("Chat log")
                    .font(.caption.bold())
                    .foregroundStyle(.secondary)
                ForEach(Array(detail.chat.enumerated()), id: \.offset) { _, entry in
                    chatRow(entry)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(10)
            .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 10))
        }
    }

    @ViewBuilder
    private func chatRow(_ entry: StoredChatInfo) -> some View {
        let stripped: (String, Bool) = {
            guard entry.role == "student" else { return (entry.text, false) }
            let s = entry.strippedReviewPrefix
            return (s.text, s.wasPrefixed)
        }()
        if entry.role == "system" {
            MarkdownText(text: entry.text)
                .font(.caption2)
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, alignment: .center)
        } else {
            HStack(alignment: .top, spacing: 6) {
                Image(systemName: entry.role == "coach" ? "graduationcap" : "person")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .padding(.top, 2)
                VStack(alignment: .leading, spacing: 2) {
                    if entry.isReview || stripped.1 {
                        Text("review")
                            .font(.caption2.bold())
                            .padding(.horizontal, 6)
                            .padding(.vertical, 1)
                            .background(Color.orange.opacity(0.18), in: Capsule())
                            .foregroundStyle(.orange)
                    }
                    // Coach messages render light markdown; the student's
                    // own words stay plain text.
                    if entry.role == "coach" {
                        MarkdownText(text: stripped.0)
                            .font(.caption)
                            .foregroundStyle(.primary)
                    } else {
                        Text(stripped.0)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    // MARK: Ask the coach

    @ViewBuilder
    private var askSection: some View {
        if !askFeed.isEmpty || isAsking {
            VStack(alignment: .leading, spacing: 8) {
                Text("Ask the coach")
                    .font(.caption.bold())
                    .foregroundStyle(.secondary)
                ForEach(askFeed) { message in
                    HStack(alignment: .top, spacing: 6) {
                        Image(systemName: message.role == .coach ? "graduationcap" : "person")
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                            .padding(.top, 2)
                        VStack(alignment: .leading, spacing: 2) {
                            Text("review")
                                .font(.caption2.bold())
                                .padding(.horizontal, 6)
                                .padding(.vertical, 1)
                                .background(Color.orange.opacity(0.18), in: Capsule())
                                .foregroundStyle(.orange)
                            if message.role == .coach {
                                MarkdownText(text: message.text)
                                    .font(.caption)
                                    .foregroundStyle(.primary)
                            } else {
                                Text(message.text)
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                            }
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
                if isAsking {
                    HStack(spacing: 6) {
                        ProgressView().controlSize(.small)
                        Text("Coach is thinking…")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(10)
            .background(Color.orange.opacity(0.08), in: RoundedRectangle(cornerRadius: 10))
        }
    }

    private var askInput: some View {
        HStack(spacing: 8) {
            TextField("Ask about this position…", text: $question)
                .textFieldStyle(.roundedBorder)
                .focused($askFocused)
                .submitLabel(.send)
                .onSubmit { ask() }
            Button {
                ask()
            } label: {
                Image(systemName: "arrow.up.circle.fill").font(.title2)
            }
            .disabled(isAsking
                      || question.trimmingCharacters(in: .whitespaces).isEmpty)
        }
        .padding(.horizontal, 12)
        .padding(.bottom, 8)
    }

    private func ask() {
        let text = question.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty, !isAsking, ply < fens.count else { return }
        question = ""
        askFocused = false
        askFeed.append(CoachMessage(role: .student, text: text, isReview: true))
        isAsking = true
        model.askCoachAboutPastGame(
            gameId: gameId,
            ply: ply,
            moveLabel: ply == 0 ? "the starting position" : plyLabel(ply),
            fen: fens[ply],
            question: text
        ) { reply in
            isAsking = false
            askFeed.append(CoachMessage(role: .coach, text: reply, isReview: true))
        }
    }

    // MARK: Replay plumbing

    /// SAN moves that replayed cleanly (fens has count+1 entries).
    private func replayableMoves(_ detail: GameDetailInfo) -> [StoredMoveInfo] {
        Array(detail.moves.prefix(max(0, fens.count - 1)))
    }

    private var currentPieces: [String: Piece] {
        guard ply < fens.count else { return [:] }
        return GameViewModel.parseFenBoard(fens[ply])
    }

    private var currentMaterial: MaterialSummaryInfo {
        guard ply < materials.count else { return .empty }
        return materials[ply]
    }

    private var lastMoveSquares: Set<String> {
        guard ply >= 1, ply < fens.count else { return [] }
        let before = GameViewModel.parseFenBoard(fens[ply - 1])
        let after = GameViewModel.parseFenBoard(fens[ply])
        return Set(before.keys).union(after.keys).filter { before[$0] != after[$0] }
    }

    /// Verdict chip for the stepped move (student moves only — opponent
    /// moves carry no judgment).
    private var verdictSquare: String? {
        guard let move = steppedMove, move.judgment != nil else { return nil }
        return Self.destinationSquare(of: move)
    }

    /// Destination square of a stored move. The store's `uci` is only
    /// filled for opponent moves, so student moves fall back to parsing
    /// the SAN (the trailing file+rank; castling maps to the king's
    /// landing square — the student always plays White here).
    static func destinationSquare(of move: StoredMoveInfo) -> String? {
        if let uci = move.uci, uci.count >= 4 {
            return String(uci.dropFirst(2).prefix(2))
        }
        let san = move.san
        if san.hasPrefix("O-O") {
            let rank = move.byStudent ? "1" : "8"
            return san.hasPrefix("O-O-O") ? "c\(rank)" : "g\(rank)"
        }
        let cleaned = san.filter { "abcdefgh12345678".contains($0) }
        guard cleaned.count >= 2 else { return nil }
        let rank = cleaned.last!
        let file = cleaned[cleaned.index(cleaned.endIndex, offsetBy: -2)]
        guard "abcdefgh".contains(file), "12345678".contains(rank) else { return nil }
        return "\(file)\(rank)"
    }

    private var verdictJudgment: String? {
        steppedMove?.judgment
    }

    private var steppedMove: StoredMoveInfo? {
        guard ply >= 1, let detail, ply - 1 < detail.moves.count else { return nil }
        return detail.moves[ply - 1]
    }

    private func plyLabel(_ p: Int) -> String {
        guard let detail, p >= 1, p - 1 < detail.moves.count else { return "move \(p)" }
        let number = (p + 1) / 2
        let separator = p.isMultiple(of: 2) ? "… " : ". "
        return "\(number)\(separator)\(detail.moves[p - 1].san)"
    }

    private func shortPlyLabel(_ p: Int) -> String {
        plyLabel(p)
    }

    private func load() {
        guard detail == nil else { return }
        let dbPath = GameViewModel.dbPath
        let id = gameId
        DispatchQueue.global(qos: .userInitiated).async {
            guard let json = (try? gameDetail(dbPath: dbPath, gameId: id)) ?? nil,
                  let decoded = try? storeDecoder().decode(GameDetailInfo.self, from: Data(json.utf8))
            else {
                DispatchQueue.main.async { missing = true }
                return
            }
            // Replay the stored SAN onto a fresh board, collecting the FEN
            // after every ply. A move that no longer replays truncates the
            // walk (the earlier positions remain browsable).
            var fenList: [String] = []
            var materialList: [MaterialSummaryInfo] = []
            let board: BoardHandle?
            if let startFen = decoded.game.startingFen {
                board = try? BoardHandle.fromFen(fen: startFen)
            } else {
                board = BoardHandle()
            }
            if let board {
                fenList.append(board.fen())
                materialList.append(GameViewModel.decodeMaterial(board.materialSummary()))
                for move in decoded.moves {
                    guard (try? board.playSan(san: move.san)) != nil else { break }
                    fenList.append(board.fen())
                    materialList.append(GameViewModel.decodeMaterial(board.materialSummary()))
                }
            }
            DispatchQueue.main.async {
                detail = decoded
                fens = fenList
                materials = materialList
                ply = max(0, fenList.count - 1)
            }
        }
    }
}

// MARK: - Read-only board

/// Non-interactive board for replaying past games: pieces, last-move
/// highlight, and the verdict chip — same look as the live board, no
/// selection or tap handling.
struct ReplayBoardView: View {
    let pieces: [String: Piece]
    let lastMoveSquares: Set<String>
    let verdictSquare: String?
    let verdictJudgment: String?
    /// Animated move arrows (the lexicon sheet's demo board draws the
    /// line being played).
    var arrows: [BoardArrow] = []

    private static let files = ["a", "b", "c", "d", "e", "f", "g", "h"]

    var body: some View {
        VStack(spacing: 0) {
            ForEach(Array((1...8).reversed()), id: \.self) { rank in
                HStack(spacing: 0) {
                    ForEach(0..<8, id: \.self) { file in
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
        .overlay(ArrowOverlay(arrows: arrows))
    }

    @ViewBuilder
    private func squareView(file: Int, rank: Int) -> some View {
        let name = "\(Self.files[file])\(rank)"
        // a1 dark, h1 light: even file+rank (0-indexed file, 1-indexed rank)
        // is a light square.
        let isLight = (file + rank) % 2 == 0
        let piece = pieces[name]

        ZStack {
            Rectangle()
                .fill(isLight ? Brand.boardLight : Brand.boardDark)
            if lastMoveSquares.contains(name) {
                Rectangle().fill(Color.yellow.opacity(0.35))
            }
            if let piece {
                GeometryReader { geo in
                    Text(piece.glyph)
                        .font(.system(size: geo.size.width * 0.72))
                        .foregroundStyle(piece.isWhite ? Color.white : Color.black)
                        .shadow(color: piece.isWhite ? .black.opacity(0.55) : .clear, radius: 1)
                        .frame(width: geo.size.width, height: geo.size.height)
                        .minimumScaleFactor(0.5)
                }
            }
            if verdictSquare == name, let judgment = verdictJudgment {
                let style = BoardView.verdictStyle(judgment)
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
        }
    }
}
