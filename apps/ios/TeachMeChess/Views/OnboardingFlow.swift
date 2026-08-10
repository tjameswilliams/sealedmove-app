import SwiftUI

/// First-launch onboarding: three animated pitch pages, the trial-or-free
/// choice, then the starting-level pick ("evaluate me" or a manual band).
/// Runs as the app's root INSTEAD of `GameScreen`, so every choice —
/// provider and level — lands in UserDefaults before the coach session is
/// ever constructed.
///
/// Styling: the flow leans fully into the Sealed Move palette — tournament
/// felt background, buff/paper type, the red annotation pen reserved for
/// the moments that matter (the trial CTA, the placement pick).
struct OnboardingFlow: View {
    /// UserDefaults flag the app root gates on.
    static let completedKey = "onboarding.completed"

    /// Called once the flow has persisted its choices — the app root swaps
    /// to the game screen.
    let onFinished: () -> Void

    private enum Phase {
        case pitch
        case level
    }

    @State private var phase: Phase = .pitch
    @State private var page = 0
    /// Chosen on the plan page; applied at finish.
    @State private var choseTrial = false
    @State private var price: String?

    /// Felt-table backdrop, darker than the board's green.
    private static let felt = Color(red: 0.10, green: 0.16, blue: 0.10)
    private static let feltDeep = Color(red: 0.05, green: 0.09, blue: 0.05)
    static let paper = Brand.boardLight

    private static let lastPitchPage = 3

    var body: some View {
        ZStack {
            LinearGradient(colors: [Self.felt, Self.feltDeep],
                           startPoint: .top, endPoint: .bottom)
                .ignoresSafeArea()

            switch phase {
            case .pitch:
                pitchPager
                    .transition(.asymmetric(
                        insertion: .identity,
                        removal: .move(edge: .leading).combined(with: .opacity)))
            case .level:
                LevelPickPage(
                    onEvaluate: { finish(placement: true, band: nil) },
                    onPick: { band in finish(placement: false, band: band) })
                    .transition(.move(edge: .trailing).combined(with: .opacity))
            }
        }
        .animation(.spring(response: 0.45, dampingFraction: 0.85), value: phase)
        .task {
            await ProCoachStore.shared.loadProduct()
            price = ProCoachStore.shared.displayPrice
        }
    }

    // MARK: - Pitch pager (pages 0–3)

    private var pitchPager: some View {
        VStack(spacing: 0) {
            HStack {
                Spacer()
                if page < Self.lastPitchPage {
                    Button("Skip") {
                        withAnimation(.easeInOut(duration: 0.35)) { page = Self.lastPitchPage }
                    }
                    .font(.subheadline)
                    .foregroundStyle(Self.paper.opacity(0.55))
                    .padding(.trailing, 22)
                }
            }
            .frame(height: 36)

            TabView(selection: $page) {
                BrandPage().tag(0)
                CoachPitchPage().tag(1)
                LadderPitchPage().tag(2)
                PlanChoicePage(
                    price: price,
                    onTrial: { choose(trial: true) },
                    onFree: { choose(trial: false) })
                    .tag(3)
            }
            .tabViewStyle(.page(indexDisplayMode: .never))

            pageDots
                .padding(.bottom, 10)

            if page < Self.lastPitchPage {
                Button {
                    withAnimation(.easeInOut(duration: 0.35)) { page += 1 }
                } label: {
                    Text("Continue")
                        .font(.headline)
                        .foregroundStyle(Self.feltDeep)
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 15)
                        .background(Self.paper, in: Capsule())
                }
                .padding(.horizontal, 28)
                .padding(.bottom, 18)
            } else {
                // The plan page carries its own two CTAs — keep the space
                // so the pager doesn't jump.
                Color.clear.frame(height: 18)
            }
        }
    }

    private var pageDots: some View {
        HStack(spacing: 8) {
            ForEach(0...Self.lastPitchPage, id: \.self) { i in
                Capsule()
                    .fill(i == page ? Brand.annotation : Self.paper.opacity(0.3))
                    .frame(width: i == page ? 22 : 7, height: 7)
            }
        }
        .animation(.spring(response: 0.35, dampingFraction: 0.8), value: page)
    }

    // MARK: - Completion

    private func choose(trial: Bool) {
        choseTrial = trial
        withAnimation { phase = .level }
    }

    /// Persist every choice, mark onboarding done, and hand off. The
    /// provider save is all the Pro Coach path needs: the game screen's
    /// launch restore applies it, which walks the consent sheet and the
    /// trial registration in the right order.
    private func finish(placement: Bool, band: Int?) {
        if placement {
            LevelProgress.beginPlacement()
        } else if let band {
            LevelProgress.setBand(band)
        }
        var settings = BackendSettings.load()
        settings.provider = choseTrial ? .proCoach : .onDevice
        settings.save()
        UserDefaults.standard.set(true, forKey: Self.completedKey)
        onFinished()
    }
}

