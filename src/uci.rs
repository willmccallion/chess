use crate::board::Board;
use crate::opening_book::get_book_move;
use crate::search::best_move_timed;
use crate::time::TimeControl;
use crate::tt::SharedTransTable;
use crate::types::START_FEN;
use crate::uci_io::{format_uci, parse_uci_move};
use num_cpus;
use std::io::{self, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

const SEARCH_THREAD_STACK: usize = 32 * 1024 * 1024; // 32 MiB

#[inline]
fn extract_i64(cmd: &str, key: &str) -> Option<i64> {
    for tok in cmd.split_whitespace().collect::<Vec<_>>().windows(2) {
        if let Ok(v) = tok[1].parse::<i64>()
            && tok[0].eq_ignore_ascii_case(key)
        {
            return Some(v);
        }
    }
    None
}

#[inline]
fn parse_setoption(rest: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = rest.splitn(2, "name").collect();
    let after = parts.get(1)?.trim();
    if let Some(idx) = after.to_ascii_lowercase().find(" value ") {
        let (n, v) = after.split_at(idx);
        Some((
            n.trim().to_string(),
            v.trim_start_matches(" value ").trim().to_string(),
        ))
    } else {
        Some((after.trim().to_string(), String::new()))
    }
}

struct PonderState {
    handle: Option<std::thread::JoinHandle<()>>,
    stop_signal: Option<Arc<AtomicBool>>,
    enabled: bool,
}
impl PonderState {
    fn new() -> Self {
        Self {
            handle: None,
            stop_signal: None,
            enabled: false,
        }
    }

    #[inline]
    fn stop_and_join(&mut self) {
        if let Some(sig) = &self.stop_signal {
            sig.store(true, Ordering::Relaxed);
        }
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        self.stop_signal = None;
    }
}

fn info<S: AsRef<str>>(s: S) {
    println!("info string {}", s.as_ref());
    let _ = io::stdout().flush();
}

fn search_and_output(
    b: &Board,
    tt: &mut SharedTransTable,
    time_ms: u64,
    depth: usize,
    stop: Arc<AtomicBool>,
    main_thread: bool,
) {
    let (best, _reached_depth, _nodes) =
        best_move_timed(b, tt, time_ms, depth, Arc::clone(&stop), main_thread);

    if let Some(m) = best {
        let mut ponder_str = String::new();
        let mut temp_board = b.clone();
        temp_board.make_move(m);
        if let Some(ponder_move) = tt.probe(temp_board.zobrist).and_then(|e| e.best_move()) {
            if temp_board.piece_on[ponder_move.from as usize].color() == Some(temp_board.turn) {
                ponder_str = format!(" ponder {}", format_uci(ponder_move));
            }
        }
        println!("bestmove {}{}", format_uci(m), ponder_str);
    } else {
        println!("bestmove 0000");
    }
    let _ = io::stdout().flush();
}

pub fn run_uci() {
    let mut b = Board::from_fen(START_FEN).expect("valid startpos");
    let mut tc = TimeControl::default();

    let mut tt_size_mb: usize = 256;
    let mut tt = SharedTransTable::new(tt_size_mb);
    let mut threads_count: usize = num_cpus::get().max(1);
    let mut ponder = PonderState::new();

    loop {
        let mut line = String::new();
        if io::stdin().read_line(&mut line).is_err() {
            break;
        }
        let cmd = line.trim();

        if cmd.eq_ignore_ascii_case("uci") {
            println!("id name chess");
            println!("id author Will");
            println!(
                "option name Hash type spin default {} min 1 max 4096",
                tt_size_mb
            );
            println!("option name Threads type spin default {} min 1 max 128", 1);
            println!("option name Ponder type check default {}", ponder.enabled);
            println!("uciok");
            let _ = io::stdout().flush();
            continue;
        }

        if cmd.eq_ignore_ascii_case("isready") {
            println!("readyok");
            let _ = io::stdout().flush();
            continue;
        }

        if cmd.eq_ignore_ascii_case("ucinewgame") {
            b = Board::from_fen(START_FEN).unwrap();
            println!(
                "info string Polyglot key for startpos: {:x}",
                crate::polyglot_zobrist::calculate_key(&b)
            );
            tt.clear();
            ponder.stop_and_join();
            let _ = io::stdout().flush();
            continue;
        }

        if let Some(rest) = cmd.strip_prefix("setoption ") {
            if let Some((name, value)) = parse_setoption(rest) {
                if name.eq_ignore_ascii_case("Hash") {
                    if let Ok(size) = value.parse::<usize>()
                        && (1..=4096).contains(&size)
                        && tt_size_mb != size
                    {
                        tt_size_mb = size;
                        tt = SharedTransTable::new(tt_size_mb);
                    }
                } else if name.eq_ignore_ascii_case("Threads") {
                    if let Ok(n) = value.parse::<usize>()
                        && (1..=128).contains(&n)
                    {
                        threads_count = n;
                    }
                } else if name.eq_ignore_ascii_case("Ponder") {
                    ponder.enabled =
                        matches!(value.to_ascii_lowercase().as_str(), "true" | "1" | "on");
                }
            }
            continue;
        }

        if let Some(rest) = cmd.strip_prefix("position ") {
            ponder.stop_and_join();

            let parts: Vec<&str> = rest.split_whitespace().collect();
            let mut moves_start_index: Option<usize> = None;

            if parts.first() == Some(&"startpos") {
                b = Board::from_fen(START_FEN).unwrap();
                if parts.get(1) == Some(&"moves") {
                    moves_start_index = Some(2);
                }
            } else if parts.first() == Some(&"fen") {
                if let Some(index) = parts.iter().position(|&s| s.eq_ignore_ascii_case("moves")) {
                    let fen_parts = &parts[1..index];
                    b = Board::from_fen(&fen_parts.join(" "))
                        .unwrap_or_else(|_| Board::from_fen(START_FEN).unwrap());
                    moves_start_index = Some(index + 1);
                } else {
                    let fen_parts = &parts[1..];
                    b = Board::from_fen(&fen_parts.join(" "))
                        .unwrap_or_else(|_| Board::from_fen(START_FEN).unwrap());
                }
            }

            if let Some(start_index) = moves_start_index {
                for move_str in parts[start_index..].iter().copied() {
                    if let Some(mv) = parse_uci_move(&mut b, move_str) {
                        let _ = b.make_move(mv);
                    }
                }
            }
            continue;
        }

        if cmd.eq_ignore_ascii_case("ponderhit") {
            continue;
        }

        if cmd.eq_ignore_ascii_case("stop") {
            ponder.stop_and_join();
            if let Some(best) = tt.probe(b.zobrist).and_then(|e| e.best_move()) {
                println!("bestmove {}", format_uci(best));
            } else {
                println!("bestmove 0000");
            }
            let _ = io::stdout().flush();
            continue;
        }

        if let Some(rest) = cmd.strip_prefix("go") {
            info(format!("FEN before go: {}", b.to_fen()));
            ponder.stop_and_join();

            if let Some(book_uci) = get_book_move(&b) {
                println!("bestmove {}", book_uci);
                let _ = io::stdout().flush();
                continue;
            }

            let is_ponder = rest
                .split_whitespace()
                .any(|t| t.eq_ignore_ascii_case("ponder"));
            let is_infinite = rest
                .split_whitespace()
                .any(|t| t.eq_ignore_ascii_case("infinite"));

            let depth = extract_i64(rest, "depth").map_or(128, |d| d.max(1) as usize);
            let helper_depth = depth.min(128);

            tc.wtime = extract_i64(rest, "wtime").unwrap_or(0);
            tc.btime = extract_i64(rest, "btime").unwrap_or(0);
            tc.winc = extract_i64(rest, "winc").unwrap_or(0);
            tc.binc = extract_i64(rest, "binc").unwrap_or(0);
            tc.movestogo = extract_i64(rest, "movestogo").unwrap_or(0) as i32;

            let time_to_use = if is_ponder || is_infinite {
                u64::MAX / 4
            } else if let Some(movetime) = extract_i64(rest, "movetime") {
                movetime.max(0) as u64
            } else {
                tc.allocation_ms(b.turn).0
            };

            if is_ponder {
                if ponder.enabled {
                    let board_clone = b.clone();
                    let mut tt_for_thread = tt.clone();
                    let stop = Arc::new(AtomicBool::new(false));
                    let stop_clone = Arc::clone(&stop);

                    let builder = thread::Builder::new()
                        .name("ponder-main".into())
                        .stack_size(SEARCH_THREAD_STACK);
                    let handle = builder
                        .spawn(move || {
                            let mut helpers = Vec::new();
                            let threads_count = num_cpus::get().max(1);
                            for i in 0..threads_count.saturating_sub(1) {
                                let board_h = board_clone.clone();
                                let tt_h = tt_for_thread.clone();
                                let stop_h = Arc::clone(&stop_clone);
                                let name = format!("ponder-helper-{}", i);
                                let _ = thread::Builder::new()
                                    .name(name)
                                    .stack_size(SEARCH_THREAD_STACK)
                                    .spawn(move || {
                                        let mut tt_loc = tt_h;
                                        let _ = best_move_timed(
                                            &board_h,
                                            &mut tt_loc,
                                            u64::MAX / 4,
                                            helper_depth,
                                            stop_h,
                                            false,
                                        );
                                    })
                                    .map(|jh| helpers.push(jh));
                            }

                            search_and_output(
                                &board_clone,
                                &mut tt_for_thread,
                                time_to_use,
                                depth, // main ponder thread uses requested depth
                                stop_clone,
                                true,
                            );
                            for h in helpers {
                                let _ = h.join();
                            }
                        })
                        .expect("spawn ponder");

                    ponder.handle = Some(handle);
                    ponder.stop_signal = Some(stop);
                    continue;
                }
            }

            let stop_signal = Arc::new(AtomicBool::new(false));

            let mut helpers = vec![];
            for i in 0..threads_count.saturating_sub(1) {
                let board_clone = b.clone();
                let tt_clone = tt.clone();
                let stop_clone = Arc::clone(&stop_signal);
                let name = format!("helper-{}", i);
                let _ = thread::Builder::new()
                    .name(name)
                    .stack_size(SEARCH_THREAD_STACK)
                    .spawn(move || {
                        let mut tt_local = tt_clone;
                        let _ = best_move_timed(
                            &board_clone,
                            &mut tt_local,
                            u64::MAX / 4,
                            helper_depth,
                            stop_clone,
                            false,
                        );
                    })
                    .map(|jh| helpers.push(jh));
            }

            // Main search
            search_and_output(
                &b,
                &mut tt,
                time_to_use,
                depth,
                Arc::clone(&stop_signal),
                true,
            );

            stop_signal.store(true, Ordering::Relaxed);
            for h in helpers {
                let _ = h.join();
            }
            continue;
        }

        if cmd.eq_ignore_ascii_case("quit") {
            ponder.stop_and_join();
            break;
        }
    }
}
