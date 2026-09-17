import SwiftUI

/// The export flow: pick a slice of the game on the timeline, choose how
/// moves are marked (highlights or arrows) and whether the coach's notes
/// ride along, then render an .mp4 and hand it to the share sheet.
struct ExportVideoSheet: View {
    let game: ExportableGame
    @Environment(\.dismiss) private var dismiss

    @State private var options: VideoExportOptions
    @State private var isExporting = false
    @State private var progress: Double = 0
    @State private var exportedURL: URL?
    @State private var errorMessage: String?
    @State private var exportTask: Task<Void, Never>?

    init(game: ExportableGame) {
        self.game = game
        _options = State(initialValue: VideoExportOptions(
            startPly: 0, endPly: max(1, game.fens.count - 1)))
    }

    private var maxPly: Int { max(1, game.fens.count - 1) }

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    boardPreview
                    rangeSection
                    optionsSection
                    exportSection
                }
                .padding(.horizontal, 16)
                .padding(.bottom, 24)
            }
            .navigationTitle("Export video")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") { dismiss() }
                }
            }
            .onDisappear { exportTask?.cancel() }
            .onChange(of: options) {
                // Changed options make a finished export stale.
                exportedURL = nil
            }
        }
    }

    // MARK: Preview board (the end of the selected range)

    private var boardPreview: some View {
        ReplayBoardView(
            pieces: GameViewModel.parseFenBoard(game.fens[options.endPly]),
            lastMoveSquares: options.moveStyle == .highlights ? previewChangedSquares : [],
            verdictSquare: nil,
            verdictJudgment: nil,
            arrows: options.moveStyle == .arrows ? previewArrow : [],
            arrowProgress: 1)
            .frame(maxWidth: .infinity)
    }

    private var previewChangedSquares: Set<String> {
        guard options.endPly >= 1 else { return [] }
        let before = GameViewModel.parseFenBoard(game.fens[options.endPly - 1])
        let after = GameViewModel.parseFenBoard(game.fens[options.endPly])
        return Set(before.keys).union(after.keys).filter { before[$0] != after[$0] }
    }

    private var previewArrow: [BoardArrow] {
        guard options.endPly >= 1, options.endPly - 1 < game.moves.count,
              let arrow = MoveResolver.resolve(
                  san: game.moves[options.endPly - 1],
                  inFen: game.fens[options.endPly - 1])
        else { return [] }
        return [arrow]
    }

    // MARK: Range

    private var rangeSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("What to export")
                .font(.caption.bold())
                .foregroundStyle(.secondary)
            ExportRangeTimeline(
                positionCount: game.fens.count,
                markers: timelineMarkers,
                start: $options.startPly,
                end: $options.endPly)
            HStack {
                Text("\(label(at: options.startPly)) to \(label(at: options.endPly))")
                    .font(.system(.footnote, design: .monospaced))
                Spacer()
                Text(durationText)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
            Button("Whole game") {
                options.startPly = 0
                options.endPly = maxPly
            }
            .font(.footnote)
            .disabled(options.startPly == 0 && options.endPly == maxPly)
        }
    }

    private var timelineMarkers: [ReviewTimeline.Marker] {
        game.moments.map { moment in
            ReviewTimeline.Marker(
                id: moment.ply,
                position: min(moment.ply + 1, maxPly),
                color: moment.evalAfterCp >= moment.evalBeforeCp
                    ? Brand.tournamentGreen : .red,
                isFocused: false)
        }
    }

    private func label(at ply: Int) -> String {
        guard ply >= 1, ply - 1 < game.moves.count else { return "start" }
        let number = (ply + 1) / 2
        let separator = ply.isMultiple(of: 2) ? "… " : ". "
        return "\(number)\(separator)\(game.moves[ply - 1])"
    }

    private var durationText: String {
        let seconds = Int(ExportFrameBuilder.estimatedDuration(
            game: game, options: options).rounded())
        if seconds < 60 { return "about \(seconds)s" }
        return "about \(seconds / 60)m \(seconds % 60)s"
    }

    // MARK: Options

    private var optionsSection: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("How moves are shown")
                .font(.caption.bold())
                .foregroundStyle(.secondary)
            Picker("Move style", selection: $options.moveStyle) {
                ForEach(VideoExportOptions.MoveStyle.allCases) { style in
                    Text(style.label).tag(style)
                }
            }
            .pickerStyle(.segmented)
            Toggle(isOn: $options.includeCommentary) {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Coach commentary")
                    Text("Holds on each marked moment with the coach's note.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            .disabled(game.moments.isEmpty)
            if game.moments.isEmpty {
                Text("This review has no marked moments, so there is no commentary to embed.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
    }

    // MARK: Export

    @ViewBuilder
    private var exportSection: some View {
        VStack(spacing: 10) {
            if let exportedURL {
                ShareLink(item: exportedURL) {
                    Label("Share video", systemImage: "square.and.arrow.up")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .tint(Brand.tournamentGreen)
                Text("Ready. Share it, or save it to your photo library from the share sheet.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } else if isExporting {
                ProgressView(value: progress) {
                    Text("Rendering the game…")
                        .font(.footnote)
                }
                Button("Cancel", role: .cancel) {
                    exportTask?.cancel()
                }
                .font(.footnote)
            } else {
                Button {
                    startExport()
                } label: {
                    Label("Export video", systemImage: "film")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .tint(Brand.tournamentGreen)
                .disabled(options.endPly <= options.startPly)
            }
            if let errorMessage {
                Text(errorMessage)
                    .font(.caption)
                    .foregroundStyle(.red)
            }
        }
        .padding(.top, 6)
    }

    private func startExport() {
        errorMessage = nil
        progress = 0
        isExporting = true
        exportTask = Task { @MainActor in
            do {
                let frames = ExportFrameBuilder.frames(game: game, options: options)
                let url = try await GameVideoExporter.export(frames: frames) {
                    progress = $0
                }
                exportedURL = url
            } catch is CancellationError {
                // The student backed out; nothing to report.
            } catch {
                errorMessage = error.localizedDescription
            }
            isExporting = false
        }
    }
}

// MARK: - Range timeline

/// The review timeline's two-thumb sibling: same track and moment dots,
/// with start and end thumbs bounding the exported slice. A drag grabs
/// whichever thumb is closer when it begins and keeps it for the whole
/// gesture, so the thumbs never swap mid-drag.
struct ExportRangeTimeline: View {
    let positionCount: Int
    let markers: [ReviewTimeline.Marker]
    @Binding var start: Int
    @Binding var end: Int

    @State private var draggingEnd: Bool?

    private var maxIndex: Int { max(1, positionCount - 1) }

    var body: some View {
        GeometryReader { geo in
            let width = geo.size.width
            let midY = geo.size.height / 2
            ZStack {
                Capsule()
                    .fill(Color(.tertiarySystemFill))
                    .frame(height: 4)
                    .position(x: width / 2, y: midY)
                // Selected slice.
                Capsule()
                    .fill(Brand.tournamentGreen.opacity(0.55))
                    .frame(width: max(4, x(of: end, in: width) - x(of: start, in: width)),
                           height: 6)
                    .position(x: (x(of: start, in: width) + x(of: end, in: width)) / 2,
                              y: midY)

                ForEach(markers) { marker in
                    let inRange = marker.position >= start && marker.position <= end
                    Circle()
                        .fill(marker.color.opacity(inRange ? 1 : 0.3))
                        .frame(width: 9, height: 9)
                        .position(x: x(of: marker.position, in: width), y: midY)
                }

                thumb.position(x: x(of: start, in: width), y: midY)
                thumb.position(x: x(of: end, in: width), y: midY)
            }
            .contentShape(Rectangle())
            .gesture(
                DragGesture(minimumDistance: 0)
                    .onChanged { value in
                        let target = position(at: value.location.x, in: width)
                        if draggingEnd == nil {
                            draggingEnd = abs(target - end) < abs(target - start)
                                || (abs(target - end) == abs(target - start) && target > end)
                        }
                        if draggingEnd == true {
                            end = min(max(target, start + 1), maxIndex)
                        } else {
                            start = max(min(target, end - 1), 0)
                        }
                    }
                    .onEnded { _ in draggingEnd = nil }
            )
        }
        .frame(height: 34)
        .accessibilityElement()
        .accessibilityLabel("Export range")
        .accessibilityValue("From position \(start) to position \(end) of \(maxIndex)")
    }

    private var thumb: some View {
        Circle()
            .fill(Color(.systemBackground))
            .frame(width: 18, height: 18)
            .shadow(color: .black.opacity(0.25), radius: 2, y: 1)
            .overlay(Circle().strokeBorder(Brand.tournamentGreen, lineWidth: 2))
    }

    private func x(of position: Int, in width: CGFloat) -> CGFloat {
        CGFloat(position) / CGFloat(maxIndex) * width
    }

    private func position(at locationX: CGFloat, in width: CGFloat) -> Int {
        let fraction = min(1, max(0, locationX / max(1, width)))
        return Int((fraction * CGFloat(maxIndex)).rounded())
    }
}
