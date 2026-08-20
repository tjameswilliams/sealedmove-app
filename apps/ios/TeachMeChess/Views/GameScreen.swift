import SwiftUI
import UIKit

/// Main screen: the board (plus a thin status strip and a compact moves
/// pill) is pinned to the top; the coach's feedback lives in a card dock
/// pinned to the bottom. Each piece of feedback is one card: the newest
/// shows collapsed, a tap expands it over the board, and swiping pages
/// back through earlier cards. Talking to the coach happens in a chat
/// sheet the dock's "Ask" button opens, so the conversation UI only
/// appears when asked for. A summary sheet appears when the game ends.
struct GameScreen: View {
    @State private var model = GameViewModel()
    @State private var showMoveList = false
    @State private var showSettings = false
    @State private var showHistory = false
    @State private var showLexicon = false
    @State private var showNewGameDialog = false
    @State private var showReview = false
    /// The chat sheet ("Ask the coach") is up.
    @State private var showChat = false
    /// Card currently expanded over the board, nil when all are collapsed.
    @State private var expandedMessage: CoachMessage?
    /// The App Store rating ask. Named apart from `model.requestReview()`,
    /// which builds the game walkthrough and has nothing to do with this.
    @Environment(\.requestReview) private var requestAppStoreReview

    /// Vertical points reserved for everything that is not the board
    /// (status strip, trays, moves pill, card dock, spacing): the board
    /// shrinks below full width only when a screen is too short for it.
    private static let reservedHeight: CGFloat = 330

    /// Spring used for the card expand/collapse morph.
    static let expandSpring = Animation.spring(response: 0.35, dampingFraction: 0.85)

