//! Search results on positions whose answer is known.

use chess::board::Board;
use chess::eval::EvalOptions;
use chess::search::{
    IterationInfo, Score, SearchControl, SearchLimits, SearchResult, SearchThreads,
    search_reporting,
};
use chess::search_params::SearchParams;
use chess::tt::SharedTransTable;
use chess::uci_io::format_uci;

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
