//! The engine must index its network's inputs exactly as bullet did when training it.

use bullet::game::formats::bulletformat::ChessBoard;
use bullet::game::inputs::{Chess768, SparseInputType};
use chess::board::Board;
use chess::eval::own::active_features;
use chess::types::MoveList;

/// Bullet's (side to move, other side) features for `board`, read the way training reads them.
fn bullet_features(board: &Board) -> (Vec<usize>, Vec<usize>) {
    let line = format!("{} | 0 | 0.5", board.to_fen());
    let position: ChessBoard = line.parse().unwrap();
    let (mut stm, mut ntm) = (Vec::new(), Vec::new());
    Chess768.map_features(&position, |us, them| {
        stm.push(us);
        ntm.push(them);
    });
    stm.sort_unstable();
    ntm.sort_unstable();
    (stm, ntm)
}

fn engine_features(board: &Board) -> (Vec<usize>, Vec<usize>) {
    let sorted = |perspective| {
        let mut features: Vec<usize> =
            active_features(board.position_view(), perspective).collect();
        features.sort_unstable();
        features
    };
    (sorted(board.turn), sorted(board.turn.other()))
}

#[test]
fn engine_features_match_bullets_after_every_kind_of_move() {
    let starts = [
        chess::types::START_FEN,
        "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
        "r3k2r/Pppp1ppp/1b3nbN/nP6/BBP1P3/q4N2/Pp1P2PP/R2Q1RK1 w kq - 0 1",
        "rnbq1k1r/pp1Pbppp/2p5/8/2B5/8/PPP1NnPP/RNBQK2R w KQ - 1 8",
        "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1",
        "rnbqkbnr/ppp1p1pp/8/3pPp2/8/8/PPPP1PPP/RNBQKBNR w KQkq f6 0 3",
    ];
    for fen in starts {
        let mut board = Board::from_fen(fen).unwrap();
        let mut moves = MoveList::new();
        board.generate_legal_moves(&mut moves);

        assert_eq!(engine_features(&board), bullet_features(&board), "{fen}");
        for i in 0..moves.len() {
            let m = moves.get(i);
            let undo = board.make_move(m);
            assert_eq!(
                engine_features(&board),
                bullet_features(&board),
                "{}",
                board.to_fen()
            );
            board.unmake_move(m, undo);
        }
    }
}