    var body: some View {
        @Bindable var model = model
        NavigationStack {
            GeometryReader { geo in
                let fullSide = geo.size.width - 20
                let side = max(220, min(fullSide, geo.size.height - Self.reservedHeight))
                ZStack(alignment: .bottom) {
                    VStack(spacing: 10) {
                        statusStrip
                        // Captured-pieces trays hug the board: above it,
                        // what the opponent (Black) has taken, in white
                        // glyphs; below it, the student's haul, in black
                        // glyphs. Each row collapses to nothing while
                        // empty. They track the board's width so they keep
                        // hugging its edges.
                        VStack(spacing: 2) {
                            CapturedTrayRow(
                                letters: model.material.capturedByBlack,
                                advantage: model.material.blackAdvantage)
                                .frame(width: side, alignment: .leading)
                                .padding(.leading, 4)
                            BoardView(model: model)
                                .frame(width: side, height: side)
                            CapturedTrayRow(
                                letters: model.material.capturedByWhite,
                                advantage: model.material.whiteAdvantage)
                                .frame(width: side, alignment: .leading)
                                .padding(.leading, 4)
                        }
                        .frame(maxWidth: .infinity)
                        movesPill
                        Spacer(minLength: 6)
                        CoachCardDock(
                            model: model,
                            onExpand: { message in
                                withAnimation(Self.expandSpring) {
                                    expandedMessage = message
                                }
                            },
                            onAsk: { showChat = true },
                            onReview: { showReview = true })
                    }
                    .padding(.top, 4)

                    // Expanded card: floats over the board, board still
                    // visible above it so tapped chess mentions can light
                    // up squares. Tap outside (or Close) to collapse.
                    if let message = expandedMessage {
                        Color.black.opacity(0.22)
                            .ignoresSafeArea()
                            .onTapGesture { collapseCard() }
                            .transition(.opacity)
                        ExpandedCoachCard(
                            message: message,
                            model: model,
                            onClose: { collapseCard() })
                            .frame(maxHeight: geo.size.height * 0.62)
                            .padding(.horizontal, 10)
                            .padding(.bottom, 8)
                            .transition(.move(edge: .bottom).combined(with: .opacity))
                    }
                }
                .animation(Self.expandSpring, value: expandedMessage?.id)
            }
            .navigationTitle("Sealed Move")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button {
                        showSettings = true
                    } label: {
                        Image(systemName: "gearshape")
                    }
                    .accessibilityLabel("Coach settings")
                }
                ToolbarItem(placement: .topBarLeading) {
                    Button {
                        showHistory = true
                    } label: {
                        Image(systemName: "clock.arrow.circlepath")
                    }
                    .accessibilityLabel("Game history")
                }
                ToolbarItem(placement: .topBarLeading) {
                    Button {
                        showLexicon = true
                    } label: {
                        Image(systemName: "book")
                    }
                    .accessibilityLabel("Chess lexicon")
                }
                ToolbarItem(placement: .primaryAction) {
                    Button("New Game") {
                        // Mid-game the button asks: abandon or resign?
                        if model.outcomeText == nil && !model.moves.isEmpty {
                            showNewGameDialog = true
                        } else {
                            model.newGame()
                        }
                    }
                    .disabled(model.isPipelineRunning)
                }
            }
            .confirmationDialog("Game in progress", isPresented: $showNewGameDialog) {
                Button("Resign, then keep the board") { model.resign() }
                Button("Abandon and start fresh", role: .destructive) { model.newGame() }
                Button("Cancel", role: .cancel) {}
            } message: {
                Text("Resigning records the game as a loss in your history. Abandoning discards it as aborted.")
            }
            .sheet(isPresented: $showMoveList) {
                MoveListSheet(model: model)
                    .presentationDetents([.medium, .large])
            }
            .sheet(isPresented: $showSettings) {
                SettingsSheet(model: model)
            }
            .sheet(isPresented: $showHistory) {
                HistoryView(model: model)
            }
            .sheet(isPresented: $showLexicon) {
                LexiconBrowser()
            }
            .sheet(item: $model.presentedLexicon) { entry in
                LexiconSheet(entry: entry)
                    .presentationDetents([.large, .medium])
            }
            // The conversation UI only exists once asked for: a medium
            // detent leaves the board visible, so mention links in the
            // coach's answers can still spotlight squares behind it.
            .sheet(isPresented: $showChat) {
                CoachChatSheet(model: model)
                    .presentationDetents([.medium, .large])
                    .presentationBackgroundInteraction(.enabled(upThrough: .medium))
            }
            // The trial lapsed (launch check or a proxy rejection) —
            // convert to the subscription or switch to the free coach.
            .sheet(isPresented: $model.showTrialExpiry) {
                TrialExpirySheet(model: model)
                    .presentationDetents([.medium, .large])
            }
            // Cloud-AI disclosure — gates the first Pro Coach switch; the
            // sheet itself records consent or reverts the provider.
            .sheet(isPresented: $model.showProCoachConsent) {
                ProCoachConsentSheet(model: model)
                    .presentationDetents([.medium, .large])
            }
            .sheet(item: $model.gameSummary, onDismiss: {
                // Not while the walkthrough is opening (it asks on its own
                // way out), and not straight after a loss.
                if !showReview && !model.lastGameWasLoss { askForRating() }
            }) { summary in
                GameSummarySheet(
                    summary: summary,
                    onNewGame: {
                        model.gameSummary = nil
                        model.newGame()
                    },
                    onReview: {
                        model.gameSummary = nil
                        showReview = true
                    })
                    .presentationDetents([.medium])
            }
            // The post-game walkthrough. Reachable from the summary sheet
            // and from the coach toolbar, so a student who only wants the
            // follow-up never has to sit through live commentary to get it.
            .sheet(isPresented: $showReview, onDismiss: { askForRating() }) {
                GameReviewSheet(model: model)
            }
            // Once per lapsed trial: offer convert-or-downgrade without
            // waiting for the student to bump into a rejected request.
            .task { await model.checkTrialExpiry() }
        }
    }

    private func collapseCard() {
        withAnimation(Self.expandSpring) { expandedMessage = nil }
    }

    /// Ask for an App Store rating if this is a good moment and we have
    /// not already asked this release. The delay lets the sheet finish
    /// dismissing: an ask that races the teardown is silently dropped.
    private func askForRating() {
        guard RatingPrompt.isEligible else { return }
        RatingPrompt.markAsked()
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.6) {
            requestAppStoreReview()
        }
    }

    /// Resign the first responder (dismiss the keyboard) without needing
    /// to know which field holds focus.
    static func hideKeyboard() {
        UIApplication.shared.sendAction(
            #selector(UIResponder.resignFirstResponder), to: nil, from: nil, for: nil)
    }

    // MARK: Status strip (turn banner / review banner)

    @ViewBuilder
    private var statusStrip: some View {
        if let ply = model.reviewPly {
            HStack(spacing: 10) {
                Image(systemName: "clock.arrow.circlepath")
                    .font(.subheadline.bold())
                Text("Reviewing \(model.plyLabel(ply))")
                    .font(.subheadline.bold())
                    .lineLimit(1)
                Button {
                    model.exitReview()
                } label: {
                    Text("Back to live")
                        .font(.subheadline.bold())
                        .padding(.horizontal, 10)
                        .padding(.vertical, 4)
                        .background(Color.orange, in: Capsule())
                        .foregroundStyle(.white)
                }
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 6)
            .background(Color.orange.opacity(0.18), in: Capsule())
            .foregroundStyle(.orange)
        } else if let outcome = model.outcomeText {
            Text(outcome)
                .font(.headline)
                .padding(.horizontal, 16)
                .padding(.vertical, 6)
                .background(Color.yellow.opacity(0.3), in: Capsule())
        } else if let opening = model.currentOpening {
            // The opening was already identified algorithmically for the
            // coach's benefit; naming it above the board tells the student
            // what they are playing while they play it.
            HStack(spacing: 5) {
                Text(opening.shortName)
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(.primary)
                    .lineLimit(1)
                    .minimumScaleFactor(0.8)
                Text("·")
                    .foregroundStyle(.tertiary)
                Text(model.whiteToMove ? "White to move" : "Black to move")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .layoutPriority(1)
            }
            .accessibilityElement(children: .combine)
        } else {
            Text(model.whiteToMove ? "White to move" : "Black to move")
                .font(.subheadline)
                .foregroundStyle(.secondary)
        }
    }

    // MARK: Moves pill (compact, expandable)

    private var latestPairText: String {
        guard !model.moves.isEmpty else { return "No moves yet" }
        let lastPairStart = (model.moves.count - 1) / 2 * 2
        let number = lastPairStart / 2 + 1
        var text = "\(number). \(model.moves[lastPairStart])"
        if lastPairStart + 1 < model.moves.count {
            text += " \(model.moves[lastPairStart + 1])"
        }
        return text
    }

    private var movesPill: some View {
        Button {
            showMoveList = true
        } label: {
            HStack(spacing: 6) {
                Text(latestPairText)
                    .font(.system(.footnote, design: .monospaced))
                    .foregroundStyle(model.moves.isEmpty ? .secondary : .primary)
                if !model.moves.isEmpty {
                    Text("· \(model.moves.count) \(model.moves.count == 1 ? "move" : "moves")")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }
                Image(systemName: "chevron.right")
                    .font(.caption2.bold())
                    .foregroundStyle(.secondary)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 6)
            .background(Color(.secondarySystemBackground), in: Capsule())
        }
        .buttonStyle(.plain)
        .disabled(model.moves.isEmpty)
        .accessibilityLabel("Move list")
    }
}

