import Foundation
import Security

/// Which coach answers: the free on-device tier or the managed Pro Coach
/// service. The embedded engine is a per-process singleton, so switching
/// swaps the backend on the LIVE session (`setBackend*` FFI methods)
/// instead of building a new one.
///
/// Earlier builds offered BYO-key providers (DeepSeek/Custom/Anthropic);
/// those persisted rawValues no longer parse, so such installs silently
/// land back on `.onDevice`. Their orphaned Keychain keys are left alone.
enum CoachProvider: String, CaseIterable, Identifiable {
    case onDevice
    case proCoach

    var id: String { rawValue }

    var displayName: String {
        switch self {
        case .onDevice: return "On-device"
        case .proCoach: return "Pro Coach"
        }
    }
}

/// How much the coach talks — the app-side mirror of the Rust core's
/// `CommentaryStyle`, persisted in UserDefaults and applied to the live
/// session via `setCommentaryStyle` on launch and on change.
enum CoachChattiness: String, CaseIterable, Identifiable {
    case quiet
    case balanced
    case chatty

    var id: String { rawValue }

    var displayName: String {
        switch self {
        case .quiet: return "Quiet"
        case .balanced: return "Balanced"
        case .chatty: return "Chatty"
        }
    }

    /// One-line description for the settings footer.
    var summary: String {
        switch self {
        case .quiet:
            return "The coach only speaks up about inaccuracies, mistakes, and blunders."
        case .balanced:
            return "Reactions to notable moves and milestones, periodic praise, and warnings on real threats."
        case .chatty:
            return "A full reaction to every move, plus threat warnings, phase notes, and game summaries."
        }
    }

    var ffiStyle: FfiCommentaryStyle {
        switch self {
        case .quiet: return .quiet
        case .balanced: return .balanced
        case .chatty: return .chatty
        }
    }

    private static let key = "coach.commentaryStyle"

    /// Default is Chatty — the product direction: an engaged, talkative
    /// coach out of the box.
    static func load() -> CoachChattiness {
        guard let raw = UserDefaults.standard.string(forKey: key),
              let style = CoachChattiness(rawValue: raw)
        else { return .chatty }
        return style
    }

    func save() {
        UserDefaults.standard.set(rawValue, forKey: Self.key)
    }
}

/// Non-secret backend settings, persisted in UserDefaults.
struct BackendSettings: Equatable {
    var provider: CoachProvider = .onDevice

    /// The backend part of the status line. Deliberately model-free: the
    /// Pro Coach's underlying model is a server-side implementation detail
    /// the proxy can swap at any time, so the app never names it.
    func backendLabel(onDeviceName: String) -> String {
        switch provider {
        case .onDevice: return onDeviceName
        case .proCoach: return "Pro Coach"
        }
    }

    private enum Keys {
        static let provider = "coach.provider"
    }

    static func load() -> BackendSettings {
        let defaults = UserDefaults.standard
        var settings = BackendSettings()
        if let raw = defaults.string(forKey: Keys.provider),
           let provider = CoachProvider(rawValue: raw) {
            settings.provider = provider
        }
        return settings
    }

    func save() {
        UserDefaults.standard.set(provider.rawValue, forKey: Keys.provider)
    }
}

/// Secret storage in the iOS Keychain (service "dev.teachmechess.app", one
/// generic-password item per account): the Pro Coach device id and JWT —
/// see `ProCoachAccount`. Secrets never touch UserDefaults.
enum KeychainStore {
    static let service = "dev.teachmechess.app"

    private static func baseQuery(account: String) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
    }

    static func save(account: String, value: String) {
        delete(account: account)
        guard !value.isEmpty else { return }
        var query = baseQuery(account: account)
        query[kSecValueData as String] = Data(value.utf8)
        query[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlock
        let status = SecItemAdd(query as CFDictionary, nil)
        if status != errSecSuccess {
            NSLog("[Keychain] SecItemAdd failed for %@: %d", account, status)
        }
    }

    static func load(account: String) -> String? {
        var query = baseQuery(account: account)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var item: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &item) == errSecSuccess,
              let data = item as? Data,
              let value = String(data: data, encoding: .utf8),
              !value.isEmpty
        else { return nil }
        return value
    }

    static func delete(account: String) {
        SecItemDelete(baseQuery(account: account) as CFDictionary)
    }
}
