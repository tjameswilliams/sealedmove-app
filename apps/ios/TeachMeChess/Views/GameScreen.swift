import SwiftUI
import UIKit

/// Main screen, docked layout: the board (plus a thin status strip) is
/// pinned to the top and never scrolls; a compact moves pill sits between
/// board and coach panel; the coach panel fills the remaining height with
/// its own internal scroller and a chat field pinned at its bottom (above
/// the keyboard). A summary sheet appears when the game ends.
struct GameScreen: View {
    @State private var model = GameViewModel()
    @State private var showMoveList = false
    @State private var showSettings = false
    @State private var showHistory = false
    @State private var showLexicon = false
    @State private var showNewGameDialog = false
    /// User-chosen board/coach split, dragged via the panel's grab bar.
    /// 0 = full-width board (minimum panel), 1 = max reading space (the
    /// board collapses to the compact context strip). Persisted in
    /// UserDefaults so the split survives relaunch; game state untouched.
    @State private var panelFraction: CGFloat =
        CGFloat(UserDefaults.standard.double(forKey: GameScreen.panelFractionKey))
    /// Fraction captured when the current grab-bar drag began (nil = no
    /// drag in flight) — the drag is applied as a delta on top of this.
    @State private var dragBaseFraction: CGFloat?
    @FocusState private var chatFocused: Bool

    static let panelFractionKey = "coach.panelFraction"
    /// Board edge (points) below which the shrinking board morphs into the
    /// compact context strip instead of an unreadably tiny board.
    static let compactBoardThreshold: CGFloat = 200

    /// Spring used for every layout-morph transition.
    static let expandSpring = Animation.spring(response: 0.35, dampingFraction: 0.85)

    var body: some View {
        @Bindable var model = model
        NavigationStack {
            // The board's height derives from the SCREEN WIDTH (which the
            // keyboard never changes), so the docked board never shrinks
            // when the keyboard appears — only the flexible coach panel
            // absorbs the height change. A full-width board plus keyboard
            // plus chrome physically exceeds an iPhone screen, so while the
            // chat field is FOCUSED the secondary chrome (nav bar, status
            // strip, moves pill, panel header) collapses; the board keeps
            // its exact size and the chat input stays above the keyboard.
            GeometryReader { geo in
                // The grab-bar drag scales the board edge between full
                // width and zero; below the threshold the board morphs
                // into the compact strip (the swap is animated so crossing
                // mid-drag feels smooth, not jumpy).
                let fullSide = geo.size.width - 20
                let side = max(0, fullSide * (1 - panelFraction))
                let compact = side < Self.compactBoardThreshold
                VStack(spacing: 10) {
                    if compact {
                        // Max-read mode: compact context strip (mini board
                        // + turn/last-move info). Drag the grab bar down
                        // to bring the full board back.
                        compactBoardStrip
                    } else {
                        VStack(spacing: 10) {
                            if !chatFocused { statusStrip }
                            // Captured-pieces trays hug the board: above it,
                            // what the opponent (Black) has taken — white
                            // glyphs; below it, the student's haul — black
                            // glyphs. Each row collapses to nothing while
                            // empty, and while typing they yield their
                            // points to the keyboard like the other chrome
                            // (the board itself never resizes). At a custom
                            // split the trays track the shrunken, centered
                            // board's width so they keep hugging its edges.
                            VStack(spacing: 2) {
                                if !chatFocused {
                                    CapturedTrayRow(
                                        letters: model.material.capturedByBlack,
                                        advantage: model.material.blackAdvantage)
                                        .frame(width: side, alignment: .leading)
                                        .padding(.leading, 4)
                                }
                                BoardView(model: model)
                                    .frame(width: side, height: side)
                                if !chatFocused {
                                    CapturedTrayRow(
                                        letters: model.material.capturedByWhite,
                                        advantage: model.material.whiteAdvantage)
                                        .frame(width: side, alignment: .leading)
                                        .padding(.leading, 4)
                                }
                            }
                            .frame(maxWidth: .infinity)
                            if !chatFocused { movesPill }
                        }
                        // Tapping the board/strip/pill area dismisses the
                        // keyboard. A SIMULTANEOUS gesture, so the squares'
                        // own tap gestures still fire — no dead first tap.
                        .simultaneousGesture(TapGesture().onEnded { Self.hideKeyboard() })
                    }
                    CoachPanel(
                        model: model, chatFocused: $chatFocused,
                        isCompact: compact,
                        onDragChanged: { translationY in
                            let base = dragBaseFraction ?? panelFraction
                            dragBaseFraction = base
                            // Dragging up (negative translation) grows the
                            // panel; the board absorbs the difference.
                            panelFraction = min(1, max(0, base - translationY / fullSide))
                        },
                        onDragEnded: {
                            dragBaseFraction = nil
                            // Settle near-extremes exactly: a drag that ends
                            // within 5% of full board (or max read) snaps
                            // there, so "back to full width" is reachable
                            // without pixel-perfect dragging.
                            if panelFraction < 0.05 {
                                withAnimation(Self.expandSpring) { panelFraction = 0 }
                            } else if panelFraction > 0.95 {
                                withAnimation(Self.expandSpring) { panelFraction = 1 }
                            }
                            Self.persistFraction(panelFraction)
                        },
                        onSnapToggle: {
                            // Double-tap on the grab bar snaps between the
                            // two presets: full board and max read.
                            withAnimation(Self.expandSpring) {
                                panelFraction = panelFraction > 0.5 ? 0 : 1
                            }
                            Self.persistFraction(panelFraction)
                        })
                }
                .padding(.top, 4)
                .animation(.easeInOut(duration: 0.2), value: chatFocused)
                .animation(Self.expandSpring, value: compact)
            }
            .toolbar(chatFocused ? .hidden : .visible, for: .navigationBar)
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
            .sheet(item: $model.gameSummary) { summary in
                GameSummarySheet(summary: summary) {
                    model.gameSummary = nil
                    model.newGame()
                }
                .presentationDetents([.medium])
            }
            // Once per lapsed trial: offer convert-or-downgrade without
            // waiting for the student to bump into a rejected request.
            .task { await model.checkTrialExpiry() }
        }
    }

