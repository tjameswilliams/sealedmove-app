import SwiftUI

/// The post-game walkthrough: the coach's read on the whole game, with
/// every claim anchored to a ply you can jump to.
///
/// The scrubber is the game's timeline: each review moment sits on it as a
/// dot: green when the move swung the evaluation the student's way, red
/// when it swung against them. Scrubbing onto a dot (or tapping play, which
/// walks moment to moment with time to read) shows that one moment's card;
/// the card replays the two moves leading into the moment, drawing each as
/// an arrow. The point is to SEE the moment arrive rather than be told
/// about it (a still diagram of a blunder teaches much less than watching
/// the two moves that set it up).
struct GameReviewSheet: View {
    let model: GameViewModel
    /// Which game this sheet is showing. The session builds one review at
    /// a time, so the sheet checks that the review in hand is the one it
    /// asked for before rendering it.
    var subject: GameViewModel.ReviewSubject = .liveGame
    /// How to (re)build this sheet's review.
    var request: (GameViewModel) -> Void = { $0.requestReview() }
    @Environment(\.dismiss) private var dismiss

    /// The review, but only when it is a review of THIS game.
    private var review: GameReviewInfo? {
        model.reviewSubject == subject ? model.gameReview : nil
    }

    /// Position the board shows: index into `fens` (0 = starting position).
    @State private var ply = 0
    /// FEN after i plies. Built once from the reviewed move list.
    @State private var fens: [String] = []
    /// The moment currently being walked through, if any.
    @State private var focusedMoment: ReviewMomentInfo?
    /// Arrow for the move that just landed during a replay.
    @State private var replayArrow: [BoardArrow] = []
    /// Cancels an in-flight replay when the student taps another moment.
    @State private var replayToken = 0
    /// Auto-walk through the moments is running.
    @State private var isPlaying = false
    /// The auto-walk task, so pause/scrub can cancel it.
    @State private var playTask: Task<Void, Never>?

    /// How many moves of run-up a moment replays before landing on itself.
    private static let runUpMoves = 2
    /// Beat between replayed moves. Long enough to read the arrow, short
    /// enough that a three-step replay does not feel like waiting.
    private static let replayBeat: TimeInterval = 0.85
    /// Judged moves below which a rating estimate is noise rather than
    /// information, so the report card shows the sample size instead.
    private static let ratingSampleFloor: UInt32 = 10

