//! UCI protocol layer, decoupled from transport.
//!
//! [`UciEngine`] speaks the UCI protocol (handshake, `go`/`info`/`bestmove`,
//! options) over any [`UciTransport`] — it does not care where the lines come
//! from. Two transports ship today:
//!
//! - [`ProcessTransport`] — a spawned engine binary (Stockfish, lc0/Maia, …)
//!   with piped stdin/stdout. This is the desktop/server path.
//! - [`ChannelTransport`] — a pair of in-process tokio mpsc channels. This is
//!   the path for platforms that cannot spawn subprocesses (iOS forbids it):
//!   a **statically-linked engine** (e.g. Stockfish compiled into the app)
//!   runs its normal `main`-style UCI loop on a dedicated thread, with its
//!   stdin reads replaced by `engine_in.blocking_recv()` and its stdout
//!   writes replaced by `engine_out.blocking_send(line)`. The protocol side
//!   is oblivious — [`UciEngine::from_transport`] runs the exact same
//!   handshake and search commands over the channels. The same wiring gives
//!   tests a scripted fake engine on a tokio task.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::mpsc;

/// Line-oriented, bidirectional link to a UCI engine.
///
/// The protocol layer ([`UciEngine`]) is written against this trait only, so
/// the engine can live in a child process, a linked library on its own
/// thread, or a test task — anywhere lines can be shuttled to and from.
#[async_trait]
pub trait UciTransport: Send {
    /// Send one line (no trailing newline) to the engine.
    async fn send_line(&mut self, line: &str) -> std::io::Result<()>;
    /// Receive the next line from the engine; Ok(None) = closed.
    async fn recv_line(&mut self) -> std::io::Result<Option<String>>;
}

/// [`UciTransport`] over a spawned engine process with piped stdin/stdout.
///
/// Stderr is discarded and the child is killed when the transport drops.
pub struct ProcessTransport {
    _child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
}

impl ProcessTransport {
    /// Spawn the engine binary. `args` covers engines that need launch-time
    /// flags — lc0/Maia takes `--weights=<file>`.
    pub fn spawn(path: &str, args: &[String]) -> std::io::Result<Self> {
        let mut child = Command::new(path)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let stdin = child.stdin.take().expect("engine stdin piped");
        let stdout = child.stdout.take().expect("engine stdout piped");
        Ok(Self {
            _child: child,
            stdin,
            lines: BufReader::new(stdout).lines(),
        })
    }
}

#[async_trait]
impl UciTransport for ProcessTransport {
    async fn send_line(&mut self, line: &str) -> std::io::Result<()> {
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.write_all(b"\n").await?;
        self.stdin.flush().await
    }

    async fn recv_line(&mut self) -> std::io::Result<Option<String>> {
        self.lines.next_line().await
    }
}

/// [`UciTransport`] over in-process tokio mpsc channels.
///
/// [`ChannelTransport::new`] returns the transport plus the far ends of both
/// channels; whatever plays the engine (a linked Stockfish on its own thread,
/// a scripted fake on a tokio task) holds those far ends:
///
/// - it *reads commands* from the returned `Receiver<String>` (its "stdin"),
/// - it *writes output lines* to the returned `Sender<String>` (its "stdout").
///
/// Dropping either far end reads as the engine dying: `send_line` fails with
/// `BrokenPipe`, `recv_line` returns `Ok(None)` once buffered lines drain.
pub struct ChannelTransport {
    /// Protocol → engine command lines (the engine's stdin).
    to_engine: mpsc::Sender<String>,
    /// Engine → protocol output lines (the engine's stdout).
    from_engine: mpsc::Receiver<String>,
}

impl ChannelTransport {
    /// Create a transport and the engine-side channel ends.
    ///
    /// Returns `(transport, engine_out, engine_in)`:
    /// - `engine_out`: the engine sends its output lines here ("stdout"),
    /// - `engine_in`: the engine receives command lines here ("stdin").
    pub fn new(buffer: usize) -> (Self, mpsc::Sender<String>, mpsc::Receiver<String>) {
        let (engine_out, from_engine) = mpsc::channel(buffer);
        let (to_engine, engine_in) = mpsc::channel(buffer);
        (
            Self {
                to_engine,
                from_engine,
            },
            engine_out,
            engine_in,
        )
    }
}

