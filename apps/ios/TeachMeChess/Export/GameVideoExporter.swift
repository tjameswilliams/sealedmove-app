import SwiftUI
import AVFoundation

// MARK: - What gets exported

/// A finished game in the shape the video exporter needs: the move list,
/// the position after every ply, and the review's anchored notes. Built by
/// whoever opens the export sheet (the review sheet already has all of it).
struct ExportableGame {
    let moves: [String]
    /// FEN after i plies; fens[0] is the starting position.
    let fens: [String]
    let moments: [ReviewMomentInfo]
    let headline: String
    let accuracy: Double
    let acl: Double
    let estRating: UInt32
    let movesJudged: UInt32
    let openingName: String?
    /// Precomputed "12… Nc6" labels, one per move — set when the game does
    /// not start from move one as White (scanned positions), where deriving
    /// numbering from the ply index would be wrong. Nil = derive.
    let moveLabels: [String]?
    /// A puzzle solution rather than a reviewed game: the closing frame is
    /// a signature card instead of the accuracy report.
    let isPuzzle: Bool

    init(review: GameReviewInfo, fens: [String]) {
        self.moves = review.moves
        self.fens = fens
        self.moments = review.moments
        self.headline = review.headline
        self.accuracy = review.accuracy
        self.acl = review.acl
        self.estRating = review.estRating
        self.movesJudged = review.movesJudged
        self.openingName = review.opening.map { "\($0.name) · \($0.eco)" }
        self.moveLabels = nil
        self.isPuzzle = false
    }

    /// The engine's solution to a scanned position. `subtitle` rides in the
    /// opening-name slot of the title card ("Scanned puzzle · White to move").
    init(solutionMoves: [String], fens: [String], moveLabels: [String],
         headline: String, subtitle: String?) {
        self.moves = solutionMoves
        self.fens = fens
        self.moments = []
        self.headline = headline
        self.accuracy = 0
        self.acl = 0
        self.estRating = 0
        self.movesJudged = 0
        self.openingName = subtitle
        self.moveLabels = moveLabels
        self.isPuzzle = true
    }

    /// "12. Nf3" / "12… Nc6" for the 1-based `ply`, honouring the
    /// precomputed labels when the game starts mid-count.
    func label(intoPly ply: Int) -> String? {
        guard ply >= 1, ply - 1 < moves.count else { return nil }
        if let moveLabels, ply - 1 < moveLabels.count { return moveLabels[ply - 1] }
        let number = (ply + 1) / 2
        let separator = ply.isMultiple(of: 2) ? "… " : ". "
        return "\(number)\(separator)\(moves[ply - 1])"
    }
}

/// Choices the student makes in the export sheet.
struct VideoExportOptions: Equatable {
    enum MoveStyle: String, CaseIterable, Identifiable {
        /// The board's usual yellow wash on the from/to squares.
        case highlights
        /// The coach's red annotation arrow drawn for each move.
        case arrows

        var id: String { rawValue }
        var label: String {
            switch self {
            case .highlights: return "Highlights"
            case .arrows: return "Arrows"
            }
        }
    }

    var moveStyle: MoveStyle = .highlights
    var includeCommentary = true
    /// Position range to export, as indices into `fens`
    /// (startPly..endPly, startPly < endPly).
    var startPly: Int
    var endPly: Int
}

// MARK: - Frames

/// One rendered state of the video. `hold` is how long the frame stays on
/// screen after any arrow animation finishes.
struct ExportFrame {
    enum Content {
        case title(opening: String?, headline: String)
        case position(PositionFrame)
        case report(accuracy: Double, acl: Double, estRating: UInt32,
                    movesJudged: UInt32)
    }

    struct PositionFrame {
        let pieces: [String: Piece]
        let lastMoveSquares: Set<String>
        let arrow: BoardArrow?
        let verdictSquare: String?
        let verdictJudgment: String?
        /// "12. Nf3" / "12… Nc6"; nil on the range's opening frame.
        let moveLabel: String?
        /// The review note anchored to this move, when commentary is on.
        let moment: ReviewMomentInfo?
    }

    let content: Content
    let hold: TimeInterval
}

