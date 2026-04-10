use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

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
