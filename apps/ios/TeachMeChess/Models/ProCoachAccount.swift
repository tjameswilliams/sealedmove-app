import CryptoKit
import DeviceCheck
import Foundation

/// Tier + trial state for this device, as last reported by the proxy's
/// register endpoint.
struct ProCoachStatus: Equatable {
    enum Tier: String {
        case trial, free, pro
    }

    let tier: Tier
    let trialExpiresAt: Date?
    let trialRequestsRemaining: Int?

    /// One-line summary for the settings sheet and feed.
    var summary: String {
        switch tier {
        case .pro:
            return "Pro"
        case .free:
            return "Trial ended — free coach available"
        case .trial:
            var parts: [String] = []
            if let trialExpiresAt {
                parts.append("expires \(trialExpiresAt.formatted(date: .abbreviated, time: .omitted))")
            }
            if let trialRequestsRemaining {
                parts.append("\(trialRequestsRemaining) requests left")
            }
            return parts.isEmpty ? "Trial" : "Trial — " + parts.joined(separator: ", ")
        }
    }
}

/// Why Pro Coach registration failed — network trouble (retryable, nothing
/// wrong with the account) is kept distinct from a proxy rejection.
enum ProCoachError: LocalizedError {
    /// The request never completed (offline, DNS, timeout).
    case network(String)
    /// The proxy answered with an error body ({"error":{"code":…}}).
    case server(code: String, message: String)
    /// A 2xx answer that didn't decode — proxy/client version mismatch.
    case invalidResponse

    var errorDescription: String? {
        switch self {
        case .network(let detail): return "network error: \(detail)"
        case .server(let code, let message): return "\(message) [\(code)]"
        case .invalidResponse: return "unexpected response from the Pro Coach service"
        }
    }
}

/// Device registration for the Pro Coach proxy: a stable anonymous device id
/// and the 30-day device JWT live in the Keychain; tier/trial metadata (not
/// secret) in UserDefaults. The JWT is the "API key" the coach backend sends
/// as its bearer token — no provider key ever reaches the app.
///
/// Dev-mode registration: a bare `{"deviceId": …}` POST, no App Attest yet
/// (the proxy runs with ALLOW_UNATTESTED_REGISTRATION=1). Re-registering the
/// same deviceId is safe and keeps the device's tier/trial state — it's also
/// how an expiring JWT gets refreshed.
enum ProCoachAccount {
    /// Deployed proxy stage. Overridable via UserDefaults ("procoach.baseURL")
    /// so a TestFlight build can be repointed without a release.
    static let defaultBaseURL = "https://2eme5zvcpf.execute-api.us-east-1.amazonaws.com/v"
    static let baseURLOverrideKey = "procoach.baseURL"

    /// Model requested through the proxy (it rewrites retired ids anyway).
    static let model = "deepseek-v4-flash"

    /// Refresh the JWT when less than this remains before `exp`.
    private static let refreshWindow: TimeInterval = 3 * 24 * 3600

    private static let deviceIdAccount = "procoach.device-id"
    private static let tokenAccount = "procoach.token"

    private enum Keys {
        static let tier = "procoach.tier"
        static let trialExpiresAt = "procoach.trialExpiresAt"
        static let trialRequestsRemaining = "procoach.trialRequestsRemaining"
    }

    static var baseURL: String {
        UserDefaults.standard.string(forKey: baseURLOverrideKey) ?? defaultBaseURL
    }

    // MARK: - Cloud-AI disclosure consent

    /// App Store guideline 5.1.2(i): game content may only reach the
    /// third-party AI behind the proxy after the student explicitly
    /// agrees. Recorded once, locally; the backend switch is gated on it
    /// (see `GameViewModel.applyProCoachBackend`).
    private static let cloudConsentKey = "procoach.cloudConsent"

    static var hasCloudConsent: Bool {
        UserDefaults.standard.bool(forKey: cloudConsentKey)
    }

    static func recordCloudConsent() {
        UserDefaults.standard.set(true, forKey: cloudConsentKey)
    }

    /// Stable anonymous device identity, minted on first use. Keychain, not
    /// UserDefaults: it must survive reinstalls, or every reinstall would
    /// look like a fresh device (and a fresh trial).
    static var deviceId: String {
        if let id = KeychainStore.load(account: deviceIdAccount) { return id }
        let id = UUID().uuidString.lowercased()
        KeychainStore.save(account: deviceIdAccount, value: id)
        return id
    }

    /// The device JWT to send as the bearer key, if one is stored.
    static func storedToken() -> String? {
        KeychainStore.load(account: tokenAccount)
    }