// MARK: - Card styling shared by dock, expansion, and chat rows

private extension CoachMessage {
    /// Icon + tint that identify the message's role wherever it renders.
    var symbolName: String {
        switch role {
        case .coach: return "graduationcap"
        case .note: return "eye"
        case .system: return "info.circle"
        case .student: return "person"
        }
    }

    var symbolTint: Color {
        switch role {
        case .note: return Brand.tournamentGreen
        default: return .secondary
        }
    }

    /// Card surface color: notes keep their "watch out" green wash.
    var cardBackground: Color {
        role == .note
            ? Brand.tournamentGreen.opacity(0.10)
            : Color(.secondarySystemBackground)
    }
}

/// Small "review" tag for messages produced while rewinding.
private struct ReviewTag: View {
    var body: some View {
        Text("review")
            .font(.caption2.bold())
            .padding(.horizontal, 6)
            .padding(.vertical, 1)
            .background(Brand.annotation.opacity(0.14), in: Capsule())
            .foregroundStyle(Brand.annotation)
    }
}

// MARK: - Coach card dock

/// Bottom-docked stack of feedback cards. The newest card shows collapsed;
/// swiping pages back through earlier ones; a tap hands the card to the
/// expansion overlay. Below the cards sit the coach controls (ask / pause /
/// review); the chat UI itself lives in a sheet.
private struct CoachCardDock: View {
    @Bindable var model: GameViewModel
    var onExpand: (CoachMessage) -> Void
    var onAsk: () -> Void
    var onReview: () -> Void