/// Turns a game plus options into the deterministic frame list the encoder
/// walks. Pure and synchronous; the only non-trivial work is resolving each
/// SAN to arrow endpoints on a throwaway board.
enum ExportFrameBuilder {
    /// Seconds an arrow takes to draw itself in (arrows style only).
    static let arrowGrowth: TimeInterval = 0.28
    /// Sub-frames rendered during the arrow growth.
    static let arrowSteps = 7

    static func frames(game: ExportableGame, options: VideoExportOptions) -> [ExportFrame] {
        var frames: [ExportFrame] = []
        let start = max(0, min(options.startPly, game.fens.count - 1))
        let end = max(start, min(options.endPly, game.fens.count - 1))

        frames.append(ExportFrame(
            content: .title(opening: game.openingName, headline: game.headline),
            hold: 2.2))

        // The range's opening position, so the viewer sees where the story
        // starts before the first move lands.
        frames.append(ExportFrame(
            content: .position(PositionFrame(
                pieces: GameViewModel.parseFenBoard(game.fens[start]),
                lastMoveSquares: [],
                arrow: nil,
                verdictSquare: nil,
                verdictJudgment: nil,
                moveLabel: start == 0 ? nil : game.label(intoPly: start),
                moment: nil)),
            hold: 1.2))

        for ply in (start + 1)...max(start + 1, end) where ply <= end {
            let moment = game.moments.first { $0.ply == ply - 1 }
            let before = GameViewModel.parseFenBoard(game.fens[ply - 1])
            let after = GameViewModel.parseFenBoard(game.fens[ply])
            let changed = Set(before.keys).union(after.keys)
                .filter { before[$0] != after[$0] }

            let arrow: BoardArrow?
            switch options.moveStyle {
            case .arrows:
                arrow = MoveResolver.resolve(
                    san: game.moves[ply - 1], inFen: game.fens[ply - 1])
            case .highlights:
                arrow = nil
            }

            let shownMoment = options.includeCommentary ? moment : nil
            frames.append(ExportFrame(
                content: .position(PositionFrame(
                    pieces: after,
                    lastMoveSquares: options.moveStyle == .highlights ? changed : [],
                    arrow: arrow,
                    verdictSquare: moment.flatMap {
                        MoveResolver.destinationSquare(of: $0.san)
                    },
                    verdictJudgment: moment?.judgment,
                    moveLabel: game.label(intoPly: ply),
                    moment: shownMoment)),
                hold: hold(for: moment, showingNote: options.includeCommentary)))
        }

        if game.isPuzzle {
            // A solution has no accuracy story to report — close on a
            // signature card instead.
            frames.append(ExportFrame(
                content: .title(opening: "Solved on Sealed Move",
                                headline: game.headline),
                hold: 3.0))
        } else {
            frames.append(ExportFrame(
                content: .report(accuracy: game.accuracy, acl: game.acl,
                                 estRating: game.estRating,
                                 movesJudged: game.movesJudged),
                hold: 3.0))
        }
        return frames
    }

    typealias PositionFrame = ExportFrame.PositionFrame

    /// Total video length for the current options, for the sheet's
    /// "about 40 seconds" line. Skips arrow resolution, so it is cheap
    /// enough to recompute on every option change.
    static func estimatedDuration(game: ExportableGame, options: VideoExportOptions) -> TimeInterval {
        let start = max(0, min(options.startPly, game.fens.count - 1))
        let end = max(start, min(options.endPly, game.fens.count - 1))
        var total: TimeInterval = 2.2 + 1.2 + 3.0
        let perMoveGrowth = options.moveStyle == .arrows ? arrowGrowth : 0
        for ply in (start + 1)...max(start + 1, end) where ply <= end {
            let moment = game.moments.first { $0.ply == ply - 1 }
            total += hold(for: moment, showingNote: options.includeCommentary) + perMoveGrowth
        }
        return total
    }

    /// The review sheet's tuned reading-time heuristic, reused so the video
    /// paces the way the walkthrough does: quick beats between moves, a
    /// note-length dwell on the moments that matter.
    private static func hold(for moment: ReviewMomentInfo?, showingNote: Bool) -> TimeInterval {
        guard let moment else { return 1.0 }
        guard showingNote else { return 1.5 }
        return min(8.0, max(3.5, Double(moment.note.count) * 0.045))
    }

}