#[async_trait]
impl UciTransport for ChannelTransport {
    async fn send_line(&mut self, line: &str) -> std::io::Result<()> {
        self.to_engine.send(line.to_string()).await.map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "engine channel closed")
        })
    }

    async fn recv_line(&mut self) -> std::io::Result<Option<String>> {
        Ok(self.from_engine.recv().await)
    }
}

/// Evaluation from the side-to-move's perspective.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Score {
    /// Centipawns.
    Cp(i32),
    /// Mate in N moves (negative = getting mated).
    Mate(i32),
}

impl Score {
    /// Collapse to centipawns for arithmetic; mates clamp to ±10_000.
    pub fn as_cp(&self) -> i32 {
        match *self {
            Score::Cp(cp) => cp,
            Score::Mate(n) if n > 0 => 10_000 - n.min(100),
            Score::Mate(n) => -10_000 - n.max(-100),
        }
    }
}

/// One principal variation from a MultiPV search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoredLine {
    pub multipv: u32,
    pub depth: u32,
    pub score: Score,
    /// Moves in UCI notation (e2e4, g1f3, …).
    pub pv: Vec<String>,
}

/// Result of one `go` command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Analysis {
    /// Engine's chosen move, UCI notation.
    pub best_move: String,
    /// Top lines, best first. Length ≤ requested MultiPV.
    pub lines: Vec<ScoredLine>,
}

/// UCI protocol driver over any [`UciTransport`].
pub struct UciEngine {
    transport: Box<dyn UciTransport>,
}

impl UciEngine {
    /// Spawn the engine binary and complete the UCI handshake.
    pub async fn spawn(path: &str) -> std::io::Result<Self> {
        Self::spawn_with_args(path, &[]).await
    }

    /// Spawn with extra command-line arguments — lc0/Maia needs
    /// `--weights=<file>` at launch.
    pub async fn spawn_with_args(path: &str, args: &[String]) -> std::io::Result<Self> {
        Self::from_transport(Box::new(ProcessTransport::spawn(path, args)?)).await
    }

    /// Take an already-connected transport and complete the UCI handshake.
    ///
    /// This is the entry point for in-process engines: wire a
    /// [`ChannelTransport`] to the engine's loop, then hand it here.
    pub async fn from_transport(transport: Box<dyn UciTransport>) -> std::io::Result<Self> {
        let mut engine = Self { transport };
        engine.send("uci").await?;
        engine.wait_for("uciok").await?;
        engine.send("isready").await?;
        engine.wait_for("readyok").await?;
        Ok(engine)
    }

    async fn send(&mut self, cmd: &str) -> std::io::Result<()> {
        self.transport.send_line(cmd).await
    }

    async fn wait_for(&mut self, prefix: &str) -> std::io::Result<()> {
        while let Some(line) = self.transport.recv_line().await? {
            if line.starts_with(prefix) {
                return Ok(());
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            format!("engine closed stdout before '{prefix}'"),
        ))
    }

    pub async fn set_option(&mut self, name: &str, value: &str) -> std::io::Result<()> {
        self.send(&format!("setoption name {name} value {value}"))
            .await
    }

    /// Analyze a position: MultiPV search to a fixed depth.
    pub async fn analyze(
        &mut self,
        fen: &str,
        depth: u32,
        multipv: u32,
    ) -> std::io::Result<Analysis> {
        self.set_option("MultiPV", &multipv.to_string()).await?;
        self.send(&format!("position fen {fen}")).await?;
        self.send(&format!("go depth {depth}")).await?;

        // Keep only the deepest report per multipv slot.
        let mut best_lines: BTreeMap<u32, ScoredLine> = BTreeMap::new();
        let mut best_move = String::new();
        while let Some(line) = self.transport.recv_line().await? {
            if let Some(rest) = line.strip_prefix("bestmove") {
                best_move = rest.split_whitespace().next().unwrap_or("").to_string();
                break;
            }
            if let Some(scored) = parse_info_line(&line) {
                best_lines.insert(scored.multipv, scored);
            }
        }
        Ok(Analysis {
            best_move,
            lines: best_lines.into_values().collect(),
        })
    }

    /// Ask the engine for a move to *play*, bounded by wall-clock time.
    pub async fn best_move(&mut self, fen: &str, movetime_ms: u32) -> std::io::Result<String> {
        self.send(&format!("position fen {fen}")).await?;
        self.send(&format!("go movetime {movetime_ms}")).await?;
        self.read_bestmove().await
    }