    /// Last-known status without touching the network (nil = never
    /// registered). The record on the server is the real authority; this is
    /// display state.
    static func storedStatus() -> ProCoachStatus? {
        let defaults = UserDefaults.standard
        guard let raw = defaults.string(forKey: Keys.tier),
              let tier = ProCoachStatus.Tier(rawValue: raw)
        else { return nil }
        let expires = (defaults.object(forKey: Keys.trialExpiresAt) as? Double)
            .map { Date(timeIntervalSince1970: $0) }
        let remaining = defaults.object(forKey: Keys.trialRequestsRemaining) as? Int
        return ProCoachStatus(tier: tier, trialExpiresAt: expires,
                              trialRequestsRemaining: remaining)
    }

    /// Make sure a usable device JWT exists: if the stored one has more than
    /// `refreshWindow` left before expiry, return the stored status without
    /// a network call (cheap to call on every Apply); otherwise (re-)register
    /// this deviceId with the proxy and persist the fresh token + status.
    /// `forceRefresh` skips the freshness shortcut and always hits the
    /// network — used right after an entitlement claim (or webhook-driven
    /// change) so the server's new tier lands immediately.
    static func ensureRegistered(forceRefresh: Bool = false) async throws -> ProCoachStatus {
        if !forceRefresh,
           let token = storedToken(),
           let exp = jwtExpiry(token),
           exp.timeIntervalSinceNow > refreshWindow,
           let status = storedStatus() {
            return status
        }
        return try await register()
    }

    // MARK: - Registration

    private struct RegisterResponse: Decodable {
        let token: String
        let tier: String
        let trialExpiresAt: String?
        let trialRequestsRemaining: Int?
    }

    private struct ServerErrorBody: Decodable {
        struct Payload: Decodable {
            let code: String?
            let message: String?
        }
        let error: Payload
    }

    /// Attested registration on capable hardware (real devices); dev-mode
    /// fallback elsewhere. Attestation failures also fall back rather than
    /// stranding the student — the proxy is the enforcement point, and
    /// while it runs with ALLOW_UNATTESTED_REGISTRATION=1 the fallback
    /// still registers. Once that flag is off in production, unattestable
    /// clients simply can't register — which is the point.
    private static func register() async throws -> ProCoachStatus {
        if DCAppAttestService.shared.isSupported {
            do {
                return try await registerAttested()
            } catch let error as ProCoachError {
                // Server rejections (bad attestation, expired challenge…)
                // are real answers — don't paper over them with dev mode.
                throw error
            } catch {
                NSLog("[ProCoach] attestation unavailable (%@) — falling back to unattested registration",
                      error.localizedDescription)
            }
        }
        return try await submitRegistration(body: ["deviceId": deviceId])
    }

    /// Full App Attest flow: one-time challenge -> fresh Secure Enclave
    /// key -> Apple attestation over SHA256(challenge) -> register. A new
    /// key per registration keeps re-registration (JWT refresh, reinstall)
    /// simple; the server swaps the stored key material and keeps the
    /// device's tier/trial. A DeviceCheck token rides along when available
    /// so the server can run its reinstall-proof trial ledger.
    private static func registerAttested() async throws -> ProCoachStatus {
        let (challengeId, challenge) = try await fetchChallenge()
        let service = DCAppAttestService.shared
        let keyId = try await service.generateKey()
        let clientDataHash = Data(SHA256.hash(data: challenge))
        let attestation = try await service.attestKey(keyId, clientDataHash: clientDataHash)

        var body: [String: String] = [
            "deviceId": deviceId,
            "keyId": keyId,
            "attestation": attestation.base64EncodedString(),
            "challengeId": challengeId,
        ]
        if DCDevice.current.isSupported,
           let deviceToken = try? await DCDevice.current.generateToken() {
            body["deviceToken"] = deviceToken.base64EncodedString()
        }
        return try await submitRegistration(body: body)
    }

    private struct ChallengeResponse: Decodable {
        let challengeId: String
        let challenge: String
    }

    private static func fetchChallenge() async throws -> (id: String, bytes: Data) {
        guard let url = URL(string: baseURL + "/v1/register/challenge") else {
            throw ProCoachError.invalidResponse
        }
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.timeoutInterval = 15
        let data: Data
        do {
            (data, _) = try await URLSession.shared.data(for: request)
        } catch {
            throw ProCoachError.network(error.localizedDescription)
        }
        guard let decoded = try? JSONDecoder().decode(ChallengeResponse.self, from: data),
              let bytes = Data(base64Encoded: decoded.challenge)
        else { throw ProCoachError.invalidResponse }
        return (decoded.challengeId, bytes)
    }

