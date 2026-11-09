//! Static evaluation through the board's incremental state.

use chess::board::Board;
use chess::eval::{EvalOptions, evaluate};
use chess::types::START_FEN;
use chess::uci_io::parse_uci_move;

fn load_stockfish_nets() {
    EvalOptions::built_in()
        .load()
        .expect("Stockfish networks (run inside the dev shell)");
}

fn board(fen: &str) -> Board {
    Board::from_fen(fen).expect("valid FEN")
}

#[test]
fn a_stateless_board_evaluates_the_same_after_make_and_unmake() {
    load_stockfish_nets();
    let fresh = evaluate(&mut board(START_FEN));
    let mut stateless = board(START_FEN).without_eval_state();
    let e2e4 = parse_uci_move(&stateless, "e2e4").expect("legal");

    let undo = stateless.make_move(e2e4);
    evaluate(&mut stateless);
    stateless.unmake_move(e2e4, undo);
    let after_round_trip = evaluate(&mut stateless);

    assert_eq!(after_round_trip, fresh, "startpos eval after e2e4/unmake");
}
