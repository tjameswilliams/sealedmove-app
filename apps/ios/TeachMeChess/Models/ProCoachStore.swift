import Foundation
import Observation
import StoreKit

/// StoreKit 2 service for the Pro Coach subscription
/// (`dev.teachmechess.app.pro.monthly`): loads the product, runs purchases
/// and restores, and keeps the proxy's device record linked to the App
/// Store transaction.
///
/// The link is the `appAccountToken` purchase option — set to this device's
/// `ProCoachAccount.deviceId` — which the App Store echoes on every renewal
/// notification, so the webhook can find the device record without any
/// account. Belt and braces, the app also POSTs the signed transaction to
/// `/v1/entitlements/claim` right after purchase/restore; the server tier
/// stays the authority, so a successful claim is followed by a forced
/// re-register to pull the fresh (pro) status.
///
/// Claim failures do NOT fail the purchase: StoreKit already granted the
/// entitlement (and local StoreKit-configuration testing signs transactions
/// with a local cert the server rightly rejects with 401), so the store
/// persists a "pending server sync" state and treats StoreKit's own
/// `Transaction.currentEntitlement` as the optimistic local truth until a
/// later claim or the webhook lands.
@MainActor
@Observable
final class ProCoachStore {
    static let productID = "dev.teachmechess.app.pro.monthly"

    /// Where the purchase/restore flow currently stands — the paywall UI
    /// renders straight off this.
    enum Phase: Equatable {
        case idle
        /// `product.purchase()` is in flight (App Store sheet up).
        case purchasing
        /// `AppStore.sync()` + entitlement walk in flight.
        case restoring
        /// Ask to Buy: the purchase awaits a family approver; the
        /// `Transaction.updates` listener picks up the eventual approval.
        case waitingForApproval
        /// Purchase (or restore) claimed and linked — server tier is pro.
        case subscribed
        /// StoreKit granted the entitlement but the claim didn't land
        /// (offline, 401 from a locally-signed test transaction, 503).
        /// Persisted across launches; retried on restore and on every
        /// transaction update.
        case pendingServerSync
        case failed(String)
    }

    /// One instance for the whole app: the `Transaction.updates` listener
    /// must outlive any single sheet, and both paywall entry points render
    /// the same state.
    static let shared = ProCoachStore()

    private(set) var phase: Phase = .idle
    private(set) var product: Product?
    /// Set when the product fetch failed or came back empty — the normal
    /// case when no StoreKit context exists (e.g. a simulator launch that
    /// bypassed the scheme's Configuration.storekit).
    private(set) var productLoadError: String?
    /// Mirror of `hasLocalEntitlement()` for synchronous view reads,
    /// refreshed on load, purchase, restore, and every transaction update.
    private(set) var localEntitlementActive = false

    /// "$5.99" once the product loads; nil while loading/unavailable.
    var displayPrice: String? { product?.displayPrice }

    private var updatesTask: Task<Void, Never>?
    private static let pendingSyncKey = "procoach.pendingClaimSync"

    private init() {
        if UserDefaults.standard.bool(forKey: Self.pendingSyncKey) {
            phase = .pendingServerSync
        }
        // Renewals, Ask to Buy approvals, and revocations arriving while
        // the app runs — each one re-claims and re-pulls server status.
        updatesTask = Task { [weak self] in
            for await update in Transaction.updates {
                await self?.handleUpdate(update)
            }
        }
    }

    // MARK: - Product

    /// Fetch the subscription product (idempotent — returns immediately
    /// once cached) and refresh the local-entitlement mirror. A failure is
    /// display state, never fatal: the paywall shows "unavailable" and the
    /// rest of the app is untouched.
    func loadProduct() async {
        await refreshLocalEntitlement()
        guard product == nil else { return }
        do {
            product = try await Product.products(for: [Self.productID]).first
            productLoadError = product == nil
                ? "The subscription isn't available from the App Store right now."
                : nil
        } catch {
            productLoadError = "Couldn't reach the App Store: \(error.localizedDescription)"
        }
    }

    // MARK: - Purchase

    /// Run the App Store purchase sheet. The `appAccountToken` option
    /// carries the device id so the server can link every renewal back to
    /// this device's record. On a verified transaction: finish it, claim it
    /// with the device JWT, and force-refresh the server status (tier
    /// should come back pro).
    func purchase() async {
        guard phase != .purchasing && phase != .restoring else { return }
        if product == nil { await loadProduct() }
        guard let product else {
            phase = .failed(productLoadError
                ?? "The subscription isn't available right now.")
            return
        }
        phase = .purchasing

        var options: Set<Product.PurchaseOption> = []
        if let token = UUID(uuidString: ProCoachAccount.deviceId) {
            options.insert(.appAccountToken(token))
        }
        do {
            switch try await product.purchase(options: options) {
            case .success(let verification):
                await handlePurchased(verification)
            case .userCancelled:
                phase = restingPhase
            case .pending:
                // Ask to Buy — the updates listener finishes the job when
                // (if) the approval arrives.
                phase = .waitingForApproval
            @unknown default:
                phase = restingPhase
            }
        } catch {
            phase = .failed(error.localizedDescription)
        }
    }

