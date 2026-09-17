import SwiftUI
import PhotosUI

/// The scan flow: photograph (or pick) a board, let the recognizer read it,
/// fix anything it got wrong on the editable board, then either play the
/// position out against the engine or have the engine solve it, and export
/// the solution as a video.
struct ScanBoardSheet: View {
    let model: GameViewModel
    @Environment(\.dismiss) private var dismiss

    // MARK: Photo + recognition

    @State private var photoItem: PhotosPickerItem?
    @State private var photo: UIImage?
    @State private var isRecognizing = false
    @State private var recognitionNotes: String?
    @State private var recognitionError: String?
    @State private var showCamera = false

    // MARK: Position under edit

    /// Starts on the standard position: familiar, and most photos replace
    /// it wholesale anyway.
    @State private var pieces = GameViewModel.parseFenBoard(
        "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR")
    @State private var whiteToMove = true

    // MARK: Solving / actions

    @State private var isSolving = false
    @State private var solution: SolutionLine?
    @State private var solveError: String?
    @State private var showExport = false
    @State private var showAbandonDialog = false

    // MARK: Coach Q&A

    /// Questions asked about the scanned position, oldest first.
    @State private var coachExchanges: [(question: String, answer: String)] = []
    @State private var coachQuestion = ""
    @State private var isAskingCoach = false
    @State private var coachError: String?

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    photoSection
                    positionSection
                    actionSection
                    if let solution { solutionSection(solution) }
                    coachSection
                }
                .padding(.horizontal, 16)
                .padding(.bottom, 24)
            }
            .navigationTitle("Scan a board")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") { dismiss() }
                }
            }
            .fullScreenCover(isPresented: $showCamera) {
                CameraPicker { image in
                    showCamera = false
                    if let image { recognize(image) }
                }
                .ignoresSafeArea()
            }
            .sheet(isPresented: $showExport) {
                if let solution {
                    ExportVideoSheet(game: ExportableGame(
                        solutionMoves: solution.sanMoves,
                        fens: solution.fens,
                        moveLabels: solution.moveLabels,
                        headline: solution.headline,
                        subtitle: solution.subtitle))
                }
            }
            .confirmationDialog("Game in progress", isPresented: $showAbandonDialog) {
                Button("Abandon it and play this position", role: .destructive) {
                    startGame()
                }
                Button("Cancel", role: .cancel) {}
            } message: {
                Text("Starting from the scanned position discards the game on the board as aborted.")
            }
            .onChange(of: photoItem) { loadPickedPhoto() }
            .onChange(of: pieces) { positionChanged() }
            .onChange(of: whiteToMove) { positionChanged() }
        }
    }

    // MARK: - Photo section

    private var photoSection: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Photo")
                .font(.caption.bold())
                .foregroundStyle(.secondary)
            HStack(spacing: 10) {
                if CameraPicker.isAvailable {
                    Button {
                        showCamera = true
                    } label: {
                        Label("Take photo", systemImage: "camera")
                            .frame(maxWidth: .infinity)
                    }
                    .buttonStyle(.bordered)
                }
                PhotosPicker(selection: $photoItem, matching: .images) {
                    Label("Choose photo", systemImage: "photo.on.rectangle")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.bordered)
            }
            .disabled(isRecognizing)

            if let photo {
                HStack(alignment: .top, spacing: 12) {
                    Image(uiImage: photo)
                        .resizable()
                        .scaledToFill()
                        .frame(width: 84, height: 84)
                        .clipShape(RoundedRectangle(cornerRadius: 8))
                    if isRecognizing {
                        HStack(spacing: 8) {
                            ProgressView()
                            Text("Reading the board…")
                                .font(.footnote)
                                .foregroundStyle(.secondary)
                        }
                        .padding(.top, 8)
                    } else if let recognitionNotes {
                        Label(recognitionNotes, systemImage: "eye")
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                            .padding(.top, 4)
                    }
                    Spacer(minLength: 0)
                }
            }
            if let recognitionError {
                Text(recognitionError)
                    .font(.caption)
                    .foregroundStyle(.red)
            }
            if photo == nil {
                Text("Snap a puzzle from a book, a newspaper, or a screenshot. "
                     + "Recognition runs on this device; the photo never leaves it. "
                     + "You can also skip the photo and set the position up by hand below.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
    }

    // MARK: - Position section

    private var positionSection: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text("Position")
                    .font(.caption.bold())
                    .foregroundStyle(.secondary)
                Spacer()
                Button("Clear") {
                    pieces = [:]
                }
                .font(.footnote)
                Button("Reset") {
                    pieces = GameViewModel.parseFenBoard(
                        "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR")
                    whiteToMove = true
                }
                .font(.footnote)
            }
            BoardEditorView(pieces: $pieces)
            Picker("Side to move", selection: $whiteToMove) {
                Text("White to move").tag(true)
                Text("Black to move").tag(false)
            }
            .pickerStyle(.segmented)
            if let problem = validationProblem {
                Text(problem)
                    .font(.caption)
                    .foregroundStyle(.red)
            }
        }
    }

    // MARK: - Actions

    private var actionSection: some View {
        VStack(spacing: 10) {
            Button {
                if model.outcomeText == nil && !model.moves.isEmpty {
                    showAbandonDialog = true
                } else {
                    startGame()
                }
            } label: {
                Label("Play from this position", systemImage: "play")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .tint(Brand.tournamentGreen)

            if isSolving {
                HStack(spacing: 8) {
                    ProgressView()
                    Text("The engine is working out the line…")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }
            } else {
                Button {
                    solve()
                } label: {
                    Label("Solve with the engine", systemImage: "brain")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.bordered)
            }
            if let solveError {
                Text(solveError)
                    .font(.caption)
                    .foregroundStyle(.red)
            }
            Text("You'll play \(whiteToMove ? "White" : "Black"): whoever is to move.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .disabled(validationProblem != nil || isRecognizing)
    }

    // MARK: - Solution

    @ViewBuilder
    private func solutionSection(_ solution: SolutionLine) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Engine's solution")
                .font(.caption.bold())
                .foregroundStyle(.secondary)
            VStack(alignment: .leading, spacing: 8) {
                Text(solution.headline)
                    .font(.headline)
                Text(solution.moveLabels.joined(separator: "  "))
                    .font(.system(.callout, design: .monospaced))
                    .foregroundStyle(.secondary)
                Text("Searched to depth \(solution.depth) with the embedded Stockfish 11.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(
                RoundedRectangle(cornerRadius: 12)
                    .fill(Brand.tournamentGreen.opacity(0.08)))
            .overlay(
                RoundedRectangle(cornerRadius: 12)
                    .strokeBorder(Brand.tournamentGreen.opacity(0.35), lineWidth: 1))
            Button {
                showExport = true
            } label: {
                Label("Export solution video", systemImage: "film")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .tint(Brand.tournamentGreen)
        }
    }

    // MARK: - Coach Q&A

    /// "Why does the queen sacrifice work?" The coach answers grounded in
    /// a fresh MultiPV engine analysis of the position as set up.
    private var coachSection: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Ask the coach")
                .font(.caption.bold())
                .foregroundStyle(.secondary)
            ForEach(coachExchanges.indices, id: \.self) { i in
                VStack(alignment: .leading, spacing: 6) {
                    Text(coachExchanges[i].question)
                        .font(.callout.weight(.medium))
                    Text(coachExchanges[i].answer)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                }
                .padding(10)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(
                    RoundedRectangle(cornerRadius: 10)
                        .fill(Color(.secondarySystemBackground)))
            }
            if isAskingCoach {
                HStack(spacing: 8) {
                    ProgressView()
                    Text("Checking with the engine…")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }
            }
            HStack(spacing: 8) {
                TextField("Why does this move work?", text: $coachQuestion)
                    .textFieldStyle(.roundedBorder)
                    .onSubmit(askCoach)
                Button {
                    askCoach()
                } label: {
                    Image(systemName: "arrow.up.circle.fill")
                        .font(.title2)
                }
                .disabled(coachQuestion.trimmingCharacters(in: .whitespaces).isEmpty
                          || isAskingCoach)
            }
            if let coachError {
                Text(coachError)
                    .font(.caption)
                    .foregroundStyle(.red)
            }
        }
        .disabled(validationProblem != nil)
    }

    private func askCoach() {
        let question = coachQuestion.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !question.isEmpty, !isAskingCoach, validationProblem == nil else { return }
        coachQuestion = ""
        coachError = nil
        isAskingCoach = true
        model.askAboutPosition(fen: composedFen, question: question) { result in
            isAskingCoach = false
            switch result {
            case .success(let answer):
                coachExchanges.append((question, answer))
            case .failure(let error):
                coachError = error.localizedDescription
            }
        }
    }

    // MARK: - Plumbing

    /// The on-device model is the recognizer; nothing leaves the phone.
    private var recognizer: BoardRecognizer { LocalBoardRecognizer() }

    private var composedFen: String {
        FenComposer.fen(pieces: pieces, whiteToMove: whiteToMove)
    }

    /// A named problem first (kings, pawns on back ranks), then whatever
    /// the move generator rejects (e.g. the side not to move in check).
    private var validationProblem: String? {
        if let named = FenComposer.validationProblem(pieces: pieces) { return named }
        if (try? BoardHandle.fromFen(fen: composedFen)) == nil {
            return "That position isn't legal as set up."
        }
        return nil
    }

    private func positionChanged() {
        solution = nil
        solveError = nil
        // Answers are about the position as it was; edits make them stale.
        coachExchanges = []
        coachError = nil
    }

    private func loadPickedPhoto() {
        guard let photoItem else { return }
        Task { @MainActor in
            defer { self.photoItem = nil }
            guard let data = try? await photoItem.loadTransferable(type: Data.self),
                  let image = UIImage(data: data)
            else {
                recognitionError = "That photo couldn't be loaded."
                return
            }
            recognize(image)
        }
    }

    private func recognize(_ image: UIImage) {
        photo = image
        recognitionError = nil
        recognitionNotes = nil
        isRecognizing = true
        Task { @MainActor in
            defer { isRecognizing = false }
            do {
                let recognized = try await recognizer.recognize(image)
                let parsed = GameViewModel.parseFenBoard(recognized.boardField)
                guard !parsed.isEmpty else {
                    recognitionError = "The photo didn't read as a chess position. "
                        + "Fix it up by hand below."
                    return
                }
                pieces = parsed
                if let side = recognized.whiteToMove { whiteToMove = side }
                recognitionNotes = recognized.notes
                    ?? "Check the board against the photo, then pick an action."
            } catch {
                recognitionError = error.localizedDescription
            }
        }
    }

    private func startGame() {
        model.newGame(fromFen: composedFen)
        dismiss()
    }

    private func solve() {
        solveError = nil
        isSolving = true
        model.solvePosition(fen: composedFen) { result in
            isSolving = false
            switch result {
            case .success(let line): solution = line
            case .failure(let error): solveError = error.localizedDescription
            }
        }
    }
}