// MARK: - Frame view (the video's canvas)

/// One video frame, laid out on a fixed 1080x1920 canvas and rendered
/// offscreen with `ImageRenderer`. Everything here must be deterministic:
/// no `.onAppear` animation, no phase animators, explicit colors only (the
/// canvas never sees the app's light/dark environment).
struct ExportFrameView: View {
    let frame: ExportFrame
    /// Arrow draw-in progress for this render (1 = fully drawn).
    var arrowProgress: CGFloat = 1

    static let canvas = CGSize(width: 1080, height: 1920)

    /// The brand's annotated-game paper, a touch lighter than the light
    /// squares so the board reads as an object sitting on the page.
    private static let paper = Color(red: 0.965, green: 0.941, blue: 0.874)
    private static let ink = Color(red: 0.145, green: 0.129, blue: 0.106)
    private static let fadedInk = Color(red: 0.145, green: 0.129, blue: 0.106).opacity(0.6)

    var body: some View {
        ZStack {
            Self.paper
            switch frame.content {
            case .title(let opening, let headline):
                titleCard(opening: opening, headline: headline)
            case .position(let position):
                positionFrame(position)
            case .report(let accuracy, let acl, let estRating, let movesJudged):
                reportCard(accuracy: accuracy, acl: acl,
                           estRating: estRating, movesJudged: movesJudged)
            }
        }
        .frame(width: Self.canvas.width, height: Self.canvas.height)
        .environment(\.colorScheme, .light)
    }

    private var wordmark: some View {
        Text("SEALED MOVE")
            .font(.system(size: 30, weight: .semibold))
            .kerning(9)
            .foregroundStyle(Self.fadedInk)
    }

    @ViewBuilder
    private func titleCard(opening: String?, headline: String) -> some View {
        VStack(spacing: 44) {
            wordmark
            Text(headline)
                .font(.system(size: 72, weight: .bold, design: .serif))
                .foregroundStyle(Self.ink)
                .multilineTextAlignment(.center)
                .minimumScaleFactor(0.6)
            if let opening {
                Text(opening)
                    .font(.system(size: 40, design: .serif).italic())
                    .foregroundStyle(Self.fadedInk)
                    .multilineTextAlignment(.center)
            }
        }
        .padding(.horizontal, 90)
    }

