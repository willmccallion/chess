use crate::board::Board;
use crate::nnue;
use crate::opening_book::get_book_move;
use crate::search::{SearchControl, SearchLimits, SearchThreads, search};
use crate::time::{TimeBudget, TimeControl};
use crate::tt::SharedTransTable;
use crate::types::START_FEN;
use crate::uci_io::{format_uci, parse_uci_move};
use std::io::{self, Write};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

const DEFAULT_HASH_MB: usize = 256;
const MAX_HASH_MB: usize = 4096;
const DEFAULT_THREADS: usize = 1;
const MAX_THREADS: usize = 128;
const DEFAULT_MOVE_OVERHEAD_MS: i64 = 10;
const MAX_MOVE_OVERHEAD_MS: i64 = 5000;
const MAX_DEPTH: usize = 127;

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

fn parse_check_value(value: &str) -> bool {
    matches!(value.to_ascii_lowercase().as_str(), "true" | "1" | "on")
}

fn has_token(cmd: &str, token: &str) -> bool {
    cmd.split_whitespace().any(|t| t.eq_ignore_ascii_case(token))
}

fn send(line: &str) {
    println!("{line}");
    let _ = io::stdout().flush();
}

/// A `go` running on a worker thread; the worker prints `bestmove` when it finishes
/// and hands the per-thread search state back.
struct ActiveSearch {
    handle: JoinHandle<SearchThreads>,
    control: Arc<SearchControl>,
    /// Budget to start when `ponderhit` arrives.
    ponder_budget: Option<TimeBudget>,
}

/// Stops any running search and returns its thread state to `threads`.
fn stop_search(active: &mut Option<ActiveSearch>, threads: &mut Option<SearchThreads>, count: usize) {
    if let Some(search) = active.take() {
        search.control.stop();
        *threads = search.handle.join().ok();
    }
    if threads.as_ref().is_none_or(|t| t.count() != count) {
        *threads = Some(SearchThreads::new(count));
    }
}

struct Options {
    hash_mb: usize,
    threads: usize,
    own_book: bool,
    move_overhead_ms: i64,
}

fn print_options() {
    send(&format!(
        "option name Hash type spin default {DEFAULT_HASH_MB} min 1 max {MAX_HASH_MB}"
    ));
    send(&format!(
        "option name Threads type spin default {DEFAULT_THREADS} min 1 max {MAX_THREADS}"
    ));
    send("option name Ponder type check default false");
    send("option name OwnBook type check default true");
    send(&format!(
        "option name Move Overhead type spin default {DEFAULT_MOVE_OVERHEAD_MS} min 0 max {MAX_MOVE_OVERHEAD_MS}"
    ));
}

fn parse_position(rest: &str) -> Board {
    let parts: Vec<&str> = rest.split_whitespace().collect();
    let moves_idx = parts.iter().position(|&s| s.eq_ignore_ascii_case("moves"));

    let mut board = match parts.first() {
        Some(&"fen") => {
            let fen_end = moves_idx.unwrap_or(parts.len());
            Board::from_fen(&parts[1..fen_end].join(" "))
                .unwrap_or_else(|_| Board::from_fen(START_FEN).expect("valid startpos"))
        }
        _ => Board::from_fen(START_FEN).expect("valid startpos"),
    };

    if let Some(idx) = moves_idx {
        for move_str in &parts[idx + 1..] {
            let Some(mv) = parse_uci_move(&mut board, move_str) else {
                send(&format!("info string ignoring illegal move {move_str}"));
                break;
            };
            board.make_move(mv);
        }
    }
    board
}

fn start_search(
    board: &Board,
    tt: &SharedTransTable,
    options: &Options,
    mut threads: SearchThreads,
    go_args: &str,
) -> ActiveSearch {
    let is_ponder = has_token(go_args, "ponder");
    let is_infinite = has_token(go_args, "infinite");
    let max_depth = extract_i64(go_args, "depth")
        .map_or(MAX_DEPTH, |d| (d.max(1) as usize).min(MAX_DEPTH));

    let tc = TimeControl {
        wtime: extract_i64(go_args, "wtime").unwrap_or(0),
        btime: extract_i64(go_args, "btime").unwrap_or(0),
        winc: extract_i64(go_args, "winc").unwrap_or(0),
        binc: extract_i64(go_args, "binc").unwrap_or(0),
        movestogo: extract_i64(go_args, "movestogo").unwrap_or(0) as i32,
        move_overhead_ms: options.move_overhead_ms,
    };
    let has_clock = tc.wtime > 0 || tc.btime > 0;

    let budget = if let Some(movetime) = extract_i64(go_args, "movetime") {
        let usable = (movetime - options.move_overhead_ms).max(1);
        Some(TimeBudget::fixed(usable as u64))
    } else if has_clock {
        Some(tc.budget(board.turn))
    } else {
        None
    };

    let (control, ponder_budget) = if is_ponder {
        (SearchControl::new(None), budget)
    } else if is_infinite {
        (SearchControl::new(None), None)
    } else {
        (SearchControl::new(budget), None)
    };

    let limits = SearchLimits {
        max_depth,
        wait_for_stop: is_ponder || is_infinite,
    };
    let board = board.clone();
    let tt = tt.clone();
    let worker_control = Arc::clone(&control);

    let handle = thread::spawn(move || {
        let result = search(&board, &tt, limits, &mut threads, &worker_control);
        match (result.best_move, result.ponder_move) {
            (Some(best), Some(ponder)) => send(&format!(
                "bestmove {} ponder {}",
                format_uci(best),
                format_uci(ponder)
            )),
            (Some(best), None) => send(&format!("bestmove {}", format_uci(best))),
            (None, _) => send("bestmove 0000"),
        }
        threads
    });

    ActiveSearch {
        handle,
        control,
        ponder_budget,
    }
}

