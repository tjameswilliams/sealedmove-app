import SwiftUI

/// Coach-backend settings: chattiness, the two-tier provider picker
/// (on-device / Pro Coach), and the Pro Coach status + subscription
/// controls. Apply swaps the backend on the LIVE session — the embedded
/// engine is a singleton, so the session survives.
struct SettingsSheet: View {
    let model: GameViewModel
    @Environment(\.dismiss) private var dismiss

    @State private var settings = BackendSettings.load()
    @State private var chattiness = CoachChattiness.load()

    // Pro Coach registration state (see ProCoachAccount).
    @State private var proCoachStatus = ProCoachAccount.storedStatus()
    @State private var proCoachError: String?
    @State private var isRegisteringProCoach = false

    // Opponent level (see LevelProgress) — the manual override for when
    // the placement or auto-advance got it wrong.
    @State private var selectedBand = LevelProgress.current.band
    @State private var autoAdvance = LevelProgress.autoAdvance
    @State private var placementPending = LevelProgress.isPlacementPending

    var body: some View {
        NavigationStack {
            Form {
                // The active-backend/engine/opponent summary lives here
                // (and as a system feed line) since the coach panel header
                // dropped its subtitle to save vertical space.
                Section("Session") {
                    Label(model.statusLine, systemImage: "cpu")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }

                Section {
                    if placementPending {
                        Label("Evaluation in progress — finish a game and the coach places you from it.",
                              systemImage: "scope")
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                    }
                    Picker("Opponent level", selection: $selectedBand) {
                        ForEach(OpponentLevel.all) { level in
                            Text(level.fullLabel).tag(level.band)
                        }
                    }
                    .pickerStyle(.menu)
                    Toggle("Level up automatically", isOn: $autoAdvance)
                    Button {
                        model.beginPlacement()
                        placementPending = true
                        selectedBand = LevelProgress.current.band
                    } label: {
                        Label("Re-evaluate my level", systemImage: "scope")
                    }
                    .disabled(placementPending)
                } header: {
                    Text("Opponent level")
                } footer: {
                    Text("If the matching feels off, set your own level — it applies to the opponent right away. Auto level-up promotes you after \(LevelProgress.winsToAdvance) wins at a level.")
                }
                .onChange(of: selectedBand) { _, band in
                    // Programmatic syncs (e.g. after Re-evaluate) land on
                    // the already-current band and fall through harmlessly.
                    guard band != LevelProgress.current.band else { return }
                    model.applyOpponentLevel(band)
                    placementPending = false
                }
                .onChange(of: autoAdvance) { _, on in
                    LevelProgress.autoAdvance = on
                }

                Section {
                    Picker("Coach chattiness", selection: $chattiness) {
                        ForEach(CoachChattiness.allCases) { level in
                            Text(level.displayName).tag(level)
                        }
                    }
                    .pickerStyle(.segmented)
                    .labelsHidden()
                } header: {
                    Text("Coach chattiness")
                } footer: {
                    Text(chattiness.summary)
                }
                .onChange(of: chattiness) {
                    // Applies immediately (no Apply needed) and persists.
                    chattiness.save()
                    model.applyChattiness(chattiness)
                }

                Section("Coach provider") {
                    Picker("Provider", selection: $settings.provider) {
                        ForEach(CoachProvider.allCases) { provider in
                            Text(provider.displayName).tag(provider)
                        }
                    }
                    .pickerStyle(.inline)
                    .labelsHidden()
                }

                providerFields

                Section {
                    Button {
                        apply()
                    } label: {
                        Text("Apply")
                            .frame(maxWidth: .infinity)
                            .fontWeight(.semibold)
                    }
                } footer: {
                    Text("Switching keeps the game and the embedded engine; only the coach's voice changes.")
                }

                Section {
                    Link(destination: Legal.sourceURL) {
                        Label("Source & Licenses", systemImage: "chevron.left.forwardslash.chevron.right")
                    }
                    Link(destination: Legal.privacyURL) {
                        Label("Privacy Policy", systemImage: "hand.raised")
                    }
                } header: {
                    Text("About")
                } footer: {
                    // GPLv3 compliance: the engine credit and where the
                    // app's corresponding source lives.
                    Text("Sealed Move's analysis is powered by Stockfish 11, © the Stockfish developers, GPLv3. The app is open source under the same license — Source & Licenses has the details.")
                }
            }
            .navigationTitle("Coach Settings")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") { dismiss() }
                }
            }
        }
    }

    @ViewBuilder
    private var providerFields: some View {
        switch settings.provider {
        case .onDevice:
            Section {
                Label(model.onDeviceBackendName, systemImage: "iphone")
                    .foregroundStyle(.secondary)
            } footer: {
                Text("Runs entirely on this device. Apple Foundation Models when available (iOS 26+ hardware with Apple Intelligence), otherwise a canned coach.")
            }

        case .proCoach:
            Section {
                if isRegisteringProCoach {
                    Label("Registering this device…", systemImage: "arrow.triangle.2.circlepath")
                        .foregroundStyle(.secondary)
                } else if let proCoachError {
                    Label(proCoachError, systemImage: "exclamationmark.triangle.fill")
                        .foregroundStyle(.orange)
                    Button("Retry registration") { registerProCoach() }
                } else {
                    Label(proCoachStatus?.summary ?? "Not registered yet",
                          systemImage: "checkmark.seal")
                        .foregroundStyle(.secondary)
                }
                // Subscribe/Restore — hidden once the server says pro.
                // While the trial runs the button reads as a secondary
                // option under the trial status; after it ends (tier free)
                // it's the section's call to action.
                if proCoachStatus?.tier != .pro {
                    ProCoachPurchaseControls(
                        model: model,
                        secondary: proCoachStatus?.tier == .trial,
                        onSubscribed: { proCoachStatus = ProCoachAccount.storedStatus() })
                }
                // Guideline 3.1.2: terms + privacy alongside the offer.
                Link("Terms of Use", destination: Legal.termsURL)
                    .font(.callout)
                Link("Privacy Policy", destination: Legal.privacyURL)
                    .font(.callout)
            } header: {
                Text("Pro Coach")
            } footer: {
                Text(proCoachFooterText)
            }
            .onAppear { registerProCoach() }
        }
    }

    /// Section footer for Pro Coach: the service blurb, plus Apple's
    /// auto-renew disclosure whenever the Subscribe button is visible.
    @MainActor private var proCoachFooterText: String {
        var text = "Managed coach service — this device registers itself, no API key needed."
        if proCoachStatus?.tier != .pro {
            let price = ProCoachStore.shared.displayPrice ?? "$5.99"
            text += " Pro Coach Monthly is \(price)/month and auto-renews until cancelled in your App Store subscription settings."
        }
        return text
    }

    /// Register (or refresh) this device with the Pro Coach proxy and show
    /// the resulting tier/trial state. No-op when a registration is already
    /// in flight; with a fresh token it returns without a network call.
    private func registerProCoach() {
        guard !isRegisteringProCoach else { return }
        isRegisteringProCoach = true
        proCoachError = nil
        Task {
            do {
                proCoachStatus = try await ProCoachAccount.ensureRegistered()
            } catch {
                proCoachError = error.localizedDescription
            }
            isRegisteringProCoach = false
        }
    }

    /// Neither tier needs validation: on-device has no inputs, and Pro
    /// Coach registers (or refreshes) itself via the model's backend switch.
    private func apply() {
        settings.save()
        model.applyBackend(settings)
        dismiss()
    }
}

#Preview {
    SettingsSheet(model: GameViewModel())
}
