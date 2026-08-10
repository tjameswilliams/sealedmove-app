import SwiftUI

/// Browsable library of the curated lexicon: openings, tactics, and
/// concepts grouped by category, searchable, each opening onto its
/// animated textbook card.
struct LexiconBrowser: View {
    @Environment(\.dismiss) private var dismiss
    @State private var query = ""

    private var entries: [LexiconEntry] { Lexicon.shared?.entries ?? [] }

    private var filtered: [LexiconEntry] {
        let trimmed = query.trimmingCharacters(in: .whitespaces)
        guard !trimmed.isEmpty else { return entries }
        return entries.filter {
            $0.name.localizedCaseInsensitiveContains(trimmed)
                || $0.aliases.contains { alias in
                    alias.localizedCaseInsensitiveContains(trimmed)
                }
        }
    }

    private func section(_ category: LexiconEntry.Category) -> [LexiconEntry] {
        filtered.filter { $0.category == category }
            .sorted { $0.name < $1.name }
    }

    var body: some View {
        NavigationStack {
            List {
                ForEach([LexiconEntry.Category.opening, .tactic, .concept], id: \.self) { category in
                    let items = section(category)
                    if !items.isEmpty {
                        Section(category.label + "s") {
                            ForEach(items) { entry in
                                NavigationLink {
                                    LexiconSheet(entry: entry, showsClose: false)
                                        .navigationBarTitleDisplayMode(.inline)
                                } label: {
                                    HStack {
                                        Text(entry.name)
                                        Spacer()
                                        if let eco = entry.eco {
                                            Text(eco)
                                                .font(.caption.monospaced())
                                                .foregroundStyle(.secondary)
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            .searchable(text: $query, prompt: "Openings, tactics, concepts…")
            .navigationTitle("Chess Lexicon")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") { dismiss() }
                }
            }
        }
    }
}

/// Lexicon term sheet: the textbook card for an opening or concept the
/// coach mentioned — an auto-playing animated board for the defining
/// line, the definition, and strengths/weaknesses.
struct LexiconSheet: View {
    let entry: LexiconEntry
    /// Standalone sheet presentation shows its own close button; pushed
    /// from the browser it relies on the navigation bar instead.
    var showsClose = true
    @Environment(\.dismiss) private var dismiss

    /// Board snapshots for plies 0...moves.count, precomputed once.
    private let frames: [Frame]
    /// How many plies of the demo line are on the board.
    @State private var shownPlies = 0
    @State private var isPlaying: Bool

    struct Frame {
        let pieces: [String: Piece]
        let lastMove: Set<String>
        let arrow: BoardArrow?
    }

    init(entry: LexiconEntry, showsClose: Bool = true) {
        self.entry = entry
        self.showsClose = showsClose
        frames = Self.buildFrames(entry: entry)
        _isPlaying = State(initialValue: entry.moves.count > 1)
    }

    private var hasBoard: Bool { frames.count > 1 || entry.startFen != nil }
    private var frame: Frame {
        frames[min(shownPlies, frames.count - 1)]
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                header

                if hasBoard {
                    ReplayBoardView(
                        pieces: frame.pieces,
                        lastMoveSquares: frame.lastMove,
                        verdictSquare: nil,
                        verdictJudgment: nil,
                        arrows: frame.arrow.map { [$0] } ?? [])
                        .frame(maxWidth: 340)
                        .frame(maxWidth: .infinity)

                    if let caption = entry.caption {
                        Text(caption)
                            .font(.footnote.italic())
                            .foregroundStyle(.secondary)
                            .frame(maxWidth: .infinity, alignment: .center)
                            .multilineTextAlignment(.center)
                    }

                    if frames.count > 1 {
                        playbackControls
                        moveTicker
                    }
                }

                Text(entry.definition)
                    .font(.body)
                    .fixedSize(horizontal: false, vertical: true)

                if !entry.strengths.isEmpty {
                    pointsSection(title: "Strengths", points: entry.strengths,
                                  symbol: "checkmark.circle.fill", color: .green)
                }
                if !entry.weaknesses.isEmpty {
                    pointsSection(title: "Weaknesses", points: entry.weaknesses,
                                  symbol: "xmark.circle.fill", color: .red)
                }

                if entry.slug.hasPrefix("eco:") {
                    Text("Line from the lichess chess-openings database (CC0).")
                        .font(.caption2)
                        .foregroundStyle(.tertiary)
                }
            }
            .padding(20)
        }
        .task(id: isPlaying) { await autoplay() }
    }

    // MARK: Header

    private var header: some View {
        HStack(alignment: .top, spacing: 8) {
            VStack(alignment: .leading, spacing: 6) {
                HStack(spacing: 6) {
                    Text(entry.category.label)
                        .font(.caption2.bold())
                        .padding(.horizontal, 8)
                        .padding(.vertical, 2)
                        .background(Color.indigo.opacity(0.15), in: Capsule())
                        .foregroundStyle(.indigo)
                    if let eco = entry.eco {
                        Text(eco)
                            .font(.caption2.bold().monospaced())
                            .padding(.horizontal, 8)
                            .padding(.vertical, 2)
                            .background(Color(.tertiarySystemFill), in: Capsule())
                            .foregroundStyle(.secondary)
                    }
                }
                Text(entry.name)
                    .font(.title2.bold())
            }
            Spacer()
            if showsClose {
                Button {
                    dismiss()
                } label: {
                    Image(systemName: "xmark.circle.fill")
                        .font(.title2)
                        .foregroundStyle(.secondary)
                }
                .accessibilityLabel("Close")
            }
        }
    }

    // MARK: Playback

    private var playbackControls: some View {
        HStack(spacing: 26) {
            Button {
                isPlaying = false
                shownPlies = 0
            } label: {
                Image(systemName: "backward.end.fill")
            }
            .disabled(shownPlies == 0)

            Button {
                isPlaying = false
                shownPlies = max(0, shownPlies - 1)
            } label: {
                Image(systemName: "backward.frame.fill")
            }
            .disabled(shownPlies == 0)

            Button {
                if !isPlaying && shownPlies >= frames.count - 1 {
                    shownPlies = 0
                }
                isPlaying.toggle()
            } label: {
                Image(systemName: isPlaying ? "pause.circle.fill" : "play.circle.fill")
                    .font(.title)
            }

            Button {
                isPlaying = false
                shownPlies = min(frames.count - 1, shownPlies + 1)
            } label: {
                Image(systemName: "forward.frame.fill")
            }
            .disabled(shownPlies >= frames.count - 1)

            Button {
                isPlaying = false
                shownPlies = frames.count - 1
            } label: {
                Image(systemName: "forward.end.fill")
            }
            .disabled(shownPlies >= frames.count - 1)
        }
        .font(.title3)
        .frame(maxWidth: .infinity)
        .foregroundStyle(.teal)
    }

    /// SAN chips for the demo line; the ply on the board is highlighted,
    /// and tapping a chip jumps there.
    private var moveTicker: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 4) {
                ForEach(Array(entry.moves.enumerated()), id: \.offset) { index, san in
                    let ply = index + 1
                    let label = ply.isMultiple(of: 2) ? san : "\((ply + 1) / 2). \(san)"
                    Button {
                        isPlaying = false
                        shownPlies = ply
                    } label: {
                        Text(label)
                            .font(.system(.footnote, design: .monospaced))
                            .padding(.horizontal, 8)
                            .padding(.vertical, 4)
                            .background(
                                shownPlies == ply ? Color.teal.opacity(0.25) : Color(.tertiarySystemFill),
                                in: RoundedRectangle(cornerRadius: 6))
                            .foregroundStyle(shownPlies == ply ? Color.teal : Color.primary)
                    }
                    .buttonStyle(.plain)
                }
            }
            .padding(.vertical, 2)
        }
    }

    /// Advance one ply at a time; loop back to the start after a pause at
    /// the end of the line.
    private func autoplay() async {
        guard isPlaying else { return }
        while isPlaying, !Task.isCancelled {
            try? await Task.sleep(for: .milliseconds(shownPlies == 0 ? 500 : 1100))
            guard isPlaying, !Task.isCancelled else { return }
            if shownPlies < frames.count - 1 {
                withAnimation(.easeInOut(duration: 0.2)) { shownPlies += 1 }
            } else {
                try? await Task.sleep(for: .milliseconds(1800))
                guard isPlaying, !Task.isCancelled else { return }
                withAnimation(.easeInOut(duration: 0.2)) { shownPlies = 0 }
            }
        }
    }

    // MARK: Sections

    @ViewBuilder
    private func pointsSection(title: String, points: [String],
                               symbol: String, color: Color) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(title)
                .font(.subheadline.bold())
            ForEach(points, id: \.self) { point in
                HStack(alignment: .top, spacing: 8) {
                    Image(systemName: symbol)
                        .font(.footnote)
                        .foregroundStyle(color)
                        .padding(.top, 2)
                    Text(point)
                        .font(.callout)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(color.opacity(0.06), in: RoundedRectangle(cornerRadius: 10))
    }

    // MARK: Frames

    /// Replay the demo line once, capturing the board after every ply
    /// (pieces, changed squares, and the move's arrow).
    private static func buildFrames(entry: LexiconEntry) -> [Frame] {
        let board: BoardHandle
        if let fen = entry.startFen, let custom = try? BoardHandle.fromFen(fen: fen) {
            board = custom
        } else {
            board = BoardHandle()
        }
        var frames = [Frame(pieces: GameViewModel.parseFenBoard(board.fen()),
                            lastMove: [], arrow: nil)]
        for san in entry.moves {
            let fenBefore = board.fen()
            let before = GameViewModel.parseFenBoard(fenBefore)
            guard (try? board.playSan(san: san)) != nil else { break }
            let after = GameViewModel.parseFenBoard(board.fen())
            let changed = Set(before.keys).union(after.keys)
                .filter { before[$0] != after[$0] }
            frames.append(Frame(
                pieces: after,
                lastMove: changed,
                arrow: MoveResolver.resolve(san: san, inFen: fenBefore)))
        }
        return frames
    }
}