    /// Which card the pager shows (message id).
    @State private var selection: UUID?

    /// How many recent cards stay swipeable. Older feedback is still in
    /// the chat sheet's transcript.
    private static let dockLimit = 50
    private static let cardHeight: CGFloat = 96

    /// Feedback cards: everything the coach volunteered. What the student
    /// typed belongs to the chat sheet, not the feedback stack.
    private var cards: [CoachMessage] {
        Array(model.coachFeed.filter { $0.role != .student }.suffix(Self.dockLimit))
    }

    var body: some View {
        VStack(spacing: 6) {
            if cards.isEmpty {
                emptyCard
            } else {
                TabView(selection: $selection) {
                    ForEach(cards) { message in
                        CollapsedCoachCard(message: message)
                            .padding(.horizontal, 2)
                            .onTapGesture { onExpand(message) }
                            .tag(Optional(message.id))
                    }
                }
                .tabViewStyle(.page(indexDisplayMode: .never))
                .frame(height: Self.cardHeight)
            }

            HStack(spacing: 8) {
                if model.isCoachThinking {
                    ProgressView().controlSize(.mini)
                    Text("Coach is thinking…")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                } else if let index = currentIndex, cards.count > 1 {
                    Text("\(index + 1) of \(cards.count)")
                        .font(.caption2.monospacedDigit())
                        .foregroundStyle(.secondary)
                    if index < cards.count - 1 {
                        Button("Latest") { snapToNewest() }
                            .font(.caption2.bold())
                            .buttonStyle(.plain)
                            .foregroundStyle(Brand.tournamentGreen)
                    }
                }
                Spacer()
                if model.isCoachPaused {
                    Text("paused")
                        .font(.caption2.bold())
                        .padding(.horizontal, 6)
                        .padding(.vertical, 1)
                        .background(Color.orange.opacity(0.18), in: Capsule())
                        .foregroundStyle(.orange)
                }
            }
            .frame(minHeight: 14)

            toolbar
        }
        .padding(10)
        .background(Color(.tertiarySystemFill).opacity(0.4), in: RoundedRectangle(cornerRadius: 14))
        .padding(.horizontal, 10)
        .padding(.bottom, 6)
        .onAppear { selection = cards.last?.id }
        .onChange(of: model.coachFeed) {
            // A new card always fronts the stack; earlier ones stay a
            // swipe away.
            snapToNewest()
        }
    }

    private var currentIndex: Int? {
        cards.firstIndex(where: { $0.id == selection })
    }

    private func snapToNewest() {
        withAnimation { selection = cards.last?.id }
    }