    /// Verified transaction -> finish -> claim -> refresh. An unverified
    /// result means StoreKit itself couldn't vouch for the transaction —
    /// that one IS a failure (nothing was granted).
    private func handlePurchased(_ verification: VerificationResult<Transaction>) async {
        guard case .verified(let transaction) = verification else {
            phase = .failed("The App Store couldn't verify that purchase.")
            return
        }
        await transaction.finish()
        await refreshLocalEntitlement()
        await claimAndRefresh(jws: verification.jwsRepresentation)
    }

    // MARK: - Restore

    /// Restore Purchases: force a transaction-history sync (may show an App
    /// Store sign-in), then claim the newest live subscription transaction.
    /// Also the manual retry path for a pending server sync.
    func restorePurchases() async {
        guard phase != .purchasing && phase != .restoring else { return }
        phase = .restoring
        // sync() throws when the student cancels the sign-in sheet — the
        // local entitlement cache is still worth walking, so carry on.
        try? await AppStore.sync()

        var newest: (transaction: Transaction, jws: String)?
        for await result in Transaction.currentEntitlements {
            guard case .verified(let transaction) = result,
                  transaction.productID == Self.productID,
                  transaction.revocationDate == nil
            else { continue }
            if newest == nil || transaction.purchaseDate > newest!.transaction.purchaseDate {
                newest = (transaction, result.jwsRepresentation)
            }
        }
        await refreshLocalEntitlement()
        guard let newest else {
            phase = .failed("No Pro Coach subscription to restore on this Apple Account.")
            return
        }
        await claimAndRefresh(jws: newest.jws)
    }

    // MARK: - Claim + server refresh

    /// Submit the signed transaction to the proxy's claim endpoint, then
    /// force a re-register so the fresh server tier (pro) replaces the
    /// stored status. Claim failure keeps the purchase: pending-sync state
    /// persists and the local entitlement carries the UI — except a 409
    /// `subscription_not_active`, where the server verified the JWS and is
    /// telling us the subscription genuinely isn't live.
    private func claimAndRefresh(jws: String) async {
        do {
            try await ProCoachAccount.claimEntitlement(signedTransaction: jws)
            _ = try? await ProCoachAccount.ensureRegistered(forceRefresh: true)
            setPendingSync(false)
            phase = .subscribed
        } catch {
            if case ProCoachError.server(let code, _) = error,
               code == "subscription_not_active" {
                setPendingSync(false)
                phase = .failed("The App Store reports that subscription is no longer active.")
                return
            }
            setPendingSync(true)
            phase = .pendingServerSync
        }
    }

    // MARK: - Transaction updates

    private func handleUpdate(_ result: VerificationResult<Transaction>) async {
        guard case .verified(let transaction) = result,
              transaction.productID == Self.productID
        else { return }
        await transaction.finish()
        await refreshLocalEntitlement()
        if transaction.revocationDate != nil {
            // Refund/revocation: the webhook downgrades the record — pull
            // fresh status so the app agrees with the server.
            setPendingSync(false)
            _ = try? await ProCoachAccount.ensureRegistered(forceRefresh: true)
            phase = .idle
            return
        }
        await claimAndRefresh(jws: result.jwsRepresentation)
    }

    // MARK: - Local entitlement

    /// StoreKit's on-device answer to "is the subscription live on this
    /// Apple Account?" — the optimistic check while a claim hasn't reached
    /// the server (pending sync).
    func hasLocalEntitlement() async -> Bool {
        guard case .verified(let transaction)? =
                await Transaction.currentEntitlement(for: Self.productID),
              transaction.revocationDate == nil
        else { return false }
        return true
    }

    private func refreshLocalEntitlement() async {
        localEntitlementActive = await hasLocalEntitlement()
    }

    // MARK: - Pending-sync persistence

    /// Where the phase settles when nothing is in flight.
    private var restingPhase: Phase {
        UserDefaults.standard.bool(forKey: Self.pendingSyncKey)
            ? .pendingServerSync : .idle
    }

    private func setPendingSync(_ pending: Bool) {
        UserDefaults.standard.set(pending, forKey: Self.pendingSyncKey)
    }
}
