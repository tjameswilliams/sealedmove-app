import SwiftUI

/// The trial-is-over decision sheet: convert to the subscription or switch
/// to the free on-device coach. Presented by the game screen when the Pro
/// Coach trial lapses — proactively at launch (`checkTrialExpiry`) or when
/// the proxy turns a Pro Coach request away for a purchasable reason.
///
/// Both paths are first-class buttons on purpose: the free coach is a real
/// product tier, not a guilt trip — nothing is lost by downgrading, and
/// the games, level, and history carry over either way.
struct TrialExpirySheet: View {
    let model: GameViewModel
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    VStack(alignment: .leading, spacing: 8) {
                        Label {
                            Text("Your free trial has ended")
                                .font(.headline)
                        } icon: {
                            Image(systemName: "graduationcap.fill")
                                .foregroundStyle(Brand.annotation)
                        }
                        Text("Hope the Pro Coach earned its keep. Keep it with a subscription — or switch to the free on-device coach. Your games, level, and history stay either way.")
                            .font(.callout)
                            .foregroundStyle(.secondary)
                    }
                    .padding(.vertical, 4)
                }

                Section {
                    ProCoachPurchaseControls(model: model, onSubscribed: { dismiss() })
                    // Guideline 3.1.2: terms + privacy alongside the offer.
                    Link("Terms of Use", destination: Legal.termsURL)
                        .font(.callout)
                    Link("Privacy Policy", destination: Legal.privacyURL)
                        .font(.callout)
                } header: {
                    Text("Keep Pro Coach")
                } footer: {
                    Text(autoRenewNote)
                }

                Section {
                    Button {
                        model.switchToFreeCoach()
                        dismiss()
                    } label: {
                        Label("Switch to the Free Coach", systemImage: "iphone")
                    }
                } footer: {
                    Text("The on-device coach — engine-checked moves and instant feedback, free forever. Pro Coach stays one tap away in Settings.")
                }
            }
            .navigationTitle("Trial Ended")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Not Now") { dismiss() }
                }
            }
        }
    }

    /// Apple's required auto-renew disclosure, kept to the point.
    @MainActor private var autoRenewNote: String {
        let price = ProCoachStore.shared.displayPrice ?? "$5.99"
        return "Pro Coach Monthly: \(price)/month. Auto-renews until cancelled in your App Store subscription settings. Payment is charged to your Apple Account."
    }
}

#Preview {
    TrialExpirySheet(model: GameViewModel())
}