    private var emptyCard: some View {
        HStack(spacing: 8) {
            Image(systemName: "graduationcap")
                .font(.caption)
                .foregroundStyle(.secondary)
            Text("The coach's feedback stacks up here. Swipe back through it any time.")
                .font(.footnote)
                .foregroundStyle(.secondary)
            Spacer(minLength: 0)
        }
        .padding(12)
        .frame(maxWidth: .infinity)
        .frame(height: Self.cardHeight)
        .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 12))
        .padding(.horizontal, 2)
    }

    // MARK: Controls row

    private var toolbar: some View {
        HStack(spacing: 8) {
            Button(action: onAsk) {
                HStack(spacing: 6) {
                    Image(systemName: "bubble.left.and.text.bubble.right")
                    Text(model.isReviewing ? "Ask about this position" : "Ask the coach")
                        .lineLimit(1)
                }
                .font(.subheadline)
                .padding(.horizontal, 12)
                .padding(.vertical, 7)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(Color(.tertiarySystemBackground), in: Capsule())
            }
            .buttonStyle(.plain)
            .disabled(!model.sessionReady)
            .accessibilityLabel("Ask the coach a question")

            toolbarButton(
                systemImage: model.isCoachPaused ? "play.fill" : "pause.fill",
                tint: model.isCoachPaused ? Brand.tournamentGreen : .secondary,
                label: model.isCoachPaused ? "Resume the coach" : "Pause the coach",
                hint: model.isCoachPaused
                    ? "The coach comments again, starting with a catch-up on what it missed."
                    : "The coach stops commenting. It keeps checking your moves with the engine."
            ) {
                model.togglePause()
            }
            .disabled(!model.sessionReady || model.isRecapping)

            toolbarButton(
                systemImage: "chart.line.uptrend.xyaxis",
                tint: .secondary,
                label: "Review this game",
                hint: "Analyzes the whole game and walks you through the moments that decided it."
            ) {
                onReview()
            }
            .disabled(!model.sessionReady || model.moves.isEmpty || model.isBuildingReview)
        }
    }

    @ViewBuilder
    private func toolbarButton(
        systemImage: String, tint: Color, label: String, hint: String,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            Image(systemName: systemImage)
                .font(.subheadline)
                .frame(width: 34, height: 34)
                .background(Color(.tertiarySystemBackground), in: Circle())
                .foregroundStyle(tint)
        }
        .buttonStyle(.plain)
        .accessibilityLabel(label)
        .accessibilityHint(hint)
    }
}

/// One collapsed card: role icon, a few lines of the message, and the
/// expand affordance. Markdown renders inline but links stay inert here:
/// the whole card is one tap target for expansion.
private struct CollapsedCoachCard: View {
    let message: CoachMessage

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: message.symbolName)
                .font(.caption)
                .foregroundStyle(message.symbolTint)
                .padding(.top, 3)
            VStack(alignment: .leading, spacing: 3) {
                if message.isReview {
                    ReviewTag()
                }
                Text(MarkdownParser.inline(message.text))
                    .font(message.role == .system ? .footnote : .callout)
                    .fontDesign(message.role == .system ? .default : .serif)
                    .foregroundStyle(message.role == .system ? .secondary : .primary)
                    .lineLimit(message.isReview ? 2 : 3)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            Image(systemName: "arrow.up.left.and.arrow.down.right")
                .font(.caption2)
                .foregroundStyle(.tertiary)
                .padding(.top, 3)
        }
        .padding(10)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(message.cardBackground, in: RoundedRectangle(cornerRadius: 12))
        .contentShape(Rectangle())
        .accessibilityHint("Tap to read the whole message.")
    }
}