fn apply_option(name: &str, value: &str, options: &mut Options, tt: &mut SharedTransTable) {
    if name.eq_ignore_ascii_case("Hash") {
        if let Ok(size) = value.parse::<usize>()
            && (1..=MAX_HASH_MB).contains(&size)
            && options.hash_mb != size
        {
            options.hash_mb = size;
            *tt = SharedTransTable::new(size);
        }
    } else if name.eq_ignore_ascii_case("Threads") {
        if let Ok(n) = value.parse::<usize>()
            && (1..=MAX_THREADS).contains(&n)
        {
            options.threads = n;
        }
    } else if name.eq_ignore_ascii_case("OwnBook") {
        options.own_book = parse_check_value(value);
    } else if name.eq_ignore_ascii_case("Move Overhead")
        && let Ok(ms) = value.parse::<i64>()
    {
        options.move_overhead_ms = ms.clamp(0, MAX_MOVE_OVERHEAD_MS);
    }
}

fn print_eval(board: &mut Board) {
    let score = nnue::evaluate(board);
    let side = if board.turn == crate::types::Color::White {
        "white"
    } else {
        "black"
    };
    let sign = if score >= 0 { "+" } else { "" };
    send(&format!("info string eval: {sign}{score} cp ({side} to move)"));
    send(&format!("info string fen: {}", board.to_fen()));
}

pub fn run_uci() {
    let mut board = Board::from_fen(START_FEN).expect("valid startpos");
    let mut options = Options {
        hash_mb: DEFAULT_HASH_MB,
        threads: DEFAULT_THREADS,
        own_book: true,
        move_overhead_ms: DEFAULT_MOVE_OVERHEAD_MS,
    };
    let mut tt = SharedTransTable::new(options.hash_mb);
    let mut active: Option<ActiveSearch> = None;
    let mut threads = Some(SearchThreads::new(options.threads));

    let stdin = io::stdin();
    let mut line = String::new();
    loop {
        line.clear();
        match stdin.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let cmd = line.trim();
        let (verb, rest) = cmd.split_once(' ').unwrap_or((cmd, ""));

        match verb {
            "uci" => {
                send("id name chess");
                send("id author Will");
                print_options();
                send("uciok");
            }
            "isready" => send("readyok"),
            "ucinewgame" => {
                stop_search(&mut active, &mut threads, options.threads);
                tt.clear();
                if let Some(t) = threads.as_mut() {
                    t.clear();
                }
            }
            "setoption" => {
                stop_search(&mut active, &mut threads, options.threads);
                if let Some((name, value)) = parse_setoption(rest) {
                    apply_option(&name, &value, &mut options, &mut tt);
                }
            }
            "position" => {
                stop_search(&mut active, &mut threads, options.threads);
                board = parse_position(rest);
            }
            "go" => {
                stop_search(&mut active, &mut threads, options.threads);
                if options.own_book
                    && !has_token(rest, "infinite")
                    && let Some(book_uci) = get_book_move(&board)
                {
                    send(&format!("bestmove {book_uci}"));
                    continue;
                }
                let search_threads = threads
                    .take()
                    .unwrap_or_else(|| SearchThreads::new(options.threads));
                active = Some(start_search(&board, &tt, &options, search_threads, rest));
            }
            "ponderhit" => {
                if let Some(search) = &active {
                    match search.ponder_budget {
                        Some(budget) => search.control.set_budget_from_now(budget),
                        None => search.control.stop(),
                    }
                }
            }
            "stop" => stop_search(&mut active, &mut threads, options.threads),
            "eval" => print_eval(&mut board),
            "quit" => break,
            _ => {}
        }
    }
    stop_search(&mut active, &mut threads, options.threads);
}
