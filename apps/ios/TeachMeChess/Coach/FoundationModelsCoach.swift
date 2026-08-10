import Foundation
#if canImport(FoundationModels)
import FoundationModels
#endif

/// Best-effort `ForeignCoachModel` backed by Apple Foundation Models
/// (on-device, iOS 26+). Falls back to throwing when the OS or device
/// doesn't support it — callers should check `isAvailable` and use
/// `CannedCoachModel` otherwise.
///
/// The generated protocol's `complete` is synchronous (Rust calls it from a
/// blocking-friendly thread), so the async FM API is bridged with a
/// semaphore. Never call this from the main thread.
final class FoundationModelsCoach: ForeignCoachModel, @unchecked Sendable {
    /// Whether the on-device model can be used here (OS + device + model
    /// download state).
    static var isAvailable: Bool {
        #if canImport(FoundationModels)
        if #available(iOS 26.0, *) {
            return SystemLanguageModel.default.isAvailable
        }
        #endif
        return false
    }

    // MARK: - ForeignCoachModel

    func supportsTools() -> Bool { false }

    func contextTokens() -> UInt32 { 4096 }

    func compactTier() -> Bool { true }

    func complete(requestJson: String) throws -> String {
        #if canImport(FoundationModels)
        if #available(iOS 26.0, *) {
            struct WireMessage: Decodable { let role: String; let content: String }
            struct WireRequest: Decodable { let system: String?; let messages: [WireMessage]? }
            let request = try? JSONDecoder().decode(WireRequest.self, from: Data(requestJson.utf8))

            var promptParts: [String] = []
            if let system = request?.system, !system.isEmpty {
                promptParts.append(system)
            }
            for message in request?.messages ?? [] {
                promptParts.append("\(message.role): \(message.content)")
            }
            let prompt = promptParts.joined(separator: "\n\n")

            // Semaphore bridge: sync protocol method -> async FM API.
            let box = ResultBox()
            let semaphore = DispatchSemaphore(value: 0)
            Task.detached {
                do {
                    let session = LanguageModelSession()
                    let response = try await session.respond(to: prompt)
                    box.set(.success(response.content))
                } catch {
                    box.set(.failure(error))
                }
                semaphore.signal()
            }
            semaphore.wait()

            switch box.get() {
            case .success(let text):
                let data = try JSONSerialization.data(withJSONObject: ["text": text])
                guard let json = String(data: data, encoding: .utf8) else {
                    throw FfiError.Serialization(msg: "FM reply failed to encode")
                }
                return json
            case .failure(let error):
                throw FfiError.Llm(msg: "Foundation Models error: \(error.localizedDescription)")
            case nil:
                throw FfiError.Llm(msg: "Foundation Models returned no result")
            }
        }
        #endif
        throw FfiError.Llm(msg: "Foundation Models unavailable on this OS")
    }
}

/// Tiny lock-guarded box for passing the async result across the semaphore.
private final class ResultBox: @unchecked Sendable {
    private let lock = NSLock()
    private var value: Result<String, Error>?

    func set(_ newValue: Result<String, Error>) {
        lock.lock()
        value = newValue
        lock.unlock()
    }

    func get() -> Result<String, Error>? {
        lock.lock()
        defer { lock.unlock() }
        return value
    }
}