/// The expanded card: full message, scrollable, with live chess-mention
/// links; the board stays visible above so a tapped mention can light up
/// its squares.
private struct ExpandedCoachCard: View {
    let message: CoachMessage
    let model: GameViewModel
    var onClose: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 6) {
                Image(systemName: message.symbolName)
                    .font(.caption)
                    .foregroundStyle(message.symbolTint)
                Text(headerTitle)
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(.secondary)
                if message.isReview {
                    ReviewTag()
                }
                Spacer()
                Button(action: onClose) {
                    Image(systemName: "xmark.circle.fill")
                        .font(.title3)
                        .foregroundStyle(.secondary)
                }
                .accessibilityLabel("Close the expanded card")
            }
            // A short message hugs its content; a long one caps at the
            // card's max height and scrolls inside.
            ViewThatFits(in: .vertical) {
                messageBody
                ScrollView { messageBody }
                    .frame(maxWidth: .infinity)
            }
            Text("Tap a highlighted move or square to see it on the board.")
                .font(.caption2)
                .foregroundStyle(.tertiary)
        }
        .padding(14)
        .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 14))
        .shadow(color: .black.opacity(0.18), radius: 12, y: 4)
    }

    private var messageBody: some View {
        MarkdownText(text: message.text, chessLinks: message.role != .system)
            .font(.body)
            .fontDesign(message.role == .system ? .default : .serif)
            .foregroundStyle(.primary)
            .frame(maxWidth: .infinity, alignment: .leading)
            .environment(\.openURL, coachLinkAction)
    }

    private var headerTitle: String {
        switch message.role {
        case .note: return "Watch out"
        case .system: return "Session"
        default: return "Coach"
        }
    }

    /// Intercepts `coachref://` mention links (board spotlight / lexicon);
    /// any real web link the model wrote still opens normally.
    private var coachLinkAction: OpenURLAction {
        OpenURLAction { url in
            guard url.scheme == ChessMention.urlScheme else { return .systemAction }
            model.handleCoachLink(url, in: message)
            return .handled
        }
    }
}

// MARK: - Feed scroll anchor

private extension View {
    /// Bottom-anchor the chat transcript like a chat log. On iOS 18+ the
    /// anchor is limited to the initial offset + alignment roles: explicit
    /// `scrollTo` calls keep the newest message in view on feed changes,
    /// so the size-change role is redundant (and historically it fed back
    /// into keyboard/layout updates until the main thread livelocked).
    @ViewBuilder
    func feedBottomAnchor() -> some View {
        if #available(iOS 18.0, *) {
            self
                .defaultScrollAnchor(.bottom, for: .initialOffset)
                .defaultScrollAnchor(.bottom, for: .alignment)
        } else {
            self.defaultScrollAnchor(.bottom)
        }
    }
}

// MARK: - Chat sheet

/// The conversation with the coach, summoned by the dock's Ask button. The
/// full transcript (feedback cards included) reads like a chat log here,
/// with the input field pinned at the bottom. At the medium detent the
/// board stays visible behind, so mention links in answers still spotlight
/// squares.
struct CoachChatSheet: View {
    @Bindable var model: GameViewModel
    @Environment(\.dismiss) private var dismiss
    @FocusState private var chatFocused: Bool

    var body: some View {
        NavigationStack {
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 10) {
                        ForEach(model.coachFeed) { message in
                            CoachMessageRow(message: message, model: model)
                                .id(message.id)
                        }
                        if model.isCoachThinking {
                            HStack(spacing: 6) {
                                ProgressView().controlSize(.small)
                                Text("Coach is thinking…")
                                    .font(.callout)
                                    .foregroundStyle(.secondary)
                            }
                            .id("thinking")
                        }
                    }
                    .padding(.horizontal, 16)
                    .padding(.vertical, 8)
                    .frame(maxWidth: .infinity)
                }
                .scrollDismissesKeyboard(.interactively)
                .feedBottomAnchor()
                .onChange(of: model.coachFeed) {
                    withAnimation {
                        if model.isCoachThinking {
                            proxy.scrollTo("thinking", anchor: .bottom)
                        } else if let last = model.coachFeed.last {
                            proxy.scrollTo(last.id, anchor: .bottom)
                        }
                    }
                }
                .onChange(of: model.isCoachThinking) {
                    withAnimation {
                        if model.isCoachThinking {
                            proxy.scrollTo("thinking", anchor: .bottom)
                        }
                    }
                }
            }
            .safeAreaInset(edge: .bottom) { chatField }
            .navigationTitle(model.isReviewing ? "About this position" : "Ask the coach")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") { dismiss() }
                }
            }
        }
        .onAppear {
            // The sheet exists to type into, so hand focus over right away.
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.4) {
                chatFocused = true
            }
        }
    }

    private var chatField: some View {
        HStack(spacing: 8) {
            TextField(model.isReviewing ? "Ask about this position…" : "Ask the coach…",
                      text: $model.chatDraft)
                .textFieldStyle(.roundedBorder)
                .focused($chatFocused)
                .submitLabel(.send)
                .onSubmit { model.sendChat() }
            Button {
                model.sendChat()
            } label: {
                Image(systemName: "arrow.up.circle.fill")
                    .font(.title2)
            }
            .disabled(!model.sessionReady || model.isCoachThinking
                      || model.chatDraft.trimmingCharacters(in: .whitespaces).isEmpty)
        }
        .padding(10)
        .background(.bar)
    }
}