    /// Resign the first responder (dismiss the keyboard) without needing
    /// to know which field holds focus.
    static func hideKeyboard() {
        UIApplication.shared.sendAction(
            #selector(UIResponder.resignFirstResponder), to: nil, from: nil, for: nil)
    }

    /// Persist the user's split so it survives relaunch.
    static func persistFraction(_ fraction: CGFloat) {
        UserDefaults.standard.set(Double(fraction), forKey: panelFractionKey)
    }

    // MARK: Compact board strip (shown in max-read mode)

    /// Mini board thumbnail beside whose-turn + last-move info, shown once
    /// the dragged board drops below the readability threshold. The
    /// thumbnail is non-interactive (no square taps while reading); drag
    /// the grab bar down to restore the full board. Updates live if an
    /// opponent move lands while reading.
    private var compactBoardStrip: some View {
        HStack(spacing: 12) {
            BoardView(model: model, showsThinkingOverlay: false)
                .frame(width: 132, height: 132)
                .allowsHitTesting(false)
            VStack(alignment: .leading, spacing: 4) {
                Text(compactStatusText)
                    .font(.subheadline.bold())
                    .lineLimit(2)
                Text(latestPairText)
                    .font(.system(.footnote, design: .monospaced))
                    .foregroundStyle(.secondary)
                // Space is tight in reading mode: no full trays, just a
                // "+N" material badge (with the biggest captured piece as
                // a tiny leading glyph) when material is uneven.
                if model.material.materialDiff != 0 {
                    HStack(spacing: 3) {
                        if let letter = compactBadgeLetter {
                            CapturedPieceGlyph(letter: letter, size: 12)
                        }
                        Text("+\(abs(Int(model.material.materialDiff)))")
                            .font(.caption2.bold().monospacedDigit())
                    }
                    .foregroundStyle(.secondary)
                }
                if model.isBotThinking {
                    HStack(spacing: 5) {
                        ProgressView().controlSize(.mini)
                        Text("Opponent is thinking…")
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                    }
                }
            }
            Spacer()
        }
        .padding(10)
        .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 12))
        .padding(.horizontal, 10)
        .contentShape(Rectangle())
        // Keyboard rule: tapping the strip only ever dismisses the
        // keyboard — resizing is the grab bar's job.
        .onTapGesture { Self.hideKeyboard() }
        .accessibilityLabel("Board summary. Drag the coach panel's grab bar down to show the full board.")
    }

    /// Most valuable piece the leading side has captured — the compact
    /// badge's tiny glyph. Nil in the promotion edge case where a side
    /// leads on material without a capture to show for it.
    private var compactBadgeLetter: String? {
        let m = model.material
        return m.materialDiff > 0 ? m.capturedByWhite.first : m.capturedByBlack.first
    }

    private var compactStatusText: String {
        if let ply = model.reviewPly {
            return "Reviewing \(model.plyLabel(ply))"
        }
        if let outcome = model.outcomeText {
            return outcome
        }
        return model.whiteToMove ? "White to move" : "Black to move"
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

// MARK: - Feed scroll anchor

private extension View {
    /// Bottom-anchor the coach feed like a chat log. On iOS 18+ the anchor
    /// is limited to the initial offset + alignment roles — deliberately
    /// NOT `.sizeChanges`: with the drag-resizable panel the scroller's
    /// height changes constantly, and the full anchor's re-pin-on-resize
    /// feeds back into safe-area/layout updates until the main thread
    /// livelocks (100% CPU freeze, reproduced on the iOS 26.2 simulator
    /// when focusing the chat field at a custom split). Explicit
    /// `scrollTo` calls already keep the newest message in view on feed
    /// changes, so the size-change role is redundant anyway.
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

// MARK: - Coach panel

/// Chronological coach feed in its own scroller (auto-scrolls to the
/// newest message) with the chat input pinned at the bottom. A grab bar
/// spans the panel's top: dragging it (or anywhere on the slim header row)
/// resizes the board/panel split, double-tapping snaps between full-board
/// and max-read presets.
private struct CoachPanel: View {
    @Bindable var model: GameViewModel
    var chatFocused: FocusState<Bool>.Binding
    /// The board is currently the compact context strip (max-read side of
    /// the threshold) — only used to re-anchor the feed after the morph.
    var isCompact: Bool
    /// Grab-bar drag callbacks (translation in points; positive = down).
    var onDragChanged: (CGFloat) -> Void
    var onDragEnded: () -> Void
    var onSnapToggle: () -> Void

    private var isFocused: Bool { chatFocused.wrappedValue }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            // Grab zone: drag-handle pill + slim header row, one full-width
            // drag target (~44pt). Collapses while typing — every point
            // goes to the feed + input, and the split is suspended until
            // the keyboard dismisses (see GameScreen's layout comment).
            if !isFocused {
                VStack(spacing: 3) {
                    Capsule()
                        .fill(Color(.tertiaryLabel))
                        .frame(width: 36, height: 5)
                    HStack {
                        Label("Coach", systemImage: "graduationcap.fill")
                            .font(.subheadline.bold())
                        Spacer()
                    }
                }
                .frame(maxWidth: .infinity, minHeight: 44, alignment: .top)
                .contentShape(Rectangle())
                // The resize drag lives on this zone ONLY — never on the
                // feed, so it can't fight the feed's scroller.
                .gesture(
                    DragGesture(minimumDistance: 1, coordinateSpace: .global)
                        .onChanged { onDragChanged($0.translation.height) }
                        .onEnded { _ in onDragEnded() }
                )
                .onTapGesture(count: 2) { onSnapToggle() }
                .accessibilityLabel("Coach panel grab bar")
                .accessibilityHint("Drag up or down to resize the coach panel. Double tap to switch between full board and reading mode.")
            }

            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 10) {
                        ForEach(model.coachFeed) { message in
                            messageRow(message)
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
                    .padding(.vertical, 2)
                    .frame(maxWidth: .infinity)
                }
                .scrollDismissesKeyboard(.interactively)
                // Tapping the feed background dismisses the keyboard
                // (simultaneous, so scrolling/taps inside still work).
                .simultaneousGesture(TapGesture().onEnded { GameScreen.hideKeyboard() })
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
                .onChange(of: isFocused) {
                    // Keep the newest message in view while the keyboard
                    // animates in and the panel shrinks.
                    guard isFocused, let last = model.coachFeed.last else { return }
                    DispatchQueue.main.asyncAfter(deadline: .now() + 0.05) {
                        withAnimation { proxy.scrollTo(last.id, anchor: .bottom) }
                    }
                }
                .onChange(of: isCompact) {
                    // Re-anchor to the newest message once the board/strip
                    // morph spring has settled (scrolling mid-resize gets
                    // clobbered by the animation).
                    guard let last = model.coachFeed.last else { return }
                    DispatchQueue.main.asyncAfter(deadline: .now() + 0.4) {
                        withAnimation { proxy.scrollTo(last.id, anchor: .bottom) }
                    }
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)

            HStack(spacing: 8) {
                TextField(model.isReviewing ? "Ask about this position…" : "Ask the coach…",
                          text: $model.chatDraft)
                    .textFieldStyle(.roundedBorder)
                    .focused(chatFocused)
                    .submitLabel(.send)
                    .onSubmit {
                        model.sendChat()
                        chatFocused.wrappedValue = false
                    }
                Button {
                    model.sendChat()
                } label: {
                    Image(systemName: "arrow.up.circle.fill")
                        .font(.title2)
                }
                .disabled(!model.sessionReady || model.isCoachThinking
                          || model.chatDraft.trimmingCharacters(in: .whitespaces).isEmpty)
            }
        }
        .padding([.horizontal, .bottom], isFocused ? 8 : 16)
        // The grab bar hugs the panel's top edge — slim top inset.
        .padding(.top, isFocused ? 8 : 6)
        .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 12))
        .padding(.horizontal, 10)
        .padding(.bottom, isFocused ? 2 : 6)
        .frame(maxHeight: .infinity)
    }

    @ViewBuilder
    private func messageRow(_ message: CoachMessage) -> some View {
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
            .environment(\.openURL, coachLinkAction(for: message))
        } else {
            HStack(alignment: .top, spacing: 6) {
                Image(systemName: message.role == .coach ? "graduationcap" : "person")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .padding(.top, 3)
                VStack(alignment: .leading, spacing: 2) {
                    if message.isReview {
                        Text("review")
                            .font(.caption2.bold())
                            .padding(.horizontal, 6)
                            .padding(.vertical, 1)
                            .background(Brand.annotation.opacity(0.14), in: Capsule())
                            .foregroundStyle(Brand.annotation)
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
                            .environment(\.openURL, coachLinkAction(for: message))
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
    private func coachLinkAction(for message: CoachMessage) -> OpenURLAction {
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

            Button {
                dismiss()
                onNewGame()
            } label: {
                Text("New game")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
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
