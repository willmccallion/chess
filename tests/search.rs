//! Search results on positions whose answer is known.

use chess::analysis::{SearchLimit, Searcher};
use chess::board::Board;
use chess::eval::EvalOptions;
use chess::search::{
    IterationInfo, Score, SearchControl, SearchLimits, SearchResult, SearchThreads,
    search_reporting,
};
use chess::search_params::SearchParams;
use chess::tt::SharedTransTable;
use chess::types::START_FEN;
use chess::uci_io::{format_uci, parse_uci_move};
use std::sync::Mutex;

fn load_stockfish_nets() {
    EvalOptions::built_in()
        .load()
        .expect("Stockfish networks (run inside the dev shell)");
}

fn board(fen: &str) -> Board {
    Board::from_fen(fen).expect("valid FEN")
}

fn fixed_depth_search(board: &Board, depth: usize, threads: usize) -> SearchResult {
    fixed_depth_search_reporting(board, depth, threads, &|_| {})
}

fn fixed_depth_search_reporting(
    board: &Board,
    depth: usize,
    threads: usize,
    report: &(dyn Fn(&IterationInfo) + Sync),
) -> SearchResult {
    let tt = SharedTransTable::new(16);
    let limits = SearchLimits {
        max_depth: depth,
        params: SearchParams::default(),
        wait_for_stop: false,
        nodes: None,
    };
    let control = SearchControl::new(None);
    search_reporting(
        board,
        &tt,
        limits,
        &mut SearchThreads::new(threads),
        &control,
        report,
    )
}

#[test]
fn plays_mate_on_the_hundredth_half_move() {
    load_stockfish_nets();
    let fen = "7k/8/6K1/8/8/8/8/R7 w - - 99 80";

    let result = fixed_depth_search(&board(fen), 6, 1);

    assert_eq!(
        (result.best_move.map(format_uci), result.score),
        (Some("a1a8".to_string()), Some(Score::MateIn(1)))
    );
}

#[test]
fn a_quiet_move_on_the_hundredth_half_move_draws() {
    load_stockfish_nets();
    let fen = "7k/8/8/8/8/8/6K1/R7 w - - 99 80";

    let result = fixed_depth_search(&board(fen), 6, 1);

    assert_eq!(result.score, Some(Score::Centipawns(0)));
}

#[test]
fn searching_a_stateless_board_matches_searching_a_stateful_one() {
    load_stockfish_nets();
    let in_check = "rnbqkbnr/ppp2ppp/8/1B1pp3/4P3/8/PPPP1PPP/RNBQK1NR b KQkq - 1 3";
    let stateful = board(in_check);
    let stateless = stateful.without_eval_state();

    let with_state = fixed_depth_search(&stateful, 8, 1);
    let without_state = fixed_depth_search(&stateless, 8, 1);

    assert_eq!(
        (
            without_state.nodes,
            without_state.score,
            without_state.best_move.map(format_uci)
        ),
        (
            with_state.nodes,
            with_state.score,
            with_state.best_move.map(format_uci)
        ),
        "stateless (left) vs stateful (right) search at depth 8"
    );
}

#[test]
fn reviewing_stateless_game_positions_matches_reviewing_stateful_ones() {
    load_stockfish_nets();
    let start = "r1bqkb1r/pppp1ppp/2n2n2/4p3/2B1P3/5N2/PPPP1PPP/RNBQK2R w KQkq - 4 4";
    let game = ["e1g1", "f8c5", "d2d3", "d7d6", "c2c3", "c8g4"];
    let review = |first: Board| {
        let mut positions = vec![first];
        for uci in game {
            let mut next = positions.last().expect("non-empty").clone();
            let m = parse_uci_move(&mut next, uci).expect("legal game move");
            next.make_move(m);
            positions.push(next);
        }
        let mut searcher = Searcher::new(1, 16);
        positions
            .iter()
            .map(|b| {
                let control = SearchControl::new(None);
                let outcome = searcher.search(b, SearchLimit::Nodes(20_000), &control, &|_| {});
                (outcome.score, outcome.best_move.map(format_uci))
            })
            .collect::<Vec<_>>()
    };

    let stateful = review(board(start));
    let stateless = review(board(start).without_eval_state());

    assert_eq!(
        stateless, stateful,
        "stateless (left) vs stateful (right) review of each position"
    );
}

#[test]
fn reported_nodes_include_helper_threads() {
    load_stockfish_nets();
    let last_reported = Mutex::new(0u64);
    let record = |info: &IterationInfo| *last_reported.lock().unwrap() = info.nodes;

    let result = fixed_depth_search_reporting(&board(START_FEN), 12, 4, &record);

    let reported = *last_reported.lock().unwrap();
    assert!(
        reported * 4 >= result.nodes * 3,
        "last info nodes {reported} vs {} searched",
        result.nodes
    );
}
