use crate::types::Centipawns;
use std::fmt;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

const ANALYSIS_TIMEOUT: Duration = Duration::from_secs(600);
const EVAL_TIMEOUT: Duration = Duration::from_secs(5);

/// A UCI score from the side to move's point of view.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum UciScore {
    Centipawns(Centipawns),
    /// Moves to mate; negative when the side to move is being mated.
    Mate(i32),
}

impl UciScore {
    /// Parses the tokens following `score` in an info line; `None` for bound-only scores.
    fn parse(tokens: &[&str]) -> Option<Self> {
        let value = tokens.get(1)?.parse::<i32>().ok()?;
        let is_bound = tokens
            .get(2)
            .is_some_and(|t| *t == "lowerbound" || *t == "upperbound");
        if is_bound {
            return None;
        }
        match *tokens.first()? {
            "cp" => Some(Self::Centipawns(Centipawns::new(value))),
            "mate" => Some(Self::Mate(value)),
            _ => None,
        }
    }
}

pub struct Analysis {
    pub score: Option<UciScore>,
    pub depth: u32,
    /// `None` when the engine reported no legal move.
    pub best_move: Option<String>,
}

/// Extracts depth and exact score from an `info` line.
fn parse_info_line(line: &str) -> Option<(u32, UciScore)> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    if tokens.first() != Some(&"info") {
        return None;
    }
    let depth_idx = tokens.iter().position(|&t| t == "depth")?;
    let depth = tokens.get(depth_idx + 1)?.parse().ok()?;
    let score_idx = tokens.iter().position(|&t| t == "score")?;
    let score = UciScore::parse(&tokens[score_idx + 1..])?;
    Some((depth, score))
}

/// Parses a static-eval report into centipawns from White's point of view.
///
/// Accepts Stockfish's `Final evaluation  +0.12 (white side)` and this engine's
/// `info string eval: +12 cp (white to move)`.
fn parse_static_eval_white(line: &str) -> Option<Centipawns> {
    if let Some(rest) = line.strip_prefix("Final evaluation") {
        let pawns: f64 = rest.split_whitespace().next()?.parse().ok()?;
        return Some(Centipawns::new((pawns * 100.0).round() as i32));
    }
    let rest = line.strip_prefix("info string eval:")?;
    let mut tokens = rest.split_whitespace();
    let side_to_move = Centipawns::new(tokens.next()?.trim_start_matches('+').parse().ok()?);
    let black_to_move = rest.contains("(black to move)");
    Some(if black_to_move {
        -side_to_move
    } else {
        side_to_move
    })
}

fn is_static_eval_line(line: &str) -> bool {
    line.starts_with("Final evaluation") || line.starts_with("info string eval:")
}

/// Why talking to an engine process failed.
#[derive(Debug)]
pub enum EngineError {
    Spawn {
        path: PathBuf,
        source: io::Error,
    },
    Send {
        engine: String,
        source: io::Error,
    },
    /// The reply waited for did not come in time.
    TimedOut {
        engine: String,
    },
    /// The engine's output closed: the process has exited.
    Died {
        engine: String,
    },
    /// A `bestmove` line without a move.
    MalformedBestMove {
        engine: String,
    },
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn { path, source } => {
                write!(f, "Failed to spawn '{}': {source}", path.display())
            }
            Self::Send { engine, source } => write!(f, "send to {engine}: {source}"),
            Self::TimedOut { engine } => write!(f, "{engine}: read timed out"),
            Self::Died { engine } => write!(f, "{engine}: engine process died"),
            Self::MalformedBestMove { engine } => {
                write!(f, "{engine}: malformed bestmove response")
            }
        }
    }
}

impl std::error::Error for EngineError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn { source, .. } | Self::Send { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// The error for a pipe that `Stdio::piped` should have opened.
fn missing_pipe(path: &Path, pipe: &str) -> EngineError {
    EngineError::Spawn {
        path: path.to_path_buf(),
        source: io::Error::other(format!("no {pipe} pipe")),
    }
}

pub struct UciEngine {
    pub name: String,
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
}

/// The name a tool shows for the engine binary at `path`: its file name.
pub fn engine_display_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

impl UciEngine {
    /// Starts the engine at `path`, on the CPUs `cpus` lists (taskset
    /// syntax) if given, completes the UCI handshake and sets `options`.
    pub fn start(
        path: &Path,
        options: &[(String, String)],
        cpus: Option<&str>,
    ) -> Result<Self, EngineError> {
        let mut engine = Self::spawn_pinned(path, cpus)?;
        engine.init_uci()?;
        engine.set_options(options)?;
        engine.is_ready()?;
        Ok(engine)
    }

    fn spawn_pinned(path: &Path, cpus: Option<&str>) -> Result<Self, EngineError> {
        let mut command = match cpus {
            Some(cpus) => {
                let mut taskset = Command::new("taskset");
                taskset.args(["-c", cpus]).arg(path);
                taskset
            }
            None => Command::new(path),
        };
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|source| EngineError::Spawn {
                path: path.to_path_buf(),
                source,
            })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| missing_pipe(path, "stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| missing_pipe(path, "stdout"))?;

        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(l) => {
                        if tx.send(l).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let name = engine_display_name(path);
        Ok(Self {
            name,
            child,
            stdin,
            lines: rx,
        })
    }

    fn timed_out(&self) -> EngineError {
        EngineError::TimedOut {
            engine: self.name.clone(),
        }
    }

    fn died(&self) -> EngineError {
        EngineError::Died {
            engine: self.name.clone(),
        }
    }

    fn send(&mut self, cmd: &str) -> Result<(), EngineError> {
        writeln!(self.stdin, "{cmd}").map_err(|source| EngineError::Send {
            engine: self.name.clone(),
            source,
        })
    }

    fn read_until(
        &self,
        pred: impl Fn(&str) -> bool,
        timeout: Duration,
    ) -> Result<String, EngineError> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(self.timed_out());
            }
            match self.lines.recv_timeout(remaining) {
                Ok(line) => {
                    if pred(&line) {
                        return Ok(line);
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(self.timed_out());
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(self.died());
                }
            }
        }
    }