    @ViewBuilder
    private func positionFrame(_ position: ExportFrame.PositionFrame) -> some View {
        VStack(spacing: 0) {
            wordmark
                .padding(.top, 66)
            Text(position.moveLabel ?? "Starting position")
                .font(.system(size: 56, weight: .bold, design: .monospaced))
                .foregroundStyle(Self.ink)
                .padding(.top, 40)
                .padding(.bottom, 36)
            ReplayBoardView(
                pieces: position.pieces,
                lastMoveSquares: position.lastMoveSquares,
                verdictSquare: position.verdictSquare,
                verdictJudgment: position.verdictJudgment,
                arrows: position.arrow.map { [$0] } ?? [],
                arrowProgress: arrowProgress)
                .frame(width: 984, height: 984)
            if let moment = position.moment {
                commentaryCard(moment)
                    .padding(.top, 44)
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 48)
    }

    @ViewBuilder
    private func commentaryCard(_ moment: ReviewMomentInfo) -> some View {
        VStack(alignment: .leading, spacing: 18) {
            HStack(spacing: 14) {
                Image(systemName: moment.kind.symbol)
                    .font(.system(size: 34, weight: .semibold))
                Text(moment.kind.label)
                    .font(.system(size: 34, weight: .bold))
                Spacer()
                Text("\(ReviewMomentInfo.pawns(moment.evalBeforeCp)) → "
                     + ReviewMomentInfo.pawns(moment.evalAfterCp))
                    .font(.system(size: 32, design: .monospaced))
                    .foregroundStyle(Self.fadedInk)
            }
            .foregroundStyle(tint(for: moment))
            Text(Self.plainText(moment.note))
                .font(.system(size: 40, design: .serif))
                .foregroundStyle(Self.ink)
                .lineSpacing(8)
                .frame(maxWidth: .infinity, alignment: .leading)
                .lineLimit(9)
                .minimumScaleFactor(0.62)
        }
        .padding(36)
        .frame(maxWidth: .infinity, alignment: .topLeading)
        .background(
            RoundedRectangle(cornerRadius: 28)
                .fill(tint(for: moment).opacity(0.08)))
        .overlay(
            RoundedRectangle(cornerRadius: 28)
                .strokeBorder(tint(for: moment).opacity(0.35), lineWidth: 3))
    }

    @ViewBuilder
    private func reportCard(accuracy: Double, acl: Double,
                            estRating: UInt32, movesJudged: UInt32) -> some View {
        VStack(spacing: 70) {
            wordmark
            Text("Report card")
                .font(.system(size: 76, weight: .bold, design: .serif))
                .foregroundStyle(Self.ink)
            HStack(spacing: 60) {
                reportStat(String(format: "%.0f%%", accuracy), "Accuracy")
                reportStat(String(format: "%.0f", acl), "Avg. loss")
                if movesJudged >= 10 {
                    reportStat("\(estRating)", "Est. rating")
                } else {
                    reportStat("\(movesJudged)", "Moves judged")
                }
            }
            Text("Reviewed with the coach on Sealed Move")
                .font(.system(size: 36, design: .serif).italic())
                .foregroundStyle(Self.fadedInk)
        }
        .padding(.horizontal, 80)
    }

    @ViewBuilder
    private func reportStat(_ value: String, _ title: String) -> some View {
        VStack(spacing: 10) {
            Text(value)
                .font(.system(size: 84, weight: .bold, design: .monospaced))
                .foregroundStyle(Self.ink)
            Text(title)
                .font(.system(size: 32))
                .foregroundStyle(Self.fadedInk)
        }
    }

    private func tint(for moment: ReviewMomentInfo) -> Color {
        switch moment.kind {
        case .turningPoint: return Brand.annotation
        case .slip: return .orange
        case .strength: return Brand.tournamentGreen
        case .gift: return .blue
        }
    }

    /// The coach writes light markdown; the video card wants plain prose.
    /// Strips emphasis and code markers rather than parsing blocks.
    static func plainText(_ markdown: String) -> String {
        markdown
            .replacingOccurrences(of: "**", with: "")
            .replacingOccurrences(of: "`", with: "")
            .replacingOccurrences(of: "\n\n", with: "\n")
    }
}

// MARK: - Encoder

/// Renders the frame list offscreen and writes an H.264 .mp4 with variable
/// frame durations: one sample per board state, plus a short burst of
/// samples while an arrow draws itself in. Runs on the main actor because
/// `ImageRenderer` requires it; yields between frames so the progress UI
/// stays live.
enum GameVideoExporter {
    enum ExportError: LocalizedError {
        case renderFailed
        case writerFailed(String)

        var errorDescription: String? {
            switch self {
            case .renderFailed:
                return "A frame could not be rendered."
            case .writerFailed(let detail):
                return "The video could not be written: \(detail)"
            }
        }
    }

    private static let timescale: CMTimeScale = 600

