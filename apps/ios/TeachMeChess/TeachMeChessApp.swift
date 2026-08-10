import SwiftUI

@main
struct TeachMeChessApp: App {
    /// Root gate: onboarding first, the game after. The flag flips exactly
    /// once, inside `OnboardingFlow.finish` — and the game screen (with its
    /// coach session) is only CONSTRUCTED after that, so the level and
    /// provider chosen in onboarding are already persisted when the session
    /// starts.
    @AppStorage(OnboardingFlow.completedKey) private var onboardingComplete = false

    init() {
        // Installs that predate onboarding (a saved student profile exists)
        // skip the pitch — they've long since made these choices.
        if !UserDefaults.standard.bool(forKey: OnboardingFlow.completedKey),
           GameViewModel.hasSavedProfile {
            UserDefaults.standard.set(true, forKey: OnboardingFlow.completedKey)
        }
    }

    var body: some Scene {
        WindowGroup {
            ZStack {
                if onboardingComplete {
                    GameScreen()
                        .transition(.opacity)
                } else {
                    OnboardingFlow {
                        onboardingComplete = true
                    }
                    .transition(.asymmetric(
                        insertion: .identity,
                        removal: .move(edge: .top).combined(with: .opacity)))
                }
            }
            .animation(.easeInOut(duration: 0.45), value: onboardingComplete)
        }
    }
}
