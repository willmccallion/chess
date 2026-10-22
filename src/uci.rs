use crate::analysis::Searcher;
use crate::board::Board;
use crate::coach::bot::{Level, choose_move, pick_weighted};
use crate::eval::{self, EvalOptions};
use crate::opening_book::{get_book_move, legal_book_moves};
use crate::search::{NodeLimit, SearchControl, SearchLimits, SearchThreads, search};
use crate::search_params::SearchParams;
use crate::time::{TimeBudget, TimeControl};
use crate::tt::SharedTransTable;
use crate::types::{Color, Move, START_FEN};
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
/// Hash for the coach bot's small per-move searches.
const COACH_HASH_MB: usize = 16;

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
    cmd.split_whitespace()
        .any(|t| t.eq_ignore_ascii_case(token))
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
fn stop_search(
    active: &mut Option<ActiveSearch>,
    threads: &mut Option<SearchThreads>,
    count: usize,
) {
    if let Some(search) = active.take() {
        search.control.stop();
        *threads = search.handle.join().ok();
    }
    if threads.as_ref().is_none_or(|t| t.count() != count) {
        *threads = Some(SearchThreads::new(count));
    }
}

struct Options {
    /// When set, `go` plays like this coach level instead of at full strength.
    coach_level: Option<Level>,
    hash_mb: usize,
    threads: usize,
    own_book: bool,
    move_overhead_ms: i64,
    eval: EvalOptions,
    search_params: SearchParams,
}

/// Loads the evaluator named by the options, or exits: the engine cannot play without it.
fn require_networks(options: &Options) {
    if let Err(e) = options.eval.load() {
        send(&format!("info string ERROR: {e}"));
        std::process::exit(1);
    }
}

fn print_options(options: &Options) {
    send(&format!(
        "option name Hash type spin default {DEFAULT_HASH_MB} min 1 max {MAX_HASH_MB}"
    ));
    send(&format!(
        "option name Threads type spin default {DEFAULT_THREADS} min 1 max {MAX_THREADS}"
    ));
    send("option name Ponder type check default false");
    send("option name OwnBook type check default true");
    send(&format!(
        "option name CoachLevel type spin default 0 min 0 max {}",
        Level::STRONGEST.number()
    ));
    send(&format!(
        "option name Move Overhead type spin default {DEFAULT_MOVE_OVERHEAD_MS} min 0 max {MAX_MOVE_OVERHEAD_MS}"
    ));
    for line in options.eval.uci_options() {
        send(&line);
    }
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
        board.forget_eval_history();
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
    let max_depth =
        extract_i64(go_args, "depth").map_or(MAX_DEPTH, |d| (d.max(1) as usize).min(MAX_DEPTH));

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
        params: options.search_params,
        max_depth,
        wait_for_stop: is_ponder || is_infinite,
        nodes: extract_i64(go_args, "nodes").map(|n| {
            let n = n.max(1) as u64;
            NodeLimit { soft: n, hard: n }
        }),
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
    } else if name.eq_ignore_ascii_case("CoachLevel") {
        options.coach_level = value.parse::<u8>().ok().and_then(Level::new);
    } else if name.eq_ignore_ascii_case("Move Overhead") {
        if let Ok(ms) = value.parse::<i64>() {
            options.move_overhead_ms = ms.clamp(0, MAX_MOVE_OVERHEAD_MS);
        }
    } else if !options.eval.set_uci_option(name, value)
        && let Ok(number) = value.parse::<i32>()
    {
        // Tunable search parameters are accepted by name but not advertised.
        options.search_params.set(name, number);
    }
}

/// The coach bot's move at `level`: from the book early on, otherwise from every move's
/// score with the level's errors mixed in. Runs to completion; `stop` is not checked.
fn coach_move(
    board: &Board,
    level: Level,
    remaining_ms: Option<i64>,
    searcher: &mut Searcher,
) -> Option<Move> {
    let spec = level.spec();
    let mut rng = rand::thread_rng();
    let ply =
        (board.fullmove_number.max(1) as usize - 1) * 2 + usize::from(board.turn == Color::Black);
    if ply < spec.book_plies {
        let book: Vec<(Move, u16)> = legal_book_moves(board)
            .iter()
            .map(|w| (w.m, w.weight))
            .collect();
        if let Some(m) = pick_weighted(&book, &mut rng) {
            return Some(m);
        }
    }
    let control = SearchControl::new(None);
    let scores = searcher.score_moves(board, spec.nodes_for_clock(remaining_ms), &control)?;
    choose_move(&scores, spec, &mut rng)
}

fn print_eval(board: &mut Board) {
    let score = eval::evaluate(board);
    let side = if board.turn == crate::types::Color::White {
        "white"
    } else {
        "black"
    };
    let sign = if score >= 0 { "+" } else { "" };
    send(&format!(
        "info string eval: {sign}{score} cp ({side} to move)"
    ));
    send(&format!("info string fen: {}", board.to_fen()));
}

pub fn run_uci(eval_kind: crate::eval::EvalKind) {
    let mut board = Board::from_fen(START_FEN).expect("valid startpos");
    let mut options = Options {
        coach_level: None,
        hash_mb: DEFAULT_HASH_MB,
        threads: DEFAULT_THREADS,
        own_book: true,
        move_overhead_ms: DEFAULT_MOVE_OVERHEAD_MS,
        eval: EvalOptions::with_kind(eval_kind),
        search_params: SearchParams::default(),
    };
    let mut tt = SharedTransTable::new(options.hash_mb);
    let mut active: Option<ActiveSearch> = None;
    let mut threads = Some(SearchThreads::new(options.threads));
    let mut coach_searcher: Option<Searcher> = None;

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
                print_options(&options);
                send("uciok");
            }
            "isready" => {
                require_networks(&options);
                send("readyok");
            }
            "ucinewgame" => {
                stop_search(&mut active, &mut threads, options.threads);
                if let Some(searcher) = coach_searcher.as_mut() {
                    searcher.clear();
                }
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
                require_networks(&options);
                if let Some(level) = options.coach_level {
                    let searcher =
                        coach_searcher.get_or_insert_with(|| Searcher::new(1, COACH_HASH_MB));
                    let clock_key = if board.turn == Color::White {
                        "wtime"
                    } else {
                        "btime"
                    };
                    let chosen = coach_move(&board, level, extract_i64(rest, clock_key), searcher);
                    send(&format!(
                        "bestmove {}",
                        chosen.map_or_else(|| "0000".to_string(), format_uci)
                    ));
                    continue;
                }
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
            "eval" => {
                require_networks(&options);
                print_eval(&mut board);
            }
            "quit" => break,
            _ => {}
        }
    }
    stop_search(&mut active, &mut threads, options.threads);
}
