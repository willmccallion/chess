use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

const ANALYSIS_TIMEOUT: Duration = Duration::from_secs(600);
const EVAL_TIMEOUT: Duration = Duration::from_secs(5);

/// A UCI score from the side to move's point of view.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum UciScore {
    Centipawns(i32),
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
            "cp" => Some(Self::Centipawns(value)),
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
fn parse_static_eval_white_cp(line: &str) -> Option<i32> {
    if let Some(rest) = line.strip_prefix("Final evaluation") {
        let pawns: f64 = rest.split_whitespace().next()?.parse().ok()?;
        return Some((pawns * 100.0).round() as i32);
    }
    let rest = line.strip_prefix("info string eval:")?;
    let mut tokens = rest.split_whitespace();
    let side_to_move_cp: i32 = tokens.next()?.trim_start_matches('+').parse().ok()?;
    let black_to_move = rest.contains("(black to move)");
    Some(if black_to_move {
        -side_to_move_cp
    } else {
        side_to_move_cp
    })
}

fn is_static_eval_line(line: &str) -> bool {
    line.starts_with("Final evaluation") || line.starts_with("info string eval:")
}

pub struct UciEngine {
    pub name: String,
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
}

impl UciEngine {
    pub fn spawn(path: &str) -> Result<Self, String> {
        let mut child = Command::new(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Failed to spawn '{}': {}", path, e))?;

        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();

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

        let name = path.rsplit('/').next().unwrap_or(path).to_string();
        Ok(Self {
            name,
            child,
            stdin,
            lines: rx,
        })
    }

    fn send(&mut self, cmd: &str) -> Result<(), String> {
        writeln!(self.stdin, "{}", cmd).map_err(|e| format!("send to {}: {}", self.name, e))
    }

    fn read_until(
        &self,
        pred: impl Fn(&str) -> bool,
        timeout: Duration,
    ) -> Result<String, String> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(format!("{}: read timed out", self.name));
            }
            match self.lines.recv_timeout(remaining) {
                Ok(line) => {
                    if pred(&line) {
                        return Ok(line);
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(format!("{}: read timed out", self.name));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(format!("{}: engine process died", self.name));
                }
            }
        }
    }

    pub fn init_uci(&mut self) -> Result<(), String> {
        self.send("uci")?;
        self.read_until(|l| l.trim() == "uciok", Duration::from_secs(10))?;
        Ok(())
    }

    pub fn set_options(&mut self, options: &[(String, String)]) -> Result<(), String> {
        for (name, value) in options {
            self.send(&format!("setoption name {} value {}", name, value))?;
        }
        Ok(())
    }

    pub fn is_ready(&mut self) -> Result<(), String> {
        self.send("isready")?;
        self.read_until(|l| l.trim() == "readyok", Duration::from_secs(30))?;
        Ok(())
    }

    pub fn new_game(&mut self) -> Result<(), String> {
        self.send("ucinewgame")?;
        self.is_ready()
    }

    pub fn set_position(&mut self, start_fen: Option<&str>, moves: &[String]) -> Result<(), String> {
        let pos = match start_fen {
            Some(fen) => format!("position fen {}", fen),
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
    ) -> Result<String, String> {
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
            .map(|s| s.to_string())
            .ok_or_else(|| format!("{}: malformed bestmove response", self.name))
    }

    /// Searches `fen` with the given `go` arguments (e.g. `depth 12`) from a fresh game.
    pub fn analyse(&mut self, fen: &str, go_args: &str) -> Result<Analysis, String> {
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
                    return Err(format!("{}: analysis timed out", self.name));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(format!("{}: engine process died", self.name));
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
    pub fn static_eval_white_cp(&mut self, fen: &str) -> Result<Option<i32>, String> {
        self.send(&format!("position fen {fen}"))?;
        self.send("eval")?;
        match self.read_until(is_static_eval_line, EVAL_TIMEOUT) {
            Ok(line) => Ok(parse_static_eval_white_cp(&line)),
            Err(_) => Ok(None),
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
        assert_eq!(parse_info_line(line), Some((12, UciScore::Centipawns(-34))));
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
        assert_eq!(parse_static_eval_white_cp(line), Some(12));
        assert_eq!(parse_static_eval_white_cp("Final evaluation: none (in check)"), None);
    }

    #[test]
    fn own_static_eval_is_converted_to_white_pov() {
        let line = "info string eval: +40 cp (black to move)";
        assert_eq!(parse_static_eval_white_cp(line), Some(-40));
        let line = "info string eval: -7 cp (white to move)";
        assert_eq!(parse_static_eval_white_cp(line), Some(-7));
    }
}
