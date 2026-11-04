//! The engine driven as a UCI process, as a GUI drives it.

use chess::board::Board;
use chess::polyglot_zobrist::calculate_key;
use chess::types::{MoveList, START_FEN};
use chess::uci_io::format_uci;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

const NET_LOAD_TIMEOUT: Duration = Duration::from_secs(60);
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

/// A UCI engine process driven through stdin, with stdout read on a background thread.
struct Engine {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    seen: Vec<String>,
}

impl Engine {
    fn start() -> Self {
        Self::start_binary(
            Path::new(env!("CARGO_BIN_EXE_chess")),
            Path::new(env!("CARGO_MANIFEST_DIR")),
        )
    }

    fn start_binary(exe: &Path, cwd: &Path) -> Self {
        let mut child = Command::new(exe)
            .arg("uci")
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("engine starts");
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            stdin,
            lines,
            seen: Vec::new(),
        }
    }

    fn send(&mut self, cmd: &str) {
        let _ = writeln!(self.stdin, "{cmd}");
        let _ = self.stdin.flush();
    }

    /// Lines up to and including the first that satisfies `done`, or `Err` with what
    /// arrived if the timeout passes or the engine exits first.
    fn read_until(
        &mut self,
        done: impl Fn(&str) -> bool,
        timeout: Duration,
    ) -> Result<Vec<String>, Vec<String>> {
        let deadline = Instant::now() + timeout;
        let mut got = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(line) => {
                    self.seen.push(line.clone());
                    let finished = done(&line);
                    got.push(line);
                    if finished {
                        return Ok(got);
                    }
                }
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => return Err(got),
            }
        }
    }

    fn handshake(&mut self) {
        self.send("uci");
        self.read_until(|l| l == "uciok", NET_LOAD_TIMEOUT)
            .expect("uciok");
        self.send("isready");
        self.read_until(|l| l == "readyok", NET_LOAD_TIMEOUT)
            .expect("readyok");
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The FEN the engine's `eval` reports for its current position.
fn current_fen(engine: &mut Engine) -> String {
    engine.send("eval");
    let lines = engine
        .read_until(|l| l.starts_with("info string fen:"), REPLY_TIMEOUT)
        .expect("eval output");
    lines
        .last()
        .expect("fen line")
        .trim_start_matches("info string fen: ")
        .to_string()
}

const KINGS_AND_ROOK: &str = "4k3/8/8/8/8/8/8/R3K3 w Q - 0 1";

#[test]
fn an_invalid_position_fen_is_reported() {
    let mut engine = Engine::start();
    engine.handshake();

    engine.send("position fen 8/8/8/8/8/8/8/8 w - - 0 1");
    engine.send("isready");
    let replies = engine
        .read_until(|l| l == "readyok", REPLY_TIMEOUT)
        .expect("readyok");

    assert!(
        replies.iter().any(|l| l.starts_with("info string")),
        "replies {replies:?}"
    );
}

#[test]
fn an_invalid_position_fen_keeps_the_position_before_it() {
    let mut engine = Engine::start();
    engine.handshake();
    engine.send(&format!("position fen {KINGS_AND_ROOK}"));

    engine.send("position fen 8/8/8/8/8/8/8/8 w - - 0 1 moves e2e4");
    let fen = current_fen(&mut engine);

    assert_eq!(fen, KINGS_AND_ROOK);
}

fn is_bestmove(line: &str) -> bool {
    line.starts_with("bestmove")
}

fn bestmove_of(lines: &[String]) -> String {
    let line = lines
        .iter()
        .rev()
        .find(|l| is_bestmove(l))
        .expect("a bestmove line");
    line.split_whitespace()
        .nth(1)
        .expect("bestmove has a move")
        .to_string()
}

fn legal_uci_moves(fen: &str) -> Vec<String> {
    let mut moves = MoveList::new();
    Board::from_fen(fen)
        .expect("valid FEN")
        .generate_legal_moves(&mut moves);
    moves.as_slice().iter().map(|&m| format_uci(m)).collect()
}

/// The engine run from a directory whose `moves/book.bin` is a Polyglot
/// book of `entries` (key, raw move). Running a copy of the binary instead
/// races other tests' spawns into ETXTBSY.
fn engine_with_book(name: &str, entries: &mut [(u64, u16)]) -> Engine {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    std::fs::create_dir_all(dir.join("moves")).expect("book dir");
    entries.sort_unstable();
    let mut bytes = Vec::new();
    for &(key, raw_move) in entries.iter() {
        bytes.extend_from_slice(&key.to_be_bytes());
        bytes.extend_from_slice(&raw_move.to_be_bytes());
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
    }
    std::fs::write(dir.join("moves/book.bin"), bytes).expect("write book");
    Engine::start_binary(Path::new(env!("CARGO_BIN_EXE_chess")), &dir)
}

const fn polyglot_move(from: u16, to: u16) -> u16 {
    (from << 6) | to
}

fn key_of(fen: &str) -> u64 {
    calculate_key(&Board::from_fen(fen).expect("valid FEN"))
}

#[test]
fn a_book_rook_move_from_e1_to_h1_is_not_turned_into_castling() {
    let fen = "7k/8/8/8/8/8/K7/4R3 w - - 0 1";
    let mut engine = engine_with_book("rook_e1h1", &mut [(key_of(fen), polyglot_move(4, 7))]);
    engine.handshake();
    engine.send(&format!("position fen {fen}"));

    engine.send("go depth 1");
    let reply = engine
        .read_until(is_bestmove, REPLY_TIMEOUT)
        .expect("bestmove");

    assert_eq!(bestmove_of(&reply), "e1h1", "output {reply:?}");
}

#[test]
fn an_illegal_book_move_is_never_played() {
    let mut engine = engine_with_book(
        "illegal_a2a5",
        &mut [(key_of(START_FEN), polyglot_move(8, 32))],
    );
    engine.handshake();
    engine.send("position startpos");

    engine.send("go depth 1");
    let reply = engine
        .read_until(is_bestmove, REPLY_TIMEOUT)
        .expect("bestmove");

    let played = bestmove_of(&reply);
    assert!(
        legal_uci_moves(START_FEN).contains(&played),
        "played {played}; output {reply:?}"
    );
}

const MIDDLEGAME: &str = "r1bq1rk1/ppp1nppp/4n3/3p3Q/3P4/1BP1B3/PP1N2PP/R4RK1 w - - 1 16";
const QUIET: Duration = Duration::from_secs(3);

/// An engine set up to answer from the book: the start position, book on.
fn engine_in_book() -> Engine {
    let mut engine = Engine::start();
    engine.handshake();
    engine.send("position startpos");
    engine
}

/// An engine playing as a coach level in a middlegame.
fn coach_engine() -> Engine {
    let mut engine = Engine::start();
    engine.handshake();
    engine.send("setoption name CoachLevel value 3");
    engine.send(&format!("position fen {MIDDLEGAME}"));
    engine
}

#[test]
fn go_ponder_holds_a_book_move_until_stop() {
    let mut engine = engine_in_book();

    engine.send("go ponder wtime 60000 btime 60000");
    let early = engine.read_until(is_bestmove, QUIET);
    engine.send("stop");
    let on_stop = engine.read_until(is_bestmove, REPLY_TIMEOUT);

    assert!(early.is_err(), "bestmove sent while pondering: {early:?}");
    assert!(on_stop.is_ok(), "no bestmove after stop");
}

#[test]
fn ponderhit_releases_a_held_book_move() {
    let mut engine = engine_in_book();
    engine.send("go ponder wtime 60000 btime 60000");
    let _ = engine.read_until(is_bestmove, QUIET);

    engine.send("ponderhit");
    let reply = engine.read_until(is_bestmove, REPLY_TIMEOUT);

    assert!(reply.is_ok(), "no bestmove after ponderhit");
}

#[test]
fn go_ponder_holds_a_coach_move_until_stop() {
    let mut engine = coach_engine();

    engine.send("go ponder wtime 60000 btime 60000");
    let early = engine.read_until(is_bestmove, QUIET);
    engine.send("stop");
    let on_stop = engine.read_until(is_bestmove, REPLY_TIMEOUT);

    assert!(early.is_err(), "bestmove sent while pondering: {early:?}");
    assert!(on_stop.is_ok(), "no bestmove after stop");
}

#[test]
fn go_infinite_holds_a_coach_move_until_stop() {
    let mut engine = coach_engine();

    engine.send("go infinite");
    let early = engine.read_until(is_bestmove, QUIET);
    engine.send("stop");
    let on_stop = engine.read_until(is_bestmove, REPLY_TIMEOUT);

    assert!(
        early.is_err(),
        "bestmove sent during go infinite: {early:?}"
    );
    assert!(on_stop.is_ok(), "no bestmove after stop");
}

#[test]
fn go_searchmoves_restricts_the_best_move() {
    let mut engine = Engine::start();
    engine.handshake();
    engine.send("setoption name OwnBook value false");
    engine.send("position startpos");

    engine.send("go depth 6 searchmoves a2a3 h2h3");
    let reply = engine
        .read_until(is_bestmove, NET_LOAD_TIMEOUT)
        .expect("bestmove");

    let played = bestmove_of(&reply);
    assert!(
        ["a2a3", "h2h3"].contains(&played.as_str()),
        "played {played}"
    );
}

/// The `info string eval:` line the engine prints for its current position.
fn eval_line(engine: &mut Engine) -> String {
    engine.send("isready");
    engine
        .read_until(|l| l == "readyok", NET_LOAD_TIMEOUT)
        .expect("readyok");
    engine.send("eval");
    let lines = engine
        .read_until(|l| l.starts_with("info string eval:"), REPLY_TIMEOUT)
        .expect("eval output");
    lines.last().expect("eval line").clone()
}

#[test]
fn changing_the_evaluator_after_isready_keeps_the_engine_running() {
    let mut engine = Engine::start();
    engine.handshake();

    engine.send("setoption name Evaluator value bootstrap");
    engine.send("isready");
    let reply = engine.read_until(|l| l == "readyok", NET_LOAD_TIMEOUT);

    assert!(reply.is_ok(), "no readyok; output {:?}", reply.unwrap_err());
}

#[test]
fn a_new_evaluator_takes_effect_and_switching_back_restores_the_first() {
    let mut engine = Engine::start();
    engine.handshake();
    engine.send(&format!("position fen {MIDDLEGAME}"));
    let stockfish = eval_line(&mut engine);

    engine.send("setoption name Evaluator value bootstrap");
    let bootstrap = eval_line(&mut engine);
    engine.send("setoption name Evaluator value stockfish");
    let stockfish_again = eval_line(&mut engine);

    assert_ne!(bootstrap, stockfish, "the bootstrap eval is Stockfish's");
    assert_eq!(stockfish_again, stockfish);
}

#[test]
fn an_eval_file_that_cannot_load_is_reported_and_the_engine_keeps_its_networks() {
    let mut engine = Engine::start();
    engine.handshake();

    engine.send("setoption name EvalFile value /nonexistent/other.nnue");
    engine.send("isready");
    let reply = engine.read_until(|l| l == "readyok", NET_LOAD_TIMEOUT);

    let reply = reply.expect("readyok after a failed load");
    assert!(
        reply.iter().any(|l| l.starts_with("info string")),
        "replies {reply:?}"
    );
}
