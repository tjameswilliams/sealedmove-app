import SwiftUI
import StoreKit

/// Compact purchase controls for the Pro Coach subscription, shared by the
/// settings sheet's Pro Coach section and the standalone paywall sheet.
/// Renders straight off `ProCoachStore.shared.phase`: Subscribe + Restore
/// while unsubscribed, progress while a purchase/restore is in flight, and
/// the pending-server-sync note once StoreKit has granted an entitlement
/// the proxy hasn't confirmed yet.
struct ProCoachPurchaseControls: View {
    let model: GameViewModel
    /// Render Subscribe as a quieter option (the trial is still running,
    /// so it sits under the trial status rather than leading the section).
    var secondary: Bool = false
    /// Called after a purchase or restore lands (subscribed or pending
    /// server sync) — the settings sheet refreshes its status line, the
    /// paywall sheet dismisses.
    var onSubscribed: (() -> Void)?

    @MainActor private var store: ProCoachStore { .shared }

    var body: some View {
        Group {
            switch store.phase {
            case .purchasing:
                progressRow("Contacting the App Store…")
            case .restoring:
                progressRow("Restoring purchases…")
            case .waitingForApproval:
                Label("Waiting for purchase approval (Ask to Buy) — Pro Coach unlocks once it's approved.",
                      systemImage: "hourglass")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            case .subscribed:
                Label("Subscribed — Pro Coach is active.", systemImage: "checkmark.seal.fill")
                    .font(.callout)
                    .foregroundStyle(Brand.tournamentGreen)
            case .pendingServerSync:
                Label("Subscription active on this device — still syncing with the coach service. It finishes on its own; Restore Purchases retries now.",
                      systemImage: "arrow.triangle.2.circlepath")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                restoreButton
            case .idle, .failed:
                if case .failed(let message) = store.phase {
                    Label(message, systemImage: "exclamationmark.triangle.fill")
                        .font(.callout)
                        .foregroundStyle(.orange)
                } else if let loadError = store.productLoadError {
                    // No StoreKit context (or no network) — say so instead
                    // of showing a dead button with no price.
                    Label(loadError, systemImage: "exclamationmark.triangle.fill")
                        .font(.callout)
                        .foregroundStyle(.orange)
                }
                subscribeButton
                restoreButton
            }
        }
        .task { await store.loadProduct() }
    }

    @MainActor private var subscribeButton: some View {
        Button {
            Task {
                await store.purchase()
                finishIfSubscribed()
            }
        } label: {
            Text("Subscribe — \(store.displayPrice ?? "…")/month")
                .fontWeight(secondary ? .regular : .semibold)
        }
        .disabled(store.product == nil)
    }

    @MainActor private var restoreButton: some View {
        Button {
            Task {
                await store.restorePurchases()
                finishIfSubscribed()
            }
        } label: {
            Text("Restore Purchases")
                .font(.callout)
        }
    }

    private func progressRow(_ text: String) -> some View {
        HStack(spacing: 6) {
            ProgressView().controlSize(.small)
            Text(text)
                .font(.callout)
                .foregroundStyle(.secondary)
        }
    }

    /// Mirror the settings sheet's Apply: when Pro Coach is the chosen
    /// provider, swap the live session onto the refreshed (pro)
    /// registration so the very next coach reply uses it.
    @MainActor private func finishIfSubscribed() {
        guard store.phase == .subscribed || store.phase == .pendingServerSync else { return }
        let settings = BackendSettings.load()
        if settings.provider == .proCoach {
            model.applyBackend(settings)
        }
        onSubscribed?()
    }
}

// The standalone paywall sheet this file used to hold was replaced by
// `TrialExpirySheet` (which adds the switch-to-free path); the shared
// purchase controls above are its surviving tenant.
