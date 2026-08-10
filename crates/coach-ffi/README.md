# coach-ffi

The UniFFI bindings crate: the surface the iOS (SwiftUI) and Android
(Compose) apps talk to. Exposes `CoachSessionHandle` (a blocking wrapper
around `coach_core::coach::CoachSession`), the foreign-implementable
`ForeignCoachModel` trait, and the `speak_san` / `version` free functions.

v1 design choices (documented in more detail in `src/lib.rs`):

- **Blocking surface** — every method does `RUNTIME.block_on(...)` on a
  crate-global tokio runtime. Call from background threads only.
- **JSON payloads** — verdicts, summaries, profiles, and the model
  request/response cross the boundary as JSON strings. Typed
  `uniffi::Record` mirrors are the planned v2 upgrade.

## Regenerating the bindings

Bindings are generated *from the compiled library* (proc-macro metadata, no
UDL files) and land in `crates/coach-ffi/bindings/` (gitignored):

```sh
cargo build -p coach-ffi
cargo run -p coach-ffi --bin uniffi-bindgen -- generate \
    --library target/debug/libcoach_ffi.dylib \
    --language swift --language kotlin \
    --out-dir crates/coach-ffi/bindings
```

(On Linux the library is `libcoach_ffi.so`.) This produces:

- `bindings/coach_ffi.swift`, `bindings/coach_ffiFFI.h`,
  `bindings/coach_ffiFFI.modulemap` — the Swift side. For an Xcode project,
  compile `coach_ffi.swift` into your target and link the Rust static
  library (`staticlib` crate type, per-target `aarch64-apple-ios*` builds
  assembled into an XCFramework).
- `bindings/uniffi/coach_ffi/coach_ffi.kt` — the Kotlin side; ships next to
  a `cdylib` built per Android ABI, loaded with JNA.

## Implementing the model in Swift (Apple Foundation Models)

`ForeignCoachModel` is exported `with_foreign`, so Swift can *implement* the
LLM backend and Rust calls back into it — this is how the on-device,
Swift-only Apple Foundation Models API becomes a coach backend:

```swift
import FoundationModels

final class AppleFoundationCoachModel: ForeignCoachModel {
    private let session = LanguageModelSession()

    func supportsTools() -> Bool { false }   // v1: no tool calling on-device
    func contextTokens() -> UInt32 { 4096 }
    func compactTier() -> Bool { true }      // selects the compact prompt/toolset

    func complete(requestJson: String) throws -> String {
        // requestJson: {"system": "...", "messages": [{role, content, ...}], "tools": [...]}
        struct WireMessage: Decodable { let role: String; let content: String }
        struct WireRequest: Decodable { let system: String; let messages: [WireMessage] }
        let req = try JSONDecoder().decode(WireRequest.self,
                                           from: Data(requestJson.utf8))

        let prompt = req.system + "\n\n" +
            req.messages.map { "\($0.role): \($0.content)" }.joined(separator: "\n")

        // Rust invokes this on a blocking-friendly thread (spawn_blocking),
        // so bridging the async API synchronously here is acceptable.
        let text = try runBlocking { try await self.session.respond(to: prompt).content }

        // Reply: {"text": ..., "tool_calls": [...], "usage": {...}} — every
        // field optional; plain {"text": "..."} is a valid response.
        return String(data: try JSONSerialization.data(
            withJSONObject: ["text": text]), encoding: .utf8)!
    }
}

// Wire it into a session (from a background queue):
let session = try CoachSessionHandle.newWithForeignModel(
    model: AppleFoundationCoachModel(),
    enginePath: enginePath, fen: nil, profileJson: savedProfile, voice: true)
```

(`runBlocking` is a small semaphore-based async-to-sync bridge; any
equivalent works.) The same pattern applies on Android for Gemini Nano
(AICore) via the generated Kotlin `ForeignCoachModel` interface.

## Limitations

**iOS cannot spawn subprocesses.** `CoachSessionHandle` constructors spawn
the analyst engine (and `attach_opponent` the opponent engine) as child
processes over stdin/stdout UCI. That works on macOS, Android, and desktop
Linux/Windows — but iOS sandboxing forbids `fork`/`exec`, so this surface
does not yet run on an iPhone. The plan is an `EngineTransport` abstraction
in coach-core with an in-process, linked-library Stockfish build (calling
its UCI loop over an internal pipe) as a second transport; the FFI surface
here should not need to change shape when that lands. Deliberately not
solved in v1.

**Blocking calls, and threading expectations.** Every method blocks the
calling thread until the work (engine search, LLM round-trips, the full
tool loop) finishes — `judge_move` is typically a couple of seconds of
Stockfish time, `coach_reaction`/`chat` include network or on-device model
latency. Never call from the UI thread. The handle is thread-safe but
internally serialized by a mutex: concurrent calls queue up rather than
interleave. A `ForeignCoachModel` implementation is invoked while that lock
is held, so it must not call back into the same `CoachSessionHandle`
(deadlock); it may block, and it must be safe to call from arbitrary
threads.

**JSON boundary.** Payload schemas are the serde serializations of
coach-core's `MoveVerdict`, `GameSummary`, `StudentProfile`,
`SessionStats`, and the `{system, messages, tools}` / `{text, tool_calls,
usage}` completion wire forms. They are stable-ish but unversioned in v1;
tolerate unknown fields when decoding on the platform side.
