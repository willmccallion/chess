//! Helpers shared by the unit tests.

use crate::board::Board;
use crate::types::{MoveList, START_FEN};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

/// Every position of `games` random games from the start, up to `plies`
/// moves each.
pub fn random_game_positions(seed: u64, games: usize, plies: usize) -> Vec<Board> {
    let mut rng = StdRng::seed_from_u64(seed);
    let mut positions = Vec::new();
    for _ in 0..games {
        let mut board = Board::from_fen(START_FEN).unwrap();
        for _ in 0..plies {
            let mut legal = MoveList::new();
            board.generate_legal_moves(&mut legal);
            if legal.is_empty() {
                break;
            }
            board.make_move(legal.get(rng.gen_range(0..legal.len())));
            positions.push(board.clone());
        }
    }
    positions
}