    @MainActor
    static func export(
        frames: [ExportFrame],
        progress: @escaping (Double) -> Void
    ) async throws -> URL {
        let size = ExportFrameView.canvas
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("sealed-move-game-\(UUID().uuidString).mp4")

        let writer = try AVAssetWriter(outputURL: url, fileType: .mp4)
        let input = AVAssetWriterInput(mediaType: .video, outputSettings: [
            AVVideoCodecKey: AVVideoCodecType.h264,
            AVVideoWidthKey: Int(size.width),
            AVVideoHeightKey: Int(size.height),
            AVVideoCompressionPropertiesKey: [
                AVVideoAverageBitRateKey: 9_000_000,
                AVVideoProfileLevelKey: AVVideoProfileLevelH264HighAutoLevel,
            ],
        ])
        input.expectsMediaDataInRealTime = false
        let adaptor = AVAssetWriterInputPixelBufferAdaptor(
            assetWriterInput: input,
            sourcePixelBufferAttributes: [
                kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA,
                kCVPixelBufferWidthKey as String: Int(size.width),
                kCVPixelBufferHeightKey as String: Int(size.height),
            ])
        guard writer.canAdd(input) else {
            throw ExportError.writerFailed("input rejected")
        }
        writer.add(input)
        guard writer.startWriting() else {
            throw ExportError.writerFailed(writer.error?.localizedDescription ?? "startWriting failed")
        }
        writer.startSession(atSourceTime: .zero)

        func fail(_ error: Error) throws -> Never {
            writer.cancelWriting()
            try? FileManager.default.removeItem(at: url)
            throw error
        }

        var time = CMTime.zero
        for (index, frame) in frames.enumerated() {
            if Task.isCancelled { try fail(CancellationError()) }

            // Arrow frames get a short draw-in: a handful of samples along
            // an ease-out curve, then the still frame holds.
            var arrowProgresses: [CGFloat] = [1]
            if case .position(let position) = frame.content, position.arrow != nil {
                arrowProgresses = (1...ExportFrameBuilder.arrowSteps).map { step in
                    let t = CGFloat(step) / CGFloat(ExportFrameBuilder.arrowSteps)
                    return 1 - pow(1 - t, 2)
                }
            }
            let stepDuration = ExportFrameBuilder.arrowGrowth
                / Double(ExportFrameBuilder.arrowSteps)

            for (step, arrowProgress) in arrowProgresses.enumerated() {
                if Task.isCancelled { try fail(CancellationError()) }
                let renderer = ImageRenderer(
                    content: ExportFrameView(frame: frame, arrowProgress: arrowProgress))
                renderer.scale = 1
                renderer.proposedSize = ProposedViewSize(size)
                guard let image = renderer.cgImage else {
                    try fail(ExportError.renderFailed)
                }
                do {
                    try await append(image, at: time, to: adaptor, input: input, size: size)
                } catch {
                    try fail(error)
                }
                let isLastStep = step == arrowProgresses.count - 1
                time = time + CMTime(
                    seconds: isLastStep ? frame.hold : stepDuration,
                    preferredTimescale: Self.timescale)
                await Task.yield()
            }
            progress(Double(index + 1) / Double(frames.count))
        }

        input.markAsFinished()
        writer.endSession(atSourceTime: time)
        await writer.finishWriting()
        guard writer.status == .completed else {
            try? FileManager.default.removeItem(at: url)
            throw ExportError.writerFailed(
                writer.error?.localizedDescription ?? "unknown")
        }
        return url
    }

    @MainActor
    private static func append(
        _ image: CGImage, at time: CMTime,
        to adaptor: AVAssetWriterInputPixelBufferAdaptor,
        input: AVAssetWriterInput, size: CGSize
    ) async throws {
        while !input.isReadyForMoreMediaData {
            try await Task.sleep(nanoseconds: 10_000_000)
        }
        var buffer: CVPixelBuffer?
        if let pool = adaptor.pixelBufferPool {
            CVPixelBufferPoolCreatePixelBuffer(nil, pool, &buffer)
        }
        if buffer == nil {
            CVPixelBufferCreate(
                nil, Int(size.width), Int(size.height),
                kCVPixelFormatType_32BGRA,
                [kCVPixelBufferCGImageCompatibilityKey: true] as CFDictionary,
                &buffer)
        }
        guard let buffer else {
            throw ExportError.writerFailed("no pixel buffer")
        }
        CVPixelBufferLockBaseAddress(buffer, [])
        defer { CVPixelBufferUnlockBaseAddress(buffer, []) }
        guard let context = CGContext(
            data: CVPixelBufferGetBaseAddress(buffer),
            width: Int(size.width), height: Int(size.height),
            bitsPerComponent: 8,
            bytesPerRow: CVPixelBufferGetBytesPerRow(buffer),
            space: CGColorSpace(name: CGColorSpace.sRGB)!,
            bitmapInfo: CGImageAlphaInfo.premultipliedFirst.rawValue
                | CGBitmapInfo.byteOrder32Little.rawValue)
        else {
            throw ExportError.writerFailed("no draw context")
        }
        context.draw(image, in: CGRect(origin: .zero, size: size))
        guard adaptor.append(buffer, withPresentationTime: time) else {
            throw ExportError.writerFailed("sample append failed")
        }
    }
}