// MARK: - Shared page scaffolding

/// Title + subtitle under a page's hero, with the staggered fade-up the
/// whole flow uses.
private struct PitchCopy: View {
    let title: String
    let subtitle: String
    var appeared: Bool

    var body: some View {
        VStack(spacing: 12) {
            Text(title)
                .font(.system(.largeTitle, design: .serif).bold())
                .foregroundStyle(OnboardingFlow.paper)
                .multilineTextAlignment(.center)
            Text(subtitle)
                .font(.callout)
                .foregroundStyle(OnboardingFlow.paper.opacity(0.75))
                .multilineTextAlignment(.center)
                .padding(.horizontal, 34)
        }
        .opacity(appeared ? 1 : 0)
        .offset(y: appeared ? 0 : 18)
    }
}

// MARK: - Page 0: brand

/// The board assembles square by square in a diagonal wave, then the brand
/// wordmark settles in.
private struct BrandPage: View {
    @State private var appeared = false

    private let files = 6

    var body: some View {
        VStack(spacing: 36) {
            Spacer()
            boardAssembly
            PitchCopy(
                title: "Sealed Move",
                subtitle: "Every move, annotated. A patient coach for every game you play.",
                appeared: appeared)
            Spacer()
            Spacer()
        }
        .onAppear {
            withAnimation(.spring(response: 0.7, dampingFraction: 0.8).delay(0.55)) {
                appeared = true
            }
        }
    }

    private var boardAssembly: some View {
        Grid(horizontalSpacing: 3, verticalSpacing: 3) {
            ForEach(0..<files, id: \.self) { rank in
                GridRow {
                    ForEach(0..<files, id: \.self) { file in
                        RoundedRectangle(cornerRadius: 4)
                            .fill((rank + file).isMultiple(of: 2)
                                  ? Brand.boardLight : Brand.boardDark)
                            .frame(width: 34, height: 34)
                            .opacity(appeared ? 1 : 0)
                            .scaleEffect(appeared ? 1 : 0.4)
                            .animation(
                                .spring(response: 0.5, dampingFraction: 0.7)
                                    .delay(Double(rank + file) * 0.055),
                                value: appeared)
                    }
                }
            }
        }
        .overlay {
            // The coach's pen: one annotated square, circled in red.
            Circle()
                .strokeBorder(Brand.annotation, lineWidth: 3)
                .frame(width: 42, height: 42)
                .offset(x: 55.5, y: -18.5)
                .opacity(appeared ? 1 : 0)
                .scaleEffect(appeared ? 1 : 1.7)
                .animation(.spring(response: 0.55, dampingFraction: 0.6).delay(1.0),
                           value: appeared)
        }
    }
}

// MARK: - Page 1: the coach

/// Mock coach-feed moments sliding in one after another — the product in
/// three bubbles.
private struct CoachPitchPage: View {
    @State private var appeared = false

