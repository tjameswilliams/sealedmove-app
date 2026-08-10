# stockfish-embedded

Stockfish 11 compiled straight into the binary and exposed as a
`coach_core::engine::ChannelTransport`. This is the engine path for
platforms that cannot spawn subprocesses — iOS forbids it — where the
desktop `ProcessTransport` (spawn a `stockfish` binary) does not exist.

```rust
let transport = stockfish_embedded::embedded_stockfish_transport().await?;
let engine = coach_core::engine::UciEngine::from_transport(Box::new(transport)).await?;
```

How it works: `build.rs` compiles the vendored Stockfish 11 sources
(`vendor/stockfish/`, minus `main.cpp`) plus `src/shim.cpp`. The shim
redirects the global `std::cin`/`std::cout` stream buffers to
mutex+condvar line queues, then runs Stockfish's normal `main()` init
sequence and `UCI::loop` on a dedicated thread. Two bridge threads shuttle
lines between those queues and the `ChannelTransport`'s channels, so the
UCI protocol layer never knows the engine is in-process.

Why Stockfish **11**: it is the last classical-evaluation release — no NNUE
network file (SF16+ requires a ~75MB net), roughly 2MB of code, and still
far stronger than a coaching analyst needs.

## Singleton constraint

**Exactly one embedded instance per process.** The shim takes over the
global `std::cin`/`std::cout` rdbufs, which can only be done once; a second
call to `embedded_stockfish_transport()` returns an
`io::ErrorKind::AlreadyExists` error. This also means there is no second
in-process engine to use as an opponent — on iOS, use
`CoachSession::set_opponent_analyst` (opponent-via-analyst with a reduced
`Skill Level`) until Maia is linked in-process.

Dropping the transport sends `quit` and joins the engine thread, but the
singleton guard stays latched: the instance cannot be restarted within the
same process.

## License

**This crate is GPL-3.0-only, not the workspace's MIT.** Stockfish is
licensed under the GNU General Public License v3 (the vendored full text is
at `vendor/stockfish/Copying.txt`). Statically linking this crate makes the
combined binary a GPL work.

A product-level decision is required **before public distribution** of any
app or binary that links this crate: either comply with the GPLv3 for the
whole combined work (offer corresponding source, etc.), or swap this engine
out for one under a compatible-with-proprietary license. Keeping this crate
a leaf dependency — nothing in `coach-core` depends on it — is what keeps
that swap cheap; the GPL boundary is exactly the crates that opt into
depending on `stockfish-embedded` (currently `coach-ffi` via the default
`embedded-stockfish` feature, and `coach-cli`).