/// One transcript row in the chat sheet: the same role styling the cards
/// use, in chat-log form.
private struct CoachMessageRow: View {
    let message: CoachMessage
    let model: GameViewModel

    var body: some View {
        if message.role == .system {
            // Subtle centered system line ("Resumed game in progress…").
            MarkdownText(text: message.text)
                .font(.caption)
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, alignment: .center)
                .padding(.vertical, 2)
        } else if message.role == .note {
            // Opponent-move observation from the commentary engine ("watch
            // out" / game-story note) — eye icon and a tinted card set it
            // apart from the coach's move reactions.
            HStack(alignment: .top, spacing: 6) {
                Image(systemName: "eye")
                    .font(.caption)
                    .foregroundStyle(Brand.tournamentGreen)
                    .padding(.top, 3)
                MarkdownText(text: message.text, chessLinks: true)
                    .font(.callout)
                    .fontDesign(.serif)
                    .foregroundStyle(.primary)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .padding(8)
            .background(Brand.tournamentGreen.opacity(0.08), in: RoundedRectangle(cornerRadius: 8))
            .environment(\.openURL, coachLinkAction)
        } else {
            HStack(alignment: .top, spacing: 6) {
                Image(systemName: message.role == .coach ? "graduationcap" : "person")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .padding(.top, 3)
                VStack(alignment: .leading, spacing: 2) {
                    if message.isReview {
                        ReviewTag()
                    }
                    // Coach messages render light markdown (with tappable
                    // chess mentions); what the student typed stays
                    // verbatim plain text.
                    if message.role == .coach {
                        // The coach speaks in serif; the engine's numbers stay
                        // mono elsewhere — the narrator/source-of-truth split
                        // made visible.
                        MarkdownText(text: message.text, chessLinks: true)
                            .font(.body)
                            .fontDesign(.serif)
                            .foregroundStyle(.primary)
                            .environment(\.openURL, coachLinkAction)
                    } else {
                        Text(message.text)
                            .font(.callout)
                            .foregroundStyle(.secondary)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    /// Intercepts `coachref://` mention links (board spotlight / lexicon);
    /// any real web link the model wrote still opens normally.
    private var coachLinkAction: OpenURLAction {
        OpenURLAction { url in
            guard url.scheme == ChessMention.urlScheme else { return .systemAction }
            model.handleCoachLink(url, in: message)
            return .handled
        }
    }
}

// MARK: - Move list sheet (review / rewind)

/// Numbered move pairs; tapping any ply rewinds the board to the position
/// after that ply (review mode — the live session is untouched).
private struct MoveListSheet: View {
    let model: GameViewModel
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            List {
                Section {
                    ForEach(pairIndices, id: \.self) { pairStart in
                        HStack(spacing: 12) {
                            Text("\(pairStart / 2 + 1).")
                                .font(.system(.subheadline, design: .monospaced))
                                .foregroundStyle(.secondary)
                                .frame(width: 34, alignment: .trailing)
                            plyButton(ply: pairStart + 1)
                            if pairStart + 1 < model.moves.count {
                                plyButton(ply: pairStart + 2)
                            } else {
                                Spacer().frame(maxWidth: .infinity)
                            }
                        }
                    }
                } footer: {
                    Text("Tap a move to review that position on the board. The game stays live — use \"Back to live\" to return.")
                }
            }
            .navigationTitle("Moves")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") { dismiss() }
                }
                if model.isReviewing {
                    ToolbarItem(placement: .primaryAction) {
                        Button("Back to live") {
                            model.exitReview()
                            dismiss()
                        }
                    }
                }
            }
        }
    }

    private var pairIndices: [Int] {
        stride(from: 0, to: model.moves.count, by: 2).map { $0 }
    }

    @ViewBuilder
    private func plyButton(ply: Int) -> some View {
        let isCurrent = model.reviewPly == ply
        Button {
            model.enterReview(ply: ply)
            dismiss()
        } label: {
            Text(model.moves[ply - 1])
                .font(.system(.subheadline, design: .monospaced))
                .padding(.horizontal, 10)
                .padding(.vertical, 5)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(isCurrent ? Color.orange.opacity(0.2) : Color.clear,
                            in: RoundedRectangle(cornerRadius: 6))
                .foregroundStyle(isCurrent ? Color.orange : Color.primary)
        }
        .buttonStyle(.plain)
    }
}

// MARK: - Game summary sheet

/// End-of-game summary: the engine's report card for the student.
struct GameSummarySheet: View {
    let summary: GameSummaryInfo
    let onNewGame: () -> Void
    let onReview: () -> Void
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(spacing: 20) {
            Text(summary.outcomeText.isEmpty ? "Game over" : summary.outcomeText)
                .font(.title3.bold())
                .padding(.top, 24)

            if let badge = summary.levelEventBadge {
                // Placement resolved or auto-advance fired this game.
                Label(badge, systemImage: "arrow.up.forward.circle.fill")
                    .font(.subheadline.bold())
                    .padding(.horizontal, 14)
                    .padding(.vertical, 8)
                    .background(Color.green.opacity(0.18), in: Capsule())
                    .foregroundStyle(.green)
            } else if summary.readyToAdvance {
                Label("Ready to advance — level up in Settings!",
                      systemImage: "arrow.up.forward.circle.fill")
                    .font(.subheadline.bold())
                    .padding(.horizontal, 14)
                    .padding(.vertical, 8)
                    .background(Color.green.opacity(0.18), in: Capsule())
                    .foregroundStyle(.green)
            }

            Grid(horizontalSpacing: 24, verticalSpacing: 12) {
                GridRow {
                    statCell("Moves judged", "\(summary.movesJudged)")
                    statCell("Accuracy", String(format: "%.1f%%", summary.accuracy))
                }
                GridRow {
                    statCell("Avg. CP loss", String(format: "%.0f", summary.acl))
                    statCell("Est. rating", "\(summary.estRating)")
                }
            }
            .padding(.horizontal)

            // The walkthrough leads: it is the part of the game that
            // teaches, and starting a new game discards the chance to see
            // it fresh.
            Button {
                dismiss()
                onReview()
            } label: {
                Label("Review this game", systemImage: "chart.line.uptrend.xyaxis")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .padding(.horizontal, 24)

            Button {
                dismiss()
                onNewGame()
            } label: {
                Text("New game")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.bordered)
            .padding(.horizontal, 24)

            Button("Keep looking at the board") { dismiss() }
                .font(.callout)
                .padding(.bottom, 16)

            Spacer(minLength: 0)
        }
    }

    @ViewBuilder
    private func statCell(_ title: String, _ value: String) -> some View {
        VStack(spacing: 2) {
            Text(value)
                .font(.title2.monospacedDigit().bold())
            Text(title)
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .frame(minWidth: 110)
    }
}

#Preview {
    GameScreen()
}