    var body: some View {
        VStack(spacing: 30) {
            Spacer()
            VStack(alignment: .leading, spacing: 12) {
                bubble(icon: "graduationcap",
                       text: "Nf3 — good square. You're developing toward the center and keeping e5 honest.",
                       delay: 0.15)
                verdictChips(delay: 0.45)
                bubble(icon: "eye",
                       text: "Careful — that bishop is eyeing f7 now.",
                       tinted: true, delay: 0.75)
                bubble(icon: "graduationcap",
                       text: "Ask me anything, any time: \u{201C}why was that a blunder?\u{201D}",
                       delay: 1.05)
            }
            .padding(.horizontal, 30)
            PitchCopy(
                title: "A coach in your corner",
                subtitle: "Real engine analysis behind every single move, explained in plain words — not just a number.",
                appeared: appeared)
            Spacer()
            Spacer()
        }
        .onAppear {
            withAnimation(.spring(response: 0.6, dampingFraction: 0.85).delay(1.2)) {
                appeared = true
            }
        }
    }

    private func bubble(icon: String, text: String, tinted: Bool = false,
                        delay: Double) -> some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: icon)
                .font(.caption)
                .foregroundStyle(tinted ? Brand.annotation : OnboardingFlow.paper.opacity(0.7))
                .padding(.top, 4)
            Text(text)
                .font(.system(.callout, design: .serif))
                .foregroundStyle(OnboardingFlow.paper)
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.white.opacity(tinted ? 0.13 : 0.08),
                    in: RoundedRectangle(cornerRadius: 12))
        .modifier(SlideIn(appeared: appeared, delay: delay))
    }

    private func verdictChips(delay: Double) -> some View {
        HStack(spacing: 8) {
            chip("best", color: .green)
            chip("good", color: .teal)
            chip("inaccuracy", color: .yellow)
            chip("blunder", color: Brand.annotation)
        }
        .modifier(SlideIn(appeared: appeared, delay: delay))
    }

    private func chip(_ label: String, color: Color) -> some View {
        Text(label)
            .font(.caption2.bold())
            .padding(.horizontal, 10)
            .padding(.vertical, 4)
            .background(color.opacity(0.25), in: Capsule())
            .foregroundStyle(color)
    }
}

/// Fade-up entrance with a per-element delay, driven by one appeared flag.
private struct SlideIn: ViewModifier {
    var appeared: Bool
    var delay: Double

    func body(content: Content) -> some View {
        content
            .opacity(appeared ? 1 : 0)
            .offset(y: appeared ? 0 : 24)
            .animation(.spring(response: 0.55, dampingFraction: 0.8).delay(delay),
                       value: appeared)
    }
}

// MARK: - Page 2: the ladder

/// The level ladder rises band by band — the promise that the opponent
/// grows with the student.
private struct LadderPitchPage: View {
    @State private var appeared = false

    var body: some View {
        VStack(spacing: 34) {
            Spacer()
            ladder
            PitchCopy(
                title: "It levels with you",
                subtitle: "Win games to climb from Newcomer to Master Track. The coach tracks what you've learned — and what to work on next.",
                appeared: appeared)
            Spacer()
            Spacer()
        }
        .onAppear {
            withAnimation(.spring(response: 0.6, dampingFraction: 0.85).delay(0.9)) {
                appeared = true
            }
        }
    }

    private var ladder: some View {
        HStack(alignment: .bottom, spacing: 7) {
            ForEach(Array(OpponentLevel.all.enumerated()), id: \.element) { i, level in
                VStack(spacing: 6) {
                    if i == OpponentLevel.all.count - 1 {
                        Image(systemName: "crown.fill")
                            .font(.caption)
                            .foregroundStyle(Brand.annotation)
                            .opacity(appeared ? 1 : 0)
                            .animation(.easeOut.delay(1.1), value: appeared)
                    }
                    RoundedRectangle(cornerRadius: 4)
                        .fill(i < 3 ? Brand.boardLight.opacity(0.9) : Brand.boardDark)
                        .overlay(RoundedRectangle(cornerRadius: 4)
                            .strokeBorder(OnboardingFlow.paper.opacity(0.25)))
                        .frame(width: 26, height: appeared ? CGFloat(26 + i * 13) : 8)
                        .animation(.spring(response: 0.5, dampingFraction: 0.72)
                            .delay(Double(i) * 0.09), value: appeared)
                }
                .accessibilityLabel(level.fullLabel)
            }
        }
        .frame(height: 150, alignment: .bottom)
    }
}