    var body: some View {
        NavigationStack {
            Group {
                if let review {
                    content(review)
                } else if model.isBuildingReview {
                    buildingState
                } else if let error = model.reviewError {
                    ContentUnavailableView {
                        Label("The review didn't finish", systemImage: "exclamationmark.triangle")
                    } description: {
                        Text(error)
                    } actions: {
                        Button("Try again") { request(model) }
                    }
                } else {
                    emptyState
                }
            }
            .navigationTitle("Game review")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") { dismiss() }
                }
            }
            .onAppear(perform: start)
            .onDisappear { stopPlayback() }
            .onChange(of: model.gameReview) { rebuildPositions() }
        }
    }

    // MARK: States

    private var buildingState: some View {
        VStack(spacing: 14) {
            ProgressView()
                .controlSize(.large)
            Text("Going back over the game…")
                .font(.headline)
            Text("The engine is sweeping every position, then taking a closer "
                 + "look at the moments that decided it. This takes a few moments.")
                .font(.callout)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .padding(.horizontal, 32)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private var emptyState: some View {
        ContentUnavailableView {
            Label("Nothing to review yet", systemImage: "chart.line.uptrend.xyaxis")
        } description: {
            Text("Play a few moves and the coach can walk you back through them.")
        }
    }

    // MARK: Loaded review

    @ViewBuilder
    private func content(_ review: GameReviewInfo) -> some View {
        GeometryReader { geo in
            ScrollView {
                VStack(alignment: .leading, spacing: 14) {
                    ReplayBoardView(
                        pieces: currentPieces,
                        lastMoveSquares: lastMoveSquares,
                        verdictSquare: nil,
                        verdictJudgment: nil,
                        arrows: replayArrow)
                        .frame(width: geo.size.width - 32, height: geo.size.width - 32)
                        .frame(maxWidth: .infinity)

                    scrubber(review)
                    if !review.moments.isEmpty {
                        focusedMomentSection(review)
                    }
                    verdict(review)
                    reportCard(review)
                    if !review.narrated {
                        Text("The coach model wasn't available, so these notes are the "
                             + "engine's own wording.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
                .padding(.horizontal, 16)
                .padding(.bottom, 24)
            }
        }
    }

    /// The game's timeline: a scrub track with a dot per review moment,
    /// green for a swing the student's way, red for one against them.
    /// Scrubbing onto a dot surfaces that moment's card; play walks the
    /// dots one by one. Manual scrubbing always pauses the walk, so the
    /// board never disagrees with the highlighted note.
    @ViewBuilder
    private func scrubber(_ review: GameReviewInfo) -> some View {
        VStack(spacing: 6) {
            HStack(spacing: 12) {
                Button {
                    isPlaying ? stopPlayback() : startPlayback(review)
                } label: {
                    Image(systemName: isPlaying ? "pause.circle.fill" : "play.circle.fill")
                        .font(.title)
                        .foregroundStyle(review.moments.isEmpty ? Color.secondary : Brand.tournamentGreen)
                }
                .disabled(review.moments.isEmpty)
                .accessibilityLabel(isPlaying ? "Pause the walkthrough" : "Play the walkthrough")
                .accessibilityHint("Steps through each marked moment, leaving time to read its note.")

                Button {
                    step(to: ply - 1)
                } label: {
                    Image(systemName: "chevron.backward.circle.fill").font(.title2)
                }
                .disabled(ply == 0)

                ReviewTimeline(
                    positionCount: fens.count,
                    markers: timelineMarkers(review),
                    current: ply,
                    onScrub: { step(to: $0) },
                    onScrubEnd: { snapToNearbyMoment(review, around: $0) })
                    .disabled(fens.count < 2)

                Button {
                    step(to: ply + 1)
                } label: {
                    Image(systemName: "chevron.forward.circle.fill").font(.title2)
                }
                .disabled(ply >= fens.count - 1)
            }
            .buttonStyle(.plain)
            .foregroundStyle(.primary)

            Text(plyLabel(review))
                .font(.system(.footnote, design: .monospaced))
                .foregroundStyle(.secondary)
        }
    }

    /// One timeline marker per moment, at the board position AFTER the
    /// moment's move (where its replay lands).
    private func timelineMarkers(_ review: GameReviewInfo) -> [ReviewTimeline.Marker] {
        guard fens.count > 1 else { return [] }
        return review.moments.map { moment in
            ReviewTimeline.Marker(
                id: moment.ply,
                position: min(moment.ply + 1, fens.count - 1),
                color: dotColor(moment),
                isFocused: focusedMoment?.ply == moment.ply)
        }
    }

    /// Green when the move swung the evaluation the student's way, red when
    /// it swung against them (evals are from the student's side).
    private func dotColor(_ moment: ReviewMomentInfo) -> Color {
        moment.evalAfterCp >= moment.evalBeforeCp ? Brand.tournamentGreen : .red
    }

    /// A scrub that ends near a dot snaps onto it and surfaces its card:
    /// dots are small targets, and landing one ply off would show an empty
    /// card area instead of the note the student was reaching for.
    private func snapToNearbyMoment(_ review: GameReviewInfo, around position: Int) {
        let tolerance = max(1, (fens.count - 1) / 30)
        guard let nearest = review.moments.min(by: {
            abs($0.ply + 1 - position) < abs($1.ply + 1 - position)
        }) else { return }
        let target = min(nearest.ply + 1, fens.count - 1)
        if abs(target - position) <= tolerance {
            ply = target
            focusedMoment = nearest
        }
    }

    // MARK: Focused moment (one card at a time)

    @ViewBuilder
    private func focusedMomentSection(_ review: GameReviewInfo) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text("The moments that mattered")
                    .font(.caption.bold())
                    .foregroundStyle(.secondary)
                Spacer()
                if let moment = focusedMoment,
                   let index = sortedMoments(review).firstIndex(where: { $0.ply == moment.ply }) {
                    Text("\(index + 1) of \(review.moments.count)")
                        .font(.caption2.monospacedDigit())
                        .foregroundStyle(.secondary)
                }
            }
            if let moment = focusedMoment {
                momentCard(moment)
            } else {
                HStack(spacing: 8) {
                    Circle().fill(Brand.tournamentGreen).frame(width: 8, height: 8)
                    Circle().fill(Color.red).frame(width: 8, height: 8)
                    Text("Scrub to a dot (green swung your way, red against you) or press play to walk them.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                .padding(10)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 10))
            }
        }
    }

    private func sortedMoments(_ review: GameReviewInfo) -> [ReviewMomentInfo] {
        review.moments.sorted { $0.ply < $1.ply }
    }

    // MARK: Playback (auto-walk the moments)

    /// Walk the remaining moments one by one: replay each (two moves of
    /// run-up, arrows on) and hold long enough to read its note, then move
    /// on. Starts from the next moment after the current position and wraps
    /// to the first when already at the end.
    private func startPlayback(_ review: GameReviewInfo) {
        guard !isPlaying, !fens.isEmpty else { return }
        let all = sortedMoments(review)
        let upcoming = all.filter { $0.ply + 1 > ply }
        let queue = upcoming.isEmpty ? all : upcoming
        guard !queue.isEmpty else { return }

        isPlaying = true
        playTask = Task { @MainActor in
            for moment in queue {
                guard !Task.isCancelled else { return }
                replay(moment)
                // The run-up animation, then reading time scaled to the note.
                let runUp = Double(Self.runUpMoves + 1) * Self.replayBeat + 0.5
                let dwell = min(9.0, max(3.5, Double(moment.note.count) * 0.045))
                try? await Task.sleep(nanoseconds: UInt64((runUp + dwell) * 1_000_000_000))
            }
            if !Task.isCancelled { isPlaying = false }
        }
    }

    /// Pause: stop advancing but stay exactly where the walk got to.
    private func stopPlayback() {
        playTask?.cancel()
        playTask = nil
        isPlaying = false
    }

    @ViewBuilder
    private func verdict(_ review: GameReviewInfo) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(review.headline)
                .font(.title3.bold())
                .fontDesign(.serif)
            MarkdownText(text: review.summary)
                .font(.body)
                .fontDesign(.serif)
            if let opening = review.opening {
                Text("\(opening.name) · \(opening.eco)")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    @ViewBuilder
    private func momentCard(_ moment: ReviewMomentInfo) -> some View {
        let isFocused = focusedMoment?.ply == moment.ply
        Button {
            stopPlayback()
            replay(moment)
        } label: {
            VStack(alignment: .leading, spacing: 6) {
                HStack(spacing: 6) {
                    Image(systemName: moment.kind.symbol)
                        .font(.caption)
                    Text(moment.moveLabel)
                        .font(.system(.subheadline, design: .monospaced).bold())
                    Text(moment.kind.label)
                        .font(.caption2.bold())
                        .padding(.horizontal, 6)
                        .padding(.vertical, 1)
                        .background(tint(for: moment).opacity(0.16), in: Capsule())
                    Spacer()
                    Text("\(ReviewMomentInfo.pawns(moment.evalBeforeCp)) → "
                         + ReviewMomentInfo.pawns(moment.evalAfterCp))
                        .font(.caption2.monospacedDigit())
                        .foregroundStyle(.secondary)
                }
                .foregroundStyle(tint(for: moment))

                MarkdownText(text: moment.note)
                    .font(.callout)
                    .fontDesign(.serif)
                    .foregroundStyle(.primary)
                    .frame(maxWidth: .infinity, alignment: .leading)

                if let best = moment.bestSan, moment.kind != .strength {
                    Text("Engine line: \(moment.bestLineSan.isEmpty ? best : moment.bestLineSan.joined(separator: " "))")
                        .font(.system(.caption2, design: .monospaced))
                        .foregroundStyle(.secondary)
                }
            }
            .padding(10)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(
                RoundedRectangle(cornerRadius: 10)
                    .fill(tint(for: moment).opacity(isFocused ? 0.14 : 0.06)))
            .overlay(
                RoundedRectangle(cornerRadius: 10)
                    .strokeBorder(tint(for: moment).opacity(isFocused ? 0.6 : 0),
                                  lineWidth: 1.5))
        }
        .buttonStyle(.plain)
        .accessibilityHint("Replays the moves leading into this position.")
    }

    private func tint(for moment: ReviewMomentInfo) -> Color {
        switch moment.kind {
        case .turningPoint: return Brand.annotation
        case .slip: return .orange
        case .strength: return Brand.tournamentGreen
        case .gift: return .blue
        }
    }

    @ViewBuilder
    private func reportCard(_ review: GameReviewInfo) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Grid(horizontalSpacing: 20, verticalSpacing: 8) {
                GridRow {
                    statCell("Accuracy", String(format: "%.0f%%", review.accuracy))
                    statCell("Avg. loss", String(format: "%.0f", review.acl))
                    // A rating off a handful of moves is noise, and printing
                    // it anyway invites the student to believe it.
                    if review.movesJudged >= Self.ratingSampleFloor {
                        statCell("Est. rating", "\(review.estRating)")
                    } else {
                        statCell("Moves judged", "\(review.movesJudged)")
                    }
                }
            }
            .frame(maxWidth: .infinity)

            if !review.strengths.isEmpty {
                listSection("What worked", review.strengths,
                            symbol: "checkmark.circle", tint: Brand.tournamentGreen)
            }
            if !review.weaknesses.isEmpty {
                listSection("What cost you", review.weaknesses,
                            symbol: "arrow.down.circle", tint: .orange)
            }
        }
        .padding(12)
        .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 12))
    }

    @ViewBuilder
    private func listSection(
        _ title: String, _ items: [String], symbol: String, tint: Color
    ) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            Text(title)
                .font(.caption.bold())
                .foregroundStyle(.secondary)
            ForEach(Array(items.enumerated()), id: \.offset) { _, item in
                HStack(alignment: .top, spacing: 6) {
                    Image(systemName: symbol)
                        .font(.caption2)
                        .foregroundStyle(tint)
                        .padding(.top, 2)
                    Text(item)
                        .font(.callout)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
        }
    }

    @ViewBuilder
    private func statCell(_ title: String, _ value: String) -> some View {
        VStack(spacing: 2) {
            Text(value)
                .font(.title3.monospacedDigit().bold())
            Text(title)
                .font(.caption2)
                .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity)
    }

    // MARK: Board state

    private var currentPieces: [String: Piece] {
        guard ply < fens.count else { return [:] }
        return GameViewModel.parseFenBoard(fens[ply])
    }

    private var lastMoveSquares: Set<String> {
        guard ply >= 1, ply < fens.count else { return [] }
        let before = GameViewModel.parseFenBoard(fens[ply - 1])
        let after = GameViewModel.parseFenBoard(fens[ply])
        return Set(before.keys).union(after.keys).filter { before[$0] != after[$0] }
    }

    private func plyLabel(_ review: GameReviewInfo) -> String {
        guard ply >= 1, ply - 1 < review.moves.count else { return "Starting position" }
        let number = (ply + 1) / 2
        let separator = ply.isMultiple(of: 2) ? "… " : ". "
        return "\(number)\(separator)\(review.moves[ply - 1])"
    }

    // MARK: Replay

    private func start() {
        // A review of some other game is not this sheet's review, so ask
        // for a fresh one rather than showing the wrong game. A live-game
        // review also goes stale when more moves were played after it was
        // built (review at move 12, reopen after the game ends): rebuild
        // whenever the reviewed move list no longer matches the game.
        if !model.isBuildingReview {
            if review == nil {
                request(model)
            } else if subject == .liveGame, let review, review.moves != model.moves {
                request(model)
            }
        }
        rebuildPositions()
    }

    /// Replay the reviewed move list once, keeping the FEN after every ply.
    /// A move that will not replay truncates the walk rather than failing
    /// the sheet — the earlier positions stay browsable.
    private func rebuildPositions() {
        guard let review else { return }
        let board = BoardHandle()
        var list = [board.fen()]
        for san in review.moves {
            guard (try? board.playSan(san: san)) != nil else { break }
            list.append(board.fen())
        }
        fens = list
        // Open on the game's turning point when there is one: it is the
        // reason to be here. With nothing flagged, the final position is
        // the useful place to start scrubbing back from.
        if let turning = review.moments.first(where: { $0.kind == .turningPoint })
            ?? review.moments.first {
            replay(turning)
        } else {
            ply = max(0, list.count - 1)
        }
    }

    /// Jump the board without animation (scrubber, arrows). Pauses any
    /// walkthrough in progress. Landing exactly on a moment's position
    /// surfaces its card; anywhere else clears the card area, so the board
    /// never disagrees with the note beside it.
    private func step(to target: Int) {
        stopPlayback()
        replayToken += 1
        replayArrow = []
        ply = min(max(0, target), max(0, fens.count - 1))
        focusedMoment = review?.moments.first { min($0.ply + 1, max(0, fens.count - 1)) == ply }
    }

    /// Walk into a moment: jump back a couple of moves, then play forward
    /// one move at a time with an arrow on each, landing on the move the
    /// note is about.
    private func replay(_ moment: ReviewMomentInfo) {
        guard !fens.isEmpty else { return }
        replayToken += 1
        let token = replayToken
        focusedMoment = moment

        // The moment's own move lands at ply index moment.ply + 1 (fens[0]
        // is the starting position).
        let target = min(moment.ply + 1, fens.count - 1)
        let start = max(0, target - Self.runUpMoves - 1)
        replayArrow = []
        withAnimation(.easeInOut(duration: 0.2)) { ply = start }

        for (offset, step) in ((start + 1)...max(start + 1, target)).enumerated()
        where step <= target {
            DispatchQueue.main.asyncAfter(
                deadline: .now() + Self.replayBeat * Double(offset) + 0.3
            ) {
                // A newer replay (or a scrub) supersedes this one.
                guard token == replayToken else { return }
                withAnimation(.easeInOut(duration: 0.25)) { ply = step }
                replayArrow = arrow(intoPly: step)
            }
        }
    }

    /// Arrow for the move that lands on `step`, resolved against the
    /// position it was played from.
    private func arrow(intoPly step: Int) -> [BoardArrow] {
        guard let review,
              step >= 1, step - 1 < review.moves.count, step - 1 < fens.count
        else { return [] }
        guard let arrow = MoveResolver.resolve(
            san: review.moves[step - 1], inFen: fens[step - 1]) else { return [] }
        return [arrow]
    }
}

