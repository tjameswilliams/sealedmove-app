//! Stockfish 11, statically linked into the process, exposed as a
//! [`ChannelTransport`] for [`coach_core::engine::UciEngine`].
//!
//! iOS forbids spawning subprocesses, so the desktop `ProcessTransport` path
//! (spawn a `stockfish` binary) does not exist there. This crate compiles
//! the vendored Stockfish 11 sources into the binary (see `build.rs` and
//! `src/shim.cpp`), runs the engine's normal UCI loop on a dedicated thread
//! with its stdin/stdout redirected to line queues, and bridges those queues
//! to a [`ChannelTransport`] — so the protocol layer is oblivious to where
//! the engine lives.
//!
//! Stockfish **11** deliberately: the last classical-eval release — no NNUE
//! network file (SF16+ needs a ~75MB net), ~2MB of code, and still far
//! stronger than a coaching analyst needs.
//!
//! # Singleton
//!
//! Exactly ONE embedded instance per process: the shim redirects the global
//! `std::cin`/`std::cout` stream buffers, which cannot be done twice. A
//! second [`embedded_stockfish_transport`] call returns an error.
//!
//! # License
//!
//! Stockfish is GPLv3. Linking this crate makes the combined binary a GPL
//! work — see this crate's README.md before distributing anything built
//! with it.

use coach_core::engine::ChannelTransport;
use std::ffi::CString;
use std::os::raw::c_char;

mod ffi {
    use std::os::raw::c_char;

    extern "C" {
        pub fn sf_start() -> i32;
        pub fn sf_send(line: *const c_char);
        pub fn sf_recv(buf: *mut c_char, cap: i32, timeout_ms: i32) -> i32;
        pub fn sf_stop();
    }
}

/// Buffer for one engine output line. UCI `info … pv …` lines at high depth
/// run long, but nowhere near 4KB.
const RECV_BUF_BYTES: usize = 4096;
/// How long each `sf_recv` blocks before re-checking channel liveness.
const RECV_TIMEOUT_MS: i32 = 200;
/// mpsc buffer of the returned [`ChannelTransport`].
const CHANNEL_BUFFER: usize = 64;

/// Start the embedded Stockfish 11 and return a [`ChannelTransport`] wired
/// to it, ready for `UciEngine::from_transport`.
///
/// Two bridge threads shuttle lines between the transport's channels and the
/// engine's C queues. When the transport (and thus its command sender) is
/// dropped, the engine is sent `quit` and its thread is joined; both bridge
/// threads then exit.
///
/// # Errors
///
/// Returns an error if an embedded instance was already started in this
/// process — the engine is a per-process singleton (global stdio
/// redirection), so there is no second instance, ever.
pub async fn embedded_stockfish_transport() -> std::io::Result<ChannelTransport> {
    // SAFETY: sf_start takes no arguments and is internally guarded against
    // double-start; it only spawns the engine thread and returns.
    if unsafe { ffi::sf_start() } != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "embedded Stockfish is already running: only one instance per process \
             (the engine takes over the global stdio buffers)",
        ));
    }

    let (transport, engine_out, mut engine_in) = ChannelTransport::new(CHANNEL_BUFFER);

    // Command bridge: protocol layer -> engine stdin queue. When the
    // transport drops its sender, recv yields None and we shut the engine
    // down (sf_stop is idempotent).
    std::thread::Builder::new()
        .name("sf-stdin".into())
        .spawn(move || {
            while let Some(line) = engine_in.blocking_recv() {
                // Interior NULs cannot occur in UCI commands; drop the line
                // rather than truncate it if one ever does.
                if let Ok(cline) = CString::new(line) {
                    // SAFETY: valid NUL-terminated pointer for the call's
                    // duration; the shim copies the bytes before returning.
                    unsafe { ffi::sf_send(cline.as_ptr()) };
                }
            }
            // SAFETY: no arguments; idempotent shutdown.
            unsafe { ffi::sf_stop() };
        })
        .expect("spawn sf-stdin bridge thread");

    // Output bridge: engine stdout queue -> protocol layer. Exits on
    // engine-stopped (-1) or when the transport side hung up.
    std::thread::Builder::new()
        .name("sf-stdout".into())
        .spawn(move || {
            let mut buf = [0u8; RECV_BUF_BYTES];
            loop {
                // SAFETY: buf outlives the call; the shim writes at most
                // cap-1 bytes plus a NUL and returns the length written.
                let n = unsafe {
                    ffi::sf_recv(buf.as_mut_ptr() as *mut c_char, RECV_BUF_BYTES as i32, RECV_TIMEOUT_MS)
                };
                match n {
                    // Engine stopped and output drained.
                    n if n < 0 => break,
                    // Timeout: loop again (also lets blocking_send below
                    // detect a dropped receiver reasonably promptly).
                    0 => continue,
                    n => {
                        let line = String::from_utf8_lossy(&buf[..n as usize]).into_owned();
                        if engine_out.blocking_send(line).is_err() {
                            break; // transport dropped; stdin bridge handles sf_stop
                        }
                    }
                }
            }
        })
        .expect("spawn sf-stdout bridge thread");

    Ok(transport)
}

#[cfg(test)]
mod tests {
    use super::*;
    use coach_core::engine::UciEngine;
    use std::time::{Duration, Instant};
    use tokio::time::timeout;

    const TEST_TIMEOUT: Duration = Duration::from_secs(60);
    const START_FEN: &str = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";

    /// One test fn on purpose: the embedded engine is a per-process
    /// singleton, so handshake, analyze, best_move_nodes, and the
    /// second-instance error must all run against the same instance.
    #[tokio::test]
    async fn embedded_engine_end_to_end() {
        let transport = embedded_stockfish_transport()
            .await
            .expect("first embedded instance starts");
        let mut engine = timeout(TEST_TIMEOUT, UciEngine::from_transport(Box::new(transport)))
            .await
            .expect("handshake timed out")
            .expect("handshake failed");

        // MultiPV analysis of the starting position.
        let started = Instant::now();
        let analysis = timeout(TEST_TIMEOUT, engine.analyze(START_FEN, 10, 2))
            .await
            .expect("analyze timed out")
            .expect("analyze failed");
        println!(
            "embedded analyze(depth 10, multipv 2) took {:?}",
            started.elapsed()
        );
        assert!(!analysis.best_move.is_empty(), "bestmove must be nonempty");
        assert!(
            !analysis.lines.is_empty(),
            "analyze must return at least one scored line"
        );

        // Node-bounded best move.
        let mv = timeout(TEST_TIMEOUT, engine.best_move_nodes(START_FEN, 10_000))
            .await
            .expect("best_move_nodes timed out")
            .expect("best_move_nodes failed");
        assert!(!mv.is_empty(), "node-bounded bestmove must be nonempty");

        // Singleton: a second embedded instance must refuse to start.
        match embedded_stockfish_transport().await {
            Ok(_) => panic!("second embedded instance must fail"),
            Err(err) => assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists),
        }
    }
}