// MARK: - Page 3: plan choice

/// The decision page: start the Pro Coach trial or play free. Both cards
/// state exactly what they are; the subscription terms sit underneath so
/// nobody is surprised later.
private struct PlanChoicePage: View {
    let price: String?
    let onTrial: () -> Void
    let onFree: () -> Void
    @State private var appeared = false

    var body: some View {
        ScrollView {
            VStack(spacing: 18) {
                PitchCopy(
                    title: "How do you want to start?",
                    subtitle: "Both come with the full game, the engine, and your coach.",
                    appeared: appeared)
                    .padding(.top, 8)

                trialCard
                    .modifier(SlideIn(appeared: appeared, delay: 0.25))
                freeCard
                    .modifier(SlideIn(appeared: appeared, delay: 0.4))

                Text("After the trial, Pro Coach is \(price ?? "$5.99")/month — auto-renews until cancelled in your App Store settings. Subscribe, restore, or switch coaches anytime in Settings.")
                    .font(.caption)
                    .foregroundStyle(OnboardingFlow.paper.opacity(0.55))
                    .multilineTextAlignment(.center)
                    .padding(.horizontal, 30)
                    .modifier(SlideIn(appeared: appeared, delay: 0.55))

                // Guideline 3.1.2: terms + privacy at every surface that
                // offers the subscription — the plan page included.
                HStack(spacing: 18) {
                    Link("Terms of Use", destination: Legal.termsURL)
                    Link("Privacy Policy", destination: Legal.privacyURL)
                }
                .font(.caption.bold())
                .foregroundStyle(OnboardingFlow.paper.opacity(0.7))
                .padding(.bottom, 8)
                .modifier(SlideIn(appeared: appeared, delay: 0.6))
            }
            .padding(.horizontal, 24)
            .padding(.bottom, 16)
        }
        .onAppear {
            withAnimation(.spring(response: 0.55, dampingFraction: 0.85)) {
                appeared = true
            }
        }
    }

    private var trialCard: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Label("Pro Coach", systemImage: "graduationcap.fill")
                    .font(.headline)
                    .foregroundStyle(OnboardingFlow.paper)
                Spacer()
                Text("7-DAY FREE TRIAL")
                    .font(.caption2.bold())
                    .padding(.horizontal, 8)
                    .padding(.vertical, 3)
                    .background(Brand.annotation, in: Capsule())
                    .foregroundStyle(.white)
            }
            benefit("Our sharpest coaching, powered by cloud AI")
            benefit("Deeper explanations and richer game stories")
            benefit("No account, no sign-up — it just starts")

            Button(action: onTrial) {
                Text("Start my free trial")
                    .font(.headline)
                    .foregroundStyle(.white)
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 13)
                    .background(Brand.annotation, in: Capsule())
            }
            .padding(.top, 4)
        }
        .padding(16)
        .background(Color.white.opacity(0.08), in: RoundedRectangle(cornerRadius: 16))
        .overlay(RoundedRectangle(cornerRadius: 16)
            .strokeBorder(Brand.annotation.opacity(0.6), lineWidth: 1.5))
    }

    private var freeCard: some View {
        VStack(alignment: .leading, spacing: 10) {
            Label("Free Coach", systemImage: "iphone")
                .font(.headline)
                .foregroundStyle(OnboardingFlow.paper)
            benefit("Runs entirely on your iPhone — nothing leaves it")
            benefit("Engine-checked moves and instant feedback")
            benefit("Free forever")

            Button(action: onFree) {
                Text("Play free")
                    .font(.headline)
                    .foregroundStyle(OnboardingFlow.paper)
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 13)
                    .background(Color.white.opacity(0.12), in: Capsule())
                    .overlay(Capsule().strokeBorder(OnboardingFlow.paper.opacity(0.5)))
            }
            .padding(.top, 4)
        }
        .padding(16)
        .background(Color.white.opacity(0.05), in: RoundedRectangle(cornerRadius: 16))
    }

    private func benefit(_ text: String) -> some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: "checkmark")
                .font(.caption.bold())
                .foregroundStyle(Brand.boardLight)
                .padding(.top, 2)
            Text(text)
                .font(.subheadline)
                .foregroundStyle(OnboardingFlow.paper.opacity(0.85))
        }
    }
}