    private static func submitRegistration(body: [String: String]) async throws -> ProCoachStatus {
        guard let url = URL(string: baseURL + "/v1/register") else {
            throw ProCoachError.invalidResponse
        }
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONEncoder().encode(body)
        request.timeoutInterval = 15

        let data: Data
        let response: URLResponse
        do {
            (data, response) = try await URLSession.shared.data(for: request)
        } catch {
            throw ProCoachError.network(error.localizedDescription)
        }

        guard let http = response as? HTTPURLResponse else {
            throw ProCoachError.invalidResponse
        }
        guard (200..<300).contains(http.statusCode) else {
            if let body = try? JSONDecoder().decode(ServerErrorBody.self, from: data) {
                throw ProCoachError.server(
                    code: body.error.code ?? "http_\(http.statusCode)",
                    message: body.error.message ?? "registration rejected")
            }
            throw ProCoachError.server(
                code: "http_\(http.statusCode)", message: "registration rejected")
        }
        guard let decoded = try? JSONDecoder().decode(RegisterResponse.self, from: data) else {
            throw ProCoachError.invalidResponse
        }

        KeychainStore.save(account: tokenAccount, value: decoded.token)
        let status = ProCoachStatus(
            tier: ProCoachStatus.Tier(rawValue: decoded.tier) ?? .free,
            trialExpiresAt: decoded.trialExpiresAt.flatMap(parseISODate),
            trialRequestsRemaining: decoded.trialRequestsRemaining)
        persist(status)
        return status
    }

    // MARK: - Entitlement claim (App Store subscription)

    /// Link a StoreKit 2 transaction to this device's record: POST the
    /// signed JWS to the proxy's claim endpoint with the device JWT. The
    /// server verifies the transaction, stores its originalTransactionId /
    /// appAccountToken against the device, and upgrades the tier. Rejections
    /// (409 subscription_not_active, 401 bad token/signature, 503 status
    /// outage) surface as `ProCoachError.server` for the store to branch on.
    /// A 200 body is ignored — register is the status authority, and the
    /// caller force-refreshes right after.
    static func claimEntitlement(signedTransaction: String) async throws {
        // The device JWT is the claim's auth — make sure one exists
        // (ensureRegistered is a no-op while the stored token is fresh).
        _ = try await ensureRegistered()
        guard let token = storedToken() else { throw ProCoachError.invalidResponse }
        guard let url = URL(string: baseURL + "/v1/entitlements/claim") else {
            throw ProCoachError.invalidResponse
        }
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        request.httpBody = try JSONEncoder().encode(
            ["signedTransaction": signedTransaction])
        request.timeoutInterval = 15

        let data: Data
        let response: URLResponse
        do {
            (data, response) = try await URLSession.shared.data(for: request)
        } catch {
            throw ProCoachError.network(error.localizedDescription)
        }
        guard let http = response as? HTTPURLResponse else {
            throw ProCoachError.invalidResponse
        }
        guard (200..<300).contains(http.statusCode) else {
            if let body = try? JSONDecoder().decode(ServerErrorBody.self, from: data) {
                throw ProCoachError.server(
                    code: body.error.code ?? "http_\(http.statusCode)",
                    message: body.error.message ?? "entitlement claim rejected")
            }
            throw ProCoachError.server(
                code: "http_\(http.statusCode)", message: "entitlement claim rejected")
        }
    }

    private static func persist(_ status: ProCoachStatus) {
        let defaults = UserDefaults.standard
        defaults.set(status.tier.rawValue, forKey: Keys.tier)
        if let date = status.trialExpiresAt {
            defaults.set(date.timeIntervalSince1970, forKey: Keys.trialExpiresAt)
        } else {
            defaults.removeObject(forKey: Keys.trialExpiresAt)
        }
        if let remaining = status.trialRequestsRemaining {
            defaults.set(remaining, forKey: Keys.trialRequestsRemaining)
        } else {
            defaults.removeObject(forKey: Keys.trialRequestsRemaining)
        }
    }

    // MARK: - JWT / date parsing

    /// `exp` claim of an HS256 JWT, read locally (base64url-decode the
    /// payload segment — no signature check needed, the proxy verifies).
    static func jwtExpiry(_ token: String) -> Date? {
        let segments = token.split(separator: ".")
        guard segments.count == 3 else { return nil }
        var base64 = segments[1]
            .replacingOccurrences(of: "-", with: "+")
            .replacingOccurrences(of: "_", with: "/")
        while base64.count % 4 != 0 { base64 += "=" }
        guard let data = Data(base64Encoded: base64),
              let payload = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let exp = payload["exp"] as? Double
        else { return nil }
        return Date(timeIntervalSince1970: exp)
    }

    /// The proxy sends ISO8601 with fractional seconds (JS `toISOString`).
    private static func parseISODate(_ string: String) -> Date? {
        let fractional = ISO8601DateFormatter()
        fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let date = fractional.date(from: string) { return date }
        let plain = ISO8601DateFormatter()
        plain.formatOptions = [.withInternetDateTime]
        return plain.date(from: string)
    }
}
