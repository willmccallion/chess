use crate::analysis::Searcher;
use crate::board::Board;
use crate::coach::bot::{Level, choose_move, pick_weighted};
use crate::eval::{self, EvalOptions};
use crate::opening_book::{get_book_move, legal_book_moves};
use crate::search::{NodeLimit, SearchControl, SearchLimits, SearchThreads, search};
use crate::search_params::SearchParams;
use crate::time::{TimeBudget, TimeControl};
use crate::tt::SharedTransTable;
use crate::types::{Color, Move, MoveList};
use crate::uci_io::{format_uci, parse_uci_move};
use std::io::{self, BufRead, Write};
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

fn parse_setoption(rest: &str) -> Option<(String, String)> {
    const VALUE: &str = " value ";
    let parts: Vec<&str> = rest.splitn(2, "name").collect();
    let after = parts.get(1)?.trim();
    if let Some(idx) = after.to_ascii_lowercase().find(VALUE) {
        let (n, v) = after.split_at(idx);
        Some((n.trim().to_string(), v[VALUE.len()..].trim().to_string()))
    } else {
        Some((after.trim().to_string(), String::new()))
    }
}

fn parse_check_value(value: &str) -> Result<bool, String> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "1" | "on" => Ok(true),
        "false" | "0" | "off" => Ok(false),
        _ => Err(format!("expected true or false, not '{value}'")),
    }
}

/// `value` as a number in `range`.
fn parse_in_range<T>(value: &str, range: std::ops::RangeInclusive<T>) -> Result<T, String>
where
    T: std::str::FromStr + PartialOrd + std::fmt::Display,
{
    match value.parse::<T>() {
        Ok(number) if range.contains(&number) => Ok(number),
        _ => Err(format!(
            "expected a number from {} to {}, not '{value}'",
            range.start(),
            range.end()
        )),
    }
}