// MARK: - Timeline

/// The review's scrub track: a thin bar spanning the whole game with a
/// colored dot per review moment and a draggable thumb. Purely positional:
/// what the positions and dots MEAN belongs to the sheet.
struct ReviewTimeline: View {
    struct Marker: Identifiable, Equatable {
        /// The moment's ply (stable identity).
        let id: Int
        /// Board-position index the dot sits at (0 = starting position).
        let position: Int
        let color: Color
        /// The sheet is currently showing this moment's card.
        let isFocused: Bool
    }

    /// Number of board positions (moves + 1).
    let positionCount: Int
    let markers: [Marker]
    /// Current board-position index.
    let current: Int
    /// Live scrub: called with the position under the finger.
    let onScrub: (Int) -> Void
    /// Drag ended at this position (the sheet may snap to a nearby dot).
    let onScrubEnd: (Int) -> Void

    private var maxIndex: Int { max(1, positionCount - 1) }

    var body: some View {
        GeometryReader { geo in
            let width = geo.size.width
            let midY = geo.size.height / 2
            ZStack {
                // Track + progress fill.
                Capsule()
                    .fill(Color(.tertiarySystemFill))
                    .frame(height: 4)
                    .position(x: width / 2, y: midY)
                Capsule()
                    .fill(Color(.systemGray2))
                    .frame(width: max(4, x(of: current, in: width)), height: 4)
                    .position(x: max(4, x(of: current, in: width)) / 2, y: midY)

                // Moment dots. The focused dot grows and gets a ring so the
                // student can see which card the board is on.
                ForEach(markers) { marker in
                    Circle()
                        .fill(marker.color)
                        .frame(width: marker.isFocused ? 13 : 9,
                               height: marker.isFocused ? 13 : 9)
                        .overlay(
                            Circle().strokeBorder(
                                marker.color.opacity(marker.isFocused ? 0.35 : 0),
                                lineWidth: 3)
                                .frame(width: 19, height: 19))
                        .position(x: x(of: marker.position, in: width), y: midY)
                        .animation(.easeInOut(duration: 0.15), value: marker.isFocused)
                }

                // Thumb.
                Circle()
                    .fill(Color(.systemBackground))
                    .frame(width: 16, height: 16)
                    .shadow(color: .black.opacity(0.25), radius: 2, y: 1)
                    .overlay(Circle().strokeBorder(Color(.systemGray3), lineWidth: 0.5))
                    .position(x: x(of: current, in: width), y: midY)
            }
            .contentShape(Rectangle())
            .gesture(
                DragGesture(minimumDistance: 0)
                    .onChanged { value in
                        onScrub(position(at: value.location.x, in: width))
                    }
                    .onEnded { value in
                        onScrubEnd(position(at: value.location.x, in: width))
                    }
            )
        }
        .frame(height: 30)
        .accessibilityElement()
        .accessibilityLabel("Game timeline")
        .accessibilityValue("Position \(current) of \(maxIndex)")
        .accessibilityAdjustableAction { direction in
            switch direction {
            case .increment: onScrub(min(current + 1, maxIndex))
            case .decrement: onScrub(max(current - 1, 0))
            @unknown default: break
            }
        }
    }

    private func x(of position: Int, in width: CGFloat) -> CGFloat {
        CGFloat(position) / CGFloat(maxIndex) * width
    }

    private func position(at locationX: CGFloat, in width: CGFloat) -> Int {
        let fraction = min(1, max(0, locationX / max(1, width)))
        return Int((fraction * CGFloat(maxIndex)).rounded())
    }
}