// MARK: - Level pick

/// The last stop: "evaluate me" (recommended) or a manual band. Either
/// choice completes onboarding.
private struct LevelPickPage: View {
    let onEvaluate: () -> Void
    let onPick: (Int) -> Void
    @State private var appeared = false

    var body: some View {
        VStack(spacing: 0) {
            PitchCopy(
                title: "Where should we start?",
                subtitle: "Too easy is boring, too hard is bruising. Let the coach find your level — or pick it yourself.",
                appeared: appeared)
                .padding(.top, 40)
                .padding(.bottom, 22)

            Button(action: onEvaluate) {
                VStack(spacing: 6) {
                    Label("Evaluate me", systemImage: "scope")
                        .font(.headline)
                        .foregroundStyle(.white)
                    Text("Play one game — the coach estimates your strength and places you. Recommended.")
                        .font(.caption)
                        .foregroundStyle(.white.opacity(0.85))
                        .multilineTextAlignment(.center)
                }
                .frame(maxWidth: .infinity)
                .padding(.vertical, 16)
                .padding(.horizontal, 14)
                .background(Brand.annotation, in: RoundedRectangle(cornerRadius: 16))
            }
            .padding(.horizontal, 24)
            .modifier(SlideIn(appeared: appeared, delay: 0.2))

            HStack {
                line
                Text("or pick a level")
                    .font(.caption)
                    .foregroundStyle(OnboardingFlow.paper.opacity(0.55))
                line
            }
            .padding(.horizontal, 30)
            .padding(.vertical, 16)
            .modifier(SlideIn(appeared: appeared, delay: 0.3))

            ScrollView {
                VStack(spacing: 8) {
                    ForEach(OpponentLevel.all) { level in
                        Button {
                            onPick(level.band)
                        } label: {
                            HStack {
                                Text("Level \(level.ordinal)")
                                    .font(.subheadline.bold().monospacedDigit())
                                    .foregroundStyle(OnboardingFlow.paper)
                                    .frame(width: 68, alignment: .leading)
                                Text(level.displayName)
                                    .font(.subheadline)
                                    .foregroundStyle(OnboardingFlow.paper.opacity(0.9))
                                Spacer()
                                Text("~\(level.band)")
                                    .font(.caption.monospacedDigit())
                                    .foregroundStyle(OnboardingFlow.paper.opacity(0.5))
                            }
                            .padding(.horizontal, 14)
                            .padding(.vertical, 11)
                            .background(Color.white.opacity(0.07),
                                        in: RoundedRectangle(cornerRadius: 12))
                        }
                    }
                }
                .padding(.horizontal, 24)
                .padding(.bottom, 24)
            }
            .modifier(SlideIn(appeared: appeared, delay: 0.4))
        }
        .onAppear {
            withAnimation(.spring(response: 0.55, dampingFraction: 0.85).delay(0.15)) {
                appeared = true
            }
        }
    }

    private var line: some View {
        Rectangle()
            .fill(OnboardingFlow.paper.opacity(0.2))
            .frame(height: 1)
    }
}

#Preview {
    OnboardingFlow(onFinished: {})
}
