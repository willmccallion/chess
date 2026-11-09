use crate::analysis::Searcher;
use crate::board::Board;
use crate::coach::bot::{Level, choose_move, pick_weighted};
use crate::eval::{self, EvalOptions};
use crate::opening_book::{get_book_move, legal_book_moves};
use crate::search::{NodeLimit, SearchControl, SearchLimits, SearchThreads, search};
use crate::search_params::SearchParams;
use crate::time::{TimeBudget, TimeControl};
use crate::tt::SharedTransTable;
use crate::types::{Color, Move, MoveList, START_FEN};
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
    const VALUE: &str = " value ";
    if let Some(idx) = after.to_ascii_lowercase().find(VALUE) {
        let (n, v) = after.split_at(idx);
        Some((n.trim().to_string(), v[VALUE.len()..].trim().to_string()))
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

/// A `go` the GUI has not yet been answered for.
enum ActiveGo {
    Searching(ActiveSearch),
    /// A book or coach move, chosen at once but held back because the GUI
    /// asked to ponder or to analyse until `stop`.
    Holding(Move),
}

fn send_bestmove(m: Option<Move>) {
    send(&format!(
        "bestmove {}",
        m.map_or_else(|| "0000".to_string(), format_uci)
    ));
}

/// Answers any `go` in progress: stops its search, which returns its thread
/// state to `threads`, or sends the move it held.
fn stop_search(active: &mut Option<ActiveGo>, threads: &mut Option<SearchThreads>, count: usize) {
    match active.take() {
        Some(ActiveGo::Searching(search)) => {
            search.control.stop();
            *threads = search.handle.join().ok();
        }
        Some(ActiveGo::Holding(m)) => send_bestmove(Some(m)),
        None => {}
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

impl Options {
    /// The options an engine starts with, evaluating with `eval`.
    fn new(eval: EvalOptions) -> Self {
        Self {
            coach_level: None,
            hash_mb: DEFAULT_HASH_MB,
            threads: DEFAULT_THREADS,
            own_book: true,
            move_overhead_ms: DEFAULT_MOVE_OVERHEAD_MS,
            eval,
            search_params: SearchParams::default(),
        }
    }
}

/// Makes the evaluator the options name the one in use and returns whether
/// it replaced another. A failure is reported and keeps the one in use, or
/// exits when there is none: the engine cannot play without an evaluator.
fn load_evaluator(options: &Options) -> bool {
    match options.eval.load() {
        Ok(replaced) => replaced,
        Err(e) if eval::is_loaded() => {
            send(&format!(
                "info string ERROR: {e}; keeping the evaluator in use"
            ));
            false
        }
        Err(e) => {
            send(&format!("info string ERROR: {e}"));
            std::process::exit(1);
        }
    }
}

/// What the engine has learnt during a game: the hash tables and histories.
struct SearchState<'s> {
    tt: &'s SharedTransTable,
    threads: &'s mut Option<SearchThreads>,
    coach_searcher: &'s mut Option<Searcher>,
}

impl SearchState<'_> {
    fn clear(self) {
        self.tt.clear();
        if let Some(threads) = self.threads.as_mut() {
            threads.clear();
        }
        if let Some(searcher) = self.coach_searcher.as_mut() {
            searcher.clear();
        }
    }
}

/// Forgets what the replaced evaluator computed: evaluations cached in the
/// search state and the board's incremental state.
fn forget_evaluations(board: &mut Board, state: SearchState<'_>) {
    state.clear();
    *board = board.without_eval_state();
}

/// Loads the options' evaluator, forgetting the old one's work if it was
/// replaced.
fn ensure_evaluator(options: &Options, board: &mut Board, state: SearchState<'_>) {
    if load_evaluator(options) {
        forget_evaluations(board, state);
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

/// The position a `position` command sets, or why it names none.
fn parse_position(rest: &str) -> Result<Board, String> {
    let parts: Vec<&str> = rest.split_whitespace().collect();
    let moves_idx = parts.iter().position(|&s| s.eq_ignore_ascii_case("moves"));

    let mut board = match parts.first() {
        Some(&"fen") => {
            let fen_end = moves_idx.unwrap_or(parts.len());
            Board::from_fen(&parts[1..fen_end].join(" "))
                .map_err(|e| format!("invalid FEN: {e}"))?
        }
        Some(&"startpos") => Board::from_fen(START_FEN)?,
        _ => return Err("expected `startpos` or `fen`".to_string()),
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
    Ok(board)
}

/// The words of a `go` command other than `searchmoves`, which end its list.
const GO_KEYWORDS: [&str; 11] = [
    "ponder",
    "wtime",
    "btime",
    "winc",
    "binc",
    "movestogo",
    "depth",
    "nodes",
    "mate",
    "movetime",
    "infinite",
];

/// The legal moves listed after `searchmoves`, or `None` when `go` lists
/// none, or none of them is legal in `board`.
fn search_moves(go_args: &str, board: &Board) -> Option<MoveList> {
    let listed = go_args
        .split_whitespace()
        .skip_while(|w| !w.eq_ignore_ascii_case("searchmoves"))
        .skip(1)
        .take_while(|w| !GO_KEYWORDS.iter().any(|k| w.eq_ignore_ascii_case(k)));
    let mut board = board.clone();
    let mut moves = MoveList::new();
    for word in listed {
        match parse_uci_move(&mut board, word) {
            Some(m) => moves.push(m),
            None => send(&format!("info string ignoring searchmove {word}")),
        }
    }
    (!moves.is_empty()).then_some(moves)
}

/// The time a `go` command gives the side to move, if any: its `movetime`,
/// or a share of its clock.
fn time_budget(go_args: &str, turn: Color, move_overhead_ms: i64) -> Option<TimeBudget> {
    if let Some(movetime) = extract_i64(go_args, "movetime") {
        let usable = (movetime - move_overhead_ms).max(1);
        return Some(TimeBudget::fixed(usable as u64));
    }
    let tc = TimeControl {
        wtime: extract_i64(go_args, "wtime").unwrap_or(0),
        btime: extract_i64(go_args, "btime").unwrap_or(0),
        winc: extract_i64(go_args, "winc").unwrap_or(0),
        binc: extract_i64(go_args, "binc").unwrap_or(0),
        movestogo: extract_i64(go_args, "movestogo").unwrap_or(0) as i32,
        move_overhead_ms,
    };
    // A clock at or below zero still bounds the search: it moves at once.
    let clock_key = if turn == Color::White {
        "wtime"
    } else {
        "btime"
    };
    extract_i64(go_args, clock_key).map(|_| tc.budget(turn))
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

    let budget = time_budget(go_args, board.turn, options.move_overhead_ms);

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
        root_moves: search_moves(go_args, board),
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

/// Sends `chosen` as the answer to `go_args`, or holds it in `active` until
/// `stop` or `ponderhit` when the GUI asked to ponder or analyse.
fn answer_now_or_hold(chosen: Option<Move>, go_args: &str, active: &mut Option<ActiveGo>) {
    let waits_for_stop = has_token(go_args, "ponder") || has_token(go_args, "infinite");
    match chosen {
        Some(m) if waits_for_stop => *active = Some(ActiveGo::Holding(m)),
        _ => send_bestmove(chosen),
    }
}

fn print_eval(board: &mut Board) {
    send(&format!(
        "info string eval: {}",
        eval::describe_evaluation(board)
    ));
    send(&format!("info string fen: {}", board.to_fen()));
}

pub fn run_uci(eval: EvalOptions) {
    let mut board = Board::from_fen(START_FEN).expect("valid startpos");
    let mut options = Options::new(eval);
    let mut tt = SharedTransTable::new(options.hash_mb);
    let mut active: Option<ActiveGo> = None;
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
                let state = SearchState {
                    tt: &tt,
                    threads: &mut threads,
                    coach_searcher: &mut coach_searcher,
                };
                ensure_evaluator(&options, &mut board, state);
                send("readyok");
            }
            "ucinewgame" => {
                stop_search(&mut active, &mut threads, options.threads);
                SearchState {
                    tt: &tt,
                    threads: &mut threads,
                    coach_searcher: &mut coach_searcher,
                }
                .clear();
            }
            "setoption" => {
                stop_search(&mut active, &mut threads, options.threads);
                if let Some((name, value)) = parse_setoption(rest) {
                    apply_option(&name, &value, &mut options, &mut tt);
                }
            }
            "position" => {
                stop_search(&mut active, &mut threads, options.threads);
                match parse_position(rest) {
                    Ok(position) => board = position,
                    Err(e) => send(&format!("info string ignoring position: {e}")),
                }
            }
            "go" => {
                stop_search(&mut active, &mut threads, options.threads);
                let state = SearchState {
                    tt: &tt,
                    threads: &mut threads,
                    coach_searcher: &mut coach_searcher,
                };
                ensure_evaluator(&options, &mut board, state);
                if let Some(level) = options.coach_level {
                    let searcher =
                        coach_searcher.get_or_insert_with(|| Searcher::new(1, COACH_HASH_MB));
                    let clock_key = if board.turn == Color::White {
                        "wtime"
                    } else {
                        "btime"
                    };
                    let chosen = coach_move(&board, level, extract_i64(rest, clock_key), searcher);
                    answer_now_or_hold(chosen, rest, &mut active);
                    continue;
                }
                if options.own_book
                    && !has_token(rest, "infinite")
                    && let Some(book_move) = get_book_move(&board)
                {
                    answer_now_or_hold(Some(book_move), rest, &mut active);
                    continue;
                }
                let search_threads = threads
                    .take()
                    .unwrap_or_else(|| SearchThreads::new(options.threads));
                let search = start_search(&board, &tt, &options, search_threads, rest);
                active = Some(ActiveGo::Searching(search));
            }
            "ponderhit" => match &active {
                Some(ActiveGo::Searching(search)) => match search.ponder_budget {
                    Some(budget) => search.control.set_budget_from_now(budget),
                    None => search.control.stop(),
                },
                Some(ActiveGo::Holding(_)) => {
                    stop_search(&mut active, &mut threads, options.threads);
                }
                None => {}
            },
            "stop" => stop_search(&mut active, &mut threads, options.threads),
            "eval" => {
                let state = SearchState {
                    tt: &tt,
                    threads: &mut threads,
                    coach_searcher: &mut coach_searcher,
                };
                ensure_evaluator(&options, &mut board, state);
                print_eval(&mut board);
            }
            "quit" => break,
            _ => {}
        }
    }
    stop_search(&mut active, &mut threads, options.threads);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zero_clock_with_increment_still_times_the_search_within_the_increment() {
        let budget = time_budget("wtime 0 btime 0 winc 100 binc 100", Color::White, 10);

        assert!(
            budget.is_some_and(|b| b.soft_ms >= 1 && b.hard_ms < 100),
            "{budget:?}"
        );
    }

    #[test]
    fn a_negative_clock_still_times_the_search_and_moves_at_once() {
        let budget = time_budget("wtime -50 btime -50", Color::White, 10);

        assert_eq!(budget, Some(TimeBudget::fixed(1)));
    }

    #[test]
    fn a_go_without_clocks_is_untimed() {
        assert_eq!(time_budget("depth 10", Color::White, 10), None);
    }

    fn listed_search_moves(go_args: &str) -> Option<Vec<String>> {
        let board = Board::from_fen(START_FEN).unwrap();
        search_moves(go_args, &board)
            .map(|ms| ms.as_slice().iter().map(|&m| format_uci(m)).collect())
    }

    #[test]
    fn searchmoves_lists_its_moves_up_to_the_next_go_keyword() {
        let listed = listed_search_moves("searchmoves e2e4 d2d4 depth 6");

        assert_eq!(listed, Some(vec!["e2e4".to_string(), "d2d4".to_string()]));
    }

    #[test]
    fn searchmoves_skips_moves_that_are_not_legal() {
        assert_eq!(
            listed_search_moves("searchmoves e2e5 g1f3"),
            Some(vec!["g1f3".to_string()])
        );
    }

    #[test]
    fn a_go_without_searchmoves_allows_every_root_move() {
        assert_eq!(listed_search_moves("depth 6"), None);
    }

    #[test]
    fn setoption_reads_names_with_spaces_and_empty_values() {
        assert_eq!(
            parse_setoption(" name Move Overhead value 30"),
            Some(("Move Overhead".to_string(), "30".to_string()))
        );
        assert_eq!(
            parse_setoption(" name Ponder"),
            Some(("Ponder".to_string(), String::new()))
        );
        assert_eq!(parse_setoption(" Hash 16"), None);
    }

    #[test]
    fn setoption_takes_the_value_keyword_in_any_case() {
        assert_eq!(
            parse_setoption(" name Threads VALUE 4"),
            Some(("Threads".to_string(), "4".to_string()))
        );
    }

    #[test]
    fn a_position_needs_startpos_or_a_valid_fen() {
        assert!(parse_position("").is_err());
        assert!(parse_position("moves e2e4").is_err());
        assert!(parse_position("fen not a fen").is_err());
    }

    #[test]
    fn a_position_stops_at_an_illegal_move_and_keeps_the_moves_before_it() {
        let board = parse_position("startpos moves e2e4 e7e9 d2d4").unwrap();

        let mut expected = Board::from_fen(START_FEN).unwrap();
        let e4 = parse_uci_move(&mut expected, "e2e4").unwrap();
        expected.make_move(e4);
        assert_eq!(board.to_fen(), expected.to_fen());
    }

    #[test]
    fn hash_and_threads_outside_their_ranges_are_ignored() {
        let mut options = Options::new(EvalOptions::built_in());
        let mut tt = SharedTransTable::new(1);

        for (name, value) in [
            ("Hash", "0"),
            ("Hash", "999999"),
            ("Threads", "0"),
            ("Threads", "100000"),
        ] {
            apply_option(name, value, &mut options, &mut tt);
        }

        assert_eq!(
            (options.hash_mb, options.threads),
            (DEFAULT_HASH_MB, DEFAULT_THREADS)
        );
    }

    #[test]
    fn move_overhead_is_clamped_to_its_range() {
        let mut options = Options::new(EvalOptions::built_in());
        let mut tt = SharedTransTable::new(1);

        apply_option("Move Overhead", "-5", &mut options, &mut tt);
        let low = options.move_overhead_ms;
        apply_option("Move Overhead", "999999", &mut options, &mut tt);

        assert_eq!((low, options.move_overhead_ms), (0, MAX_MOVE_OVERHEAD_MS));
    }

    #[test]
    fn movetime_gives_its_time_less_the_overhead() {
        let budget = time_budget("movetime 500", Color::Black, 10);

        assert_eq!(budget, Some(TimeBudget::fixed(490)));
    }
}