fn parse_number<T: std::str::FromStr>(value: &str) -> Result<T, String> {
    value
        .parse()
        .map_err(|_| format!("expected a number, not '{value}'"))
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

fn send_bestmove(best: Option<Move>, ponder: Option<Move>) {
    let line = match (best, ponder) {
        (Some(best), Some(ponder)) => {
            format!(
                "bestmove {} ponder {}",
                format_uci(best),
                format_uci(ponder)
            )
        }
        (Some(best), None) => format!("bestmove {}", format_uci(best)),
        (None, _) => "bestmove 0000".to_string(),
    };
    send(&line);
}

/// Answers any `go` in progress: stops its search, which returns its thread
/// state to `threads`, or sends the move it held.
fn stop_search(active: &mut Option<ActiveGo>, threads: &mut Option<SearchThreads>, count: usize) {
    match active.take() {
        Some(ActiveGo::Searching(search)) => {
            search.control.stop();
            *threads = if let Ok(returned) = search.handle.join() {
                Some(returned)
            } else {
                // It died before answering, so the GUI still waits for a move.
                send("info string ERROR: the search thread panicked");
                send_bestmove(None, None);
                None
            };
        }
        Some(ActiveGo::Holding(m)) => send_bestmove(Some(m), None),
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
/// it replaced another. A failure is reported and keeps the one in use, or is
/// an error when there is none: the engine cannot play without an evaluator.
fn load_evaluator(options: &Options) -> Result<bool, String> {
    match options.eval.load() {
        Ok(replaced) => Ok(replaced),
        Err(e) if eval::is_loaded() => {
            send(&format!(
                "info string ERROR: {e}; keeping the evaluator in use"
            ));
            Ok(false)
        }
        Err(e) => {
            send(&format!("info string ERROR: {e}"));
            Err(format!("no evaluator to play with: {e}"))
        }
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
        Some(&"startpos") => Board::start(),
        _ => return Err("expected `startpos` or `fen`".to_string()),
    };

    if let Some(idx) = moves_idx {
        for move_str in &parts[idx + 1..] {
            let Some(mv) = parse_uci_move(&board, move_str) else {
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

/// A `go` command's arguments. A number is the first one given for its
/// keyword that reads as one.
#[derive(Debug, Default, PartialEq, Eq)]
struct GoCommand {
    ponder: bool,
    infinite: bool,
    wtime: Option<i64>,
    btime: Option<i64>,
    winc: Option<i64>,
    binc: Option<i64>,
    movestogo: Option<i64>,
    depth: Option<i64>,
    nodes: Option<i64>,
    movetime: Option<i64>,
    /// The words after `searchmoves`, up to the next keyword.
    searchmoves: Vec<String>,
}

impl GoCommand {
    fn parse(args: &str) -> Self {
        let words: Vec<&str> = args.split_whitespace().collect();
        let mut go = Self::default();
        for (i, word) in words.iter().enumerate() {
            if word.eq_ignore_ascii_case("ponder") {
                go.ponder = true;
            } else if word.eq_ignore_ascii_case("infinite") {
                go.infinite = true;
            } else if let Some(slot) = go.number_for(word)
                && slot.is_none()
            {
                *slot = words.get(i + 1).and_then(|value| value.parse().ok());
            }
        }
        go.searchmoves = words
            .iter()
            .skip_while(|w| !w.eq_ignore_ascii_case("searchmoves"))
            .skip(1)
            .take_while(|w| !GO_KEYWORDS.iter().any(|k| w.eq_ignore_ascii_case(k)))
            .map(|w| (*w).to_string())
            .collect();
        go
    }

    fn number_for(&mut self, keyword: &str) -> Option<&mut Option<i64>> {
        match keyword.to_ascii_lowercase().as_str() {
            "wtime" => Some(&mut self.wtime),
            "btime" => Some(&mut self.btime),
            "winc" => Some(&mut self.winc),
            "binc" => Some(&mut self.binc),
            "movestogo" => Some(&mut self.movestogo),
            "depth" => Some(&mut self.depth),
            "nodes" => Some(&mut self.nodes),
            "movetime" => Some(&mut self.movetime),
            _ => None,
        }
    }

    /// Whether the answer waits for `stop` or `ponderhit`.
    const fn waits_for_stop(&self) -> bool {
        self.ponder || self.infinite
    }

    /// The time left on the clock of the side to move, `turn`.
    const fn clock(&self, turn: Color) -> Option<i64> {
        match turn {
            Color::White => self.wtime,
            Color::Black => self.btime,
        }
    }
}

/// The legal moves `go` lists after `searchmoves`, or `None` when it lists
/// none, or none of them is legal in `board`.
fn search_moves(go: &GoCommand, board: &Board) -> Option<MoveList> {
    let mut moves = MoveList::new();
    for word in &go.searchmoves {
        match parse_uci_move(board, word) {
            Some(m) => moves.push(m),
            None => send(&format!("info string ignoring searchmove {word}")),
        }
    }
    (!moves.is_empty()).then_some(moves)
}

/// The time `go` gives the side to move, if any: its `movetime`, or a share
/// of its clock.
fn time_budget(go: &GoCommand, turn: Color, move_overhead_ms: i64) -> Option<TimeBudget> {
    if let Some(movetime) = go.movetime {
        let usable = (movetime - move_overhead_ms).max(1);
        return Some(TimeBudget::fixed(usable as u64));
    }
    let tc = TimeControl {
        wtime: go.wtime.unwrap_or(0),
        btime: go.btime.unwrap_or(0),
        winc: go.winc.unwrap_or(0),
        binc: go.binc.unwrap_or(0),
        movestogo: go.movestogo.unwrap_or(0) as i32,
        move_overhead_ms,
    };
    // A clock at or below zero still bounds the search: it moves at once.
    go.clock(turn).map(|_| tc.budget(turn))
}

fn start_search(
    board: &Board,
    tt: &SharedTransTable,
    options: &Options,
    mut threads: SearchThreads,
    go: &GoCommand,
) -> ActiveSearch {
    let max_depth = go
        .depth
        .map_or(MAX_DEPTH, |d| (d.max(1) as usize).min(MAX_DEPTH));
    let budget = time_budget(go, board.turn(), options.move_overhead_ms);

    let (control, ponder_budget) = if go.ponder {
        (SearchControl::new(None), budget)
    } else if go.infinite {
        (SearchControl::new(None), None)
    } else {
        (SearchControl::new(budget), None)
    };

    let limits = SearchLimits {
        params: options.search_params,
        max_depth,
        wait_for_stop: go.waits_for_stop(),
        nodes: go.nodes.map(|n| {
            let n = n.max(1) as u64;
            NodeLimit { soft: n, hard: n }
        }),
        root_moves: search_moves(go, board),
    };
    let board = board.clone();
    let tt = tt.clone();
    let worker_control = Arc::clone(&control);

    let handle = thread::spawn(move || {
        let result = search(&board, &tt, &limits, &mut threads, &worker_control);
        send_bestmove(result.best_move, result.ponder_move);
        threads
    });

    ActiveSearch {
        handle,
        control,
        ponder_budget,
    }
}

/// Applies `setoption name <name> value <value>`, or says why it cannot;
/// the option then keeps its value.
fn apply_option(
    name: &str,
    value: &str,
    options: &mut Options,
    tt: &mut SharedTransTable,
) -> Result<(), String> {
    if name.eq_ignore_ascii_case("Hash") {
        let size = parse_in_range(value, 1..=MAX_HASH_MB)?;
        if options.hash_mb != size {
            options.hash_mb = size;
            *tt = SharedTransTable::new(size);
        }
    } else if name.eq_ignore_ascii_case("Threads") {
        options.threads = parse_in_range(value, 1..=MAX_THREADS)?;
    } else if name.eq_ignore_ascii_case("OwnBook") {
        options.own_book = parse_check_value(value)?;
    } else if name.eq_ignore_ascii_case("CoachLevel") {
        // Level 0 is the engine itself, with no coach.
        let number = parse_in_range(value, 0..=Level::STRONGEST.number())?;
        options.coach_level = Level::new(number);
    } else if name.eq_ignore_ascii_case("Move Overhead") {
        let ms: i64 = parse_number(value)?;
        options.move_overhead_ms = ms.clamp(0, MAX_MOVE_OVERHEAD_MS);
    } else if !options.eval.set_uci_option(name, value)? {
        // Tunable search parameters are accepted by name but not advertised.
        let number = parse_number(value)?;
        if !options.search_params.set(name, number) {
            return Err("no such option".to_string());
        }
    }
    Ok(())
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
    let ply = (board.fullmove_number().max(1) as usize - 1) * 2
        + usize::from(board.turn() == Color::Black);
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

/// Sends `chosen` as the answer to `go`, or holds it in `active` until
/// `stop` or `ponderhit` when the GUI asked to ponder or analyse.
fn answer_now_or_hold(chosen: Option<Move>, go: &GoCommand, active: &mut Option<ActiveGo>) {
    match chosen {
        Some(m) if go.waits_for_stop() => *active = Some(ActiveGo::Holding(m)),
        _ => send_bestmove(chosen, None),
    }
}

fn print_eval(board: &mut Board) {
    send(&format!(
        "info string eval: {}",
        eval::describe_evaluation(board)
    ));
    send(&format!("info string fen: {}", board.to_fen()));
}

/// Answers UCI commands from stdin until `quit` or the end of input, or until
/// the engine is left with no evaluator.
pub fn run_uci(eval: EvalOptions) -> Result<(), String> {
    let mut session = Session::new(eval);
    let served = session.serve(io::stdin().lock());
    session.stop_search();
    served
}

/// The engine between commands: the position, the options, what it has
/// learnt and the `go` it is answering.
struct Session {
    board: Board,
    options: Options,
    tt: SharedTransTable,
    active: Option<ActiveGo>,
    threads: Option<SearchThreads>,
    coach_searcher: Option<Searcher>,
}

impl Session {
    fn new(eval: EvalOptions) -> Self {
        let options = Options::new(eval);
        Self {
            board: Board::start(),
            tt: SharedTransTable::new(options.hash_mb),
            active: None,
            threads: Some(SearchThreads::new(options.threads)),
            coach_searcher: None,
            options,
        }
    }

    fn serve(&mut self, input: impl BufRead) -> Result<(), String> {
        for line in input.lines() {
            let Ok(line) = line else {
                break;
            };
            let cmd = line.trim();
            let (verb, rest) = cmd.split_once(' ').unwrap_or((cmd, ""));
            if verb == "quit" {
                break;
            }
            self.handle(verb, rest)?;
        }
        Ok(())
    }

    fn handle(&mut self, verb: &str, rest: &str) -> Result<(), String> {
        match verb {
            "uci" => {
                send("id name chess");
                send("id author Will");
                print_options(&self.options);
                send("uciok");
            }
            "isready" => {
                self.ensure_evaluator()?;
                send("readyok");
            }
            "ucinewgame" => {
                self.stop_search();
                self.clear_learning();
            }
            "setoption" => {
                self.stop_search();
                if let Some((name, value)) = parse_setoption(rest)
                    && let Err(reason) =
                        apply_option(&name, &value, &mut self.options, &mut self.tt)
                {
                    send(&format!("info string ignoring option {name}: {reason}"));
                }
            }
            "position" => {
                self.stop_search();
                match parse_position(rest) {
                    Ok(position) => self.board = position,
                    Err(e) => send(&format!("info string ignoring position: {e}")),
                }
            }
            "go" => self.go(&GoCommand::parse(rest))?,
            "ponderhit" => self.ponderhit(),
            "stop" => self.stop_search(),
            "eval" => {
                self.ensure_evaluator()?;
                print_eval(&mut self.board);
            }
            _ => {}
        }
        Ok(())
    }

    /// Answers `go`: with the coach's move, a book move, or a search.
    fn go(&mut self, go: &GoCommand) -> Result<(), String> {
        self.stop_search();
        self.ensure_evaluator()?;
        if let Some(level) = self.options.coach_level {
            let searcher = self
                .coach_searcher
                .get_or_insert_with(|| Searcher::new(1, COACH_HASH_MB));
            let chosen = coach_move(&self.board, level, go.clock(self.board.turn()), searcher);
            answer_now_or_hold(chosen, go, &mut self.active);
            return Ok(());
        }
        if self.options.own_book
            && !go.infinite
            && let Some(book_move) = get_book_move(&self.board)
        {
            answer_now_or_hold(Some(book_move), go, &mut self.active);
            return Ok(());
        }
        let threads = self
            .threads
            .take()
            .unwrap_or_else(|| SearchThreads::new(self.options.threads));
        let search = start_search(&self.board, &self.tt, &self.options, threads, go);
        self.active = Some(ActiveGo::Searching(search));
        Ok(())
    }

    /// Starts the clock of a pondering search, or answers a held move.
    fn ponderhit(&mut self) {
        match &self.active {
            Some(ActiveGo::Searching(search)) => match search.ponder_budget {
                Some(budget) => search.control.set_budget_from_now(budget),
                None => search.control.stop(),
            },
            Some(ActiveGo::Holding(_)) => self.stop_search(),
            None => {}
        }
    }

    fn stop_search(&mut self) {
        stop_search(&mut self.active, &mut self.threads, self.options.threads);
    }

    /// Loads the options' evaluator, forgetting what the one it replaced
    /// computed: evaluations cached by the searches and the board's
    /// incremental state.
    fn ensure_evaluator(&mut self) -> Result<(), String> {
        if load_evaluator(&self.options)? {
            self.clear_learning();
            self.board = self.board.without_eval_state();
        }
        Ok(())
    }

    /// Forgets what the engine learnt during a game: the hash tables and
    /// histories.
    fn clear_learning(&mut self) {
        self.tt.clear();
        if let Some(threads) = self.threads.as_mut() {
            threads.clear();
        }
        if let Some(searcher) = self.coach_searcher.as_mut() {
            searcher.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zero_clock_with_increment_still_times_the_search_within_the_increment() {
        let budget = time_budget(
            &GoCommand::parse("wtime 0 btime 0 winc 100 binc 100"),
            Color::White,
            10,
        );

        assert!(
            budget.is_some_and(|b| b.soft_ms >= 1 && b.hard_ms < 100),
            "{budget:?}"
        );
    }

    #[test]
    fn a_negative_clock_still_times_the_search_and_moves_at_once() {
        let budget = time_budget(&GoCommand::parse("wtime -50 btime -50"), Color::White, 10);

        assert_eq!(budget, Some(TimeBudget::fixed(1)));
    }

    #[test]
    fn a_go_without_clocks_is_untimed() {
        assert_eq!(
            time_budget(&GoCommand::parse("depth 10"), Color::White, 10),
            None
        );
    }

    #[test]
    fn a_go_number_is_the_first_given_for_its_keyword_that_reads_as_one() {
        let go = GoCommand::parse("wtime soon wtime 300 btime 200 wtime 100 ponder");

        assert_eq!(
            (go.wtime, go.btime, go.ponder),
            (Some(300), Some(200), true)
        );
        assert_eq!((go.winc, go.infinite), (None, false));
    }

    #[test]
    fn go_keywords_are_read_in_any_case() {
        let go = GoCommand::parse("WTIME 5 Infinite MoveTime 40");

        assert_eq!(
            (go.wtime, go.movetime, go.infinite),
            (Some(5), Some(40), true)
        );
    }

    fn listed_search_moves(go_args: &str) -> Option<Vec<String>> {
        let board = Board::start();
        search_moves(&GoCommand::parse(go_args), &board)
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

        let mut expected = Board::start();
        let e4 = parse_uci_move(&expected, "e2e4").unwrap();
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
            assert!(apply_option(name, value, &mut options, &mut tt).is_err());
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

        apply_option("Move Overhead", "-5", &mut options, &mut tt).unwrap();
        let low = options.move_overhead_ms;
        apply_option("Move Overhead", "999999", &mut options, &mut tt).unwrap();

        assert_eq!((low, options.move_overhead_ms), (0, MAX_MOVE_OVERHEAD_MS));
    }

    #[test]
    fn a_coach_level_out_of_range_keeps_the_coach_and_zero_turns_it_off() {
        let mut options = Options::new(EvalOptions::built_in());
        let mut tt = SharedTransTable::new(1);
        apply_option("CoachLevel", "3", &mut options, &mut tt).unwrap();

        let too_strong = apply_option("CoachLevel", "200", &mut options, &mut tt);
        let kept = options.coach_level;
        apply_option("CoachLevel", "0", &mut options, &mut tt).unwrap();

        assert!(too_strong.is_err());
        assert_eq!((kept, options.coach_level), (Level::new(3), None));
    }

    #[test]
    fn unknown_options_and_unreadable_values_are_refused() {
        let mut options = Options::new(EvalOptions::built_in());
        let mut tt = SharedTransTable::new(1);

        let refused = [
            ("NoSuchOption", "1"),
            ("OwnBook", "maybe"),
            ("Move Overhead", "soon"),
            ("Evaluator", "magic"),
        ]
        .map(|(name, value)| apply_option(name, value, &mut options, &mut tt).is_err());

        assert_eq!(refused, [true; 4]);
        assert!(options.own_book);
    }

    #[test]
    fn movetime_gives_its_time_less_the_overhead() {
        let budget = time_budget(&GoCommand::parse("movetime 500"), Color::Black, 10);

        assert_eq!(budget, Some(TimeBudget::fixed(490)));
    }
}