    /// Ask the engine for a move bounded by node count. `nodes 1` is the
    /// canonical way to run Maia — the raw policy network's move, which is
    /// what makes it play like a human at its trained rating band.
    pub async fn best_move_nodes(&mut self, fen: &str, nodes: u64) -> std::io::Result<String> {
        self.send(&format!("position fen {fen}")).await?;
        self.send(&format!("go nodes {nodes}")).await?;
        self.read_bestmove().await
    }

    async fn read_bestmove(&mut self) -> std::io::Result<String> {
        while let Some(line) = self.transport.recv_line().await? {
            if let Some(rest) = line.strip_prefix("bestmove") {
                return Ok(rest.split_whitespace().next().unwrap_or("").to_string());
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "engine closed stdout before bestmove",
        ))
    }
}

/// Parse one `info … multipv … score … pv …` line. Returns `None` for lines
/// that carry no complete scored PV (e.g. `info string`, currmove reports).
pub fn parse_info_line(line: &str) -> Option<ScoredLine> {
    if !line.starts_with("info ") {
        return None;
    }
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let mut depth: Option<u32> = None;
    let mut multipv: u32 = 1;
    let mut score: Option<Score> = None;
    let mut pv: Vec<String> = Vec::new();

    let mut i = 0;
    while i < tokens.len() {
        match tokens[i] {
            "depth" => {
                depth = tokens.get(i + 1)?.parse().ok();
                i += 2;
            }
            "multipv" => {
                multipv = tokens.get(i + 1)?.parse().ok()?;
                i += 2;
            }
            "score" => {
                score = match *tokens.get(i + 1)? {
                    "cp" => Some(Score::Cp(tokens.get(i + 2)?.parse().ok()?)),
                    "mate" => Some(Score::Mate(tokens.get(i + 2)?.parse().ok()?)),
                    _ => None,
                };
                i += 3;
            }
            "pv" => {
                pv = tokens[i + 1..].iter().map(|s| s.to_string()).collect();
                break;
            }
            _ => i += 1,
        }
    }

    if pv.is_empty() {
        return None;
    }
    Some(ScoredLine {
        multipv,
        depth: depth?,
        score: score?,
        pv,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multipv_cp_line() {
        let line = "info depth 20 seldepth 28 multipv 2 score cp 34 nodes 1500000 nps 900000 time 1667 pv e2e4 e7e5 g1f3";
        let scored = parse_info_line(line).unwrap();
        assert_eq!(scored.depth, 20);
        assert_eq!(scored.multipv, 2);
        assert_eq!(scored.score, Score::Cp(34));
        assert_eq!(scored.pv, vec!["e2e4", "e7e5", "g1f3"]);
    }

    #[test]
    fn parses_mate_score() {
        let line = "info depth 12 multipv 1 score mate 3 pv d1h5 g6h5 f3g5";
        let scored = parse_info_line(line).unwrap();
        assert_eq!(scored.score, Score::Mate(3));
        assert_eq!(scored.score.as_cp(), 9_997);
    }

    #[test]
    fn ignores_non_pv_lines() {
        assert!(parse_info_line("info string NNUE evaluation using nn.nnue").is_none());
        assert!(parse_info_line("info depth 5 currmove e2e4 currmovenumber 1").is_none());
        assert!(parse_info_line("readyok").is_none());
    }
}

#[cfg(test)]
mod transport_tests {
    use super::*;
    use std::time::Duration;
    use tokio::time::timeout;

    const TEST_TIMEOUT: Duration = Duration::from_secs(5);
    const START_FEN: &str = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";

    /// Scripted fake engine: reads command lines off `engine_in`, replies with
    /// canned UCI output on `engine_out` — exactly how a statically-linked
    /// engine would bridge, just with a script instead of a search.
    ///
    /// `die_after_go` simulates the engine crashing mid-search: on the first
    /// `go` command the task drops both channel ends without answering.
    fn spawn_fake_engine(die_after_go: bool) -> ChannelTransport {
        let (transport, engine_out, mut engine_in) = ChannelTransport::new(64);
        tokio::spawn(async move {
            while let Some(cmd) = engine_in.recv().await {
                let first = cmd.split_whitespace().next().unwrap_or("");
                match first {
                    "uci" => {
                        engine_out
                            .send("id name FakeFish 1.0".to_string())
                            .await
                            .ok();
                        engine_out
                            .send("option name MultiPV type spin default 1 min 1 max 500".to_string())
                            .await
                            .ok();
                        engine_out.send("uciok".to_string()).await.ok();
                    }
                    "isready" => {
                        engine_out.send("readyok".to_string()).await.ok();
                    }
                    "setoption" | "position" => {}
                    "go" => {
                        if die_after_go {
                            // Engine crashes mid-search: both ends drop.
                            return;
                        }
                        if cmd.contains("nodes") {
                            engine_out
                                .send("info depth 1 score cp 12 nodes 1 pv d2d4".to_string())
                                .await
                                .ok();
                            engine_out.send("bestmove d2d4 ponder g8f6".to_string()).await.ok();
                        } else {
                            // MultiPV search: shallow reports first, then
                            // deeper ones for the same slots, then bestmove.
                            for line in [
                                "info string starting search",
                                "info depth 5 multipv 1 score cp 20 pv e2e4 e7e5",
                                "info depth 5 multipv 2 score cp 10 pv d2d4 d7d5",
                                "info depth 10 currmove e2e4 currmovenumber 1",
                                "info depth 12 multipv 1 score cp 31 pv e2e4 c7c5 g1f3",
                                "info depth 12 multipv 2 score mate 4 pv d2d4 g8f6 c2c4",
                                "bestmove e2e4 ponder c7c5",
                            ] {
                                engine_out.send(line.to_string()).await.ok();
                            }
                        }
                    }
                    "quit" => return,
                    _ => {}
                }
            }
        });
        transport
    }

    #[tokio::test]
    async fn handshake_over_channel_transport() {
        let transport = spawn_fake_engine(false);
        let engine = timeout(TEST_TIMEOUT, UciEngine::from_transport(Box::new(transport)))
            .await
            .expect("handshake timed out");
        assert!(engine.is_ok(), "handshake failed: {:?}", engine.err());
    }

    #[tokio::test]
    async fn analyze_multipv_round_trip() {
        let transport = spawn_fake_engine(false);
        let mut engine = timeout(TEST_TIMEOUT, UciEngine::from_transport(Box::new(transport)))
            .await
            .expect("handshake timed out")
            .unwrap();

        let analysis = timeout(TEST_TIMEOUT, engine.analyze(START_FEN, 12, 2))
            .await
            .expect("analyze timed out")
            .unwrap();

        assert_eq!(analysis.best_move, "e2e4");
        assert_eq!(analysis.lines.len(), 2);
        // Deepest report per multipv slot wins over the depth-5 ones.
        assert_eq!(analysis.lines[0].multipv, 1);
        assert_eq!(analysis.lines[0].depth, 12);
        assert_eq!(analysis.lines[0].score, Score::Cp(31));
        assert_eq!(analysis.lines[0].pv, vec!["e2e4", "c7c5", "g1f3"]);
        assert_eq!(analysis.lines[1].multipv, 2);
        assert_eq!(analysis.lines[1].depth, 12);
        assert_eq!(analysis.lines[1].score, Score::Mate(4));
    }

    #[tokio::test]
    async fn best_move_nodes_round_trip() {
        let transport = spawn_fake_engine(false);
        let mut engine = timeout(TEST_TIMEOUT, UciEngine::from_transport(Box::new(transport)))
            .await
            .expect("handshake timed out")
            .unwrap();

        let mv = timeout(TEST_TIMEOUT, engine.best_move_nodes(START_FEN, 1))
            .await
            .expect("best_move_nodes timed out")
            .unwrap();
        assert_eq!(mv, "d2d4");
    }

    #[tokio::test]
    async fn engine_dies_mid_search_is_clean_error() {
        let transport = spawn_fake_engine(true);
        let mut engine = timeout(TEST_TIMEOUT, UciEngine::from_transport(Box::new(transport)))
            .await
            .expect("handshake timed out")
            .unwrap();

        let result = timeout(TEST_TIMEOUT, engine.best_move(START_FEN, 100))
            .await
            .expect("dead engine must error, not hang");
        let err = result.expect_err("dead engine must yield an error");
        assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
    }
}