    pub fn init_uci(&mut self) -> Result<(), EngineError> {
        self.send("uci")?;
        self.read_until(|l| l.trim() == "uciok", Duration::from_secs(10))?;
        Ok(())
    }

    pub fn set_options(&mut self, options: &[(String, String)]) -> Result<(), EngineError> {
        for (name, value) in options {
            self.send(&format!("setoption name {name} value {value}"))?;
        }
        Ok(())
    }

    pub fn is_ready(&mut self) -> Result<(), EngineError> {
        self.send("isready")?;
        self.read_until(|l| l.trim() == "readyok", Duration::from_secs(30))?;
        Ok(())
    }

    pub fn new_game(&mut self) -> Result<(), EngineError> {
        self.send("ucinewgame")?;
        self.is_ready()
    }

    pub fn set_position(
        &mut self,
        start_fen: Option<&str>,
        moves: &[String],
    ) -> Result<(), EngineError> {
        let pos = match start_fen {
            Some(fen) => format!("position fen {fen}"),
            None => "position startpos".to_string(),
        };
        if moves.is_empty() {
            self.send(&pos)
        } else {
            self.send(&format!("{} moves {}", pos, moves.join(" ")))
        }
    }

    pub fn go(
        &mut self,
        wtime: i64,
        btime: i64,
        winc: u64,
        binc: u64,
    ) -> Result<String, EngineError> {
        self.send(&format!(
            "go wtime {} btime {} winc {} binc {}",
            wtime.max(1),
            btime.max(1),
            winc,
            binc
        ))?;
        let line = self.read_until(|l| l.starts_with("bestmove"), Duration::from_secs(300))?;
        line.split_whitespace()
            .nth(1)
            .map(std::string::ToString::to_string)
            .ok_or_else(|| EngineError::MalformedBestMove {
                engine: self.name.clone(),
            })
    }

    /// Searches `fen` with the given `go` arguments (e.g. `depth 12`) from a fresh game.
    pub fn analyse(&mut self, fen: &str, go_args: &str) -> Result<Analysis, EngineError> {
        self.new_game()?;
        self.send(&format!("position fen {fen}"))?;
        self.send(&format!("go {go_args}"))?;

        let mut latest: Option<(u32, UciScore)> = None;
        let deadline = std::time::Instant::now() + ANALYSIS_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let line = match self.lines.recv_timeout(remaining) {
                Ok(line) => line,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(self.timed_out());
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(self.died());
                }
            };
            if let Some(info) = parse_info_line(&line) {
                latest = Some(info);
                continue;
            }
            if let Some(rest) = line.strip_prefix("bestmove") {
                let best_move = rest
                    .split_whitespace()
                    .next()
                    .filter(|m| *m != "(none)" && *m != "0000")
                    .map(str::to_string);
                return Ok(Analysis {
                    score: latest.map(|(_, score)| score),
                    depth: latest.map_or(0, |(depth, _)| depth),
                    best_move,
                });
            }
        }
    }

    /// Asks for the static evaluation via the non-standard `eval` command.
    /// Returns `None` if the engine does not support it or declines (e.g. in check).
    pub fn static_eval_white(&mut self, fen: &str) -> Result<Option<Centipawns>, EngineError> {
        self.send(&format!("position fen {fen}"))?;
        self.send("eval")?;
        match self.read_until(is_static_eval_line, EVAL_TIMEOUT) {
            Ok(line) => Ok(parse_static_eval_white(&line)),
            Err(EngineError::TimedOut { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn quit(mut self) {
        let _ = self.send("quit");
        let _ = self.child.wait();
    }
}

impl Drop for UciEngine {
    fn drop(&mut self) {
        let _ = writeln!(self.stdin, "quit");
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_line_with_exact_score_is_parsed() {
        let line = "info depth 12 seldepth 18 multipv 1 score cp -34 nodes 1000 pv e2e4";
        assert_eq!(
            parse_info_line(line),
            Some((12, UciScore::Centipawns(Centipawns::new(-34))))
        );
    }

    #[test]
    fn info_line_with_bound_score_is_ignored() {
        let line = "info depth 12 score cp 50 lowerbound nodes 10";
        assert_eq!(parse_info_line(line), None);
    }

    #[test]
    fn mate_score_is_parsed() {
        let line = "info depth 5 score mate -3 pv a1a8";
        assert_eq!(parse_info_line(line), Some((5, UciScore::Mate(-3))));
    }

    #[test]
    fn stockfish_static_eval_is_white_pov_centipawns() {
        let line = "Final evaluation       +0.12 (white side) [with scaled NNUE, ...]";
        assert_eq!(parse_static_eval_white(line), Some(Centipawns::new(12)));
        assert_eq!(
            parse_static_eval_white("Final evaluation: none (in check)"),
            None
        );
    }

    #[test]
    fn own_static_eval_is_converted_to_white_pov() {
        let line = "info string eval: +40 cp (black to move)";
        assert_eq!(parse_static_eval_white(line), Some(Centipawns::new(-40)));
        let line = "info string eval: -7 cp (white to move)";
        assert_eq!(parse_static_eval_white(line), Some(Centipawns::new(-7)));
    }
}
