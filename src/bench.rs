//! Fixed-depth search over a set of positions; the node total is a search signature.

use crate::board::Board;
use crate::search::{SearchControl, SearchLimits, SearchThreads, search};
use crate::tt::SharedTransTable;
use std::time::Instant;

pub const BENCH_HASH_MB: usize = 16;

pub const BENCH_FENS: [&str; 30] = [
    "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1",
    "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 10",
    "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 11",
    "4rrk1/pp1n3p/3q2pQ/2p1pb2/2PP4/2P3N1/P2B2PP/4RRK1 b - - 7 19",
    "rq3rk1/ppp2ppp/1bnpb3/3N2B1/3NP3/7P/PPPQ1PP1/2KR3R w - - 7 14",
    "r1bq1r1k/1pp1n1pp/1p1p4/4p2Q/4Pp2/1BNP4/PPP2PPP/3R1RK1 w - - 2 14",
    "r3r1k1/2p2ppp/p1p1bn2/8/1q2P3/2NPQN2/PPP3PP/R4RK1 b - - 2 15",
    "r1bbk1nr/pp3p1p/2n5/1N4p1/2Np1B2/8/PPP2PPP/2KR1B1R w kq - 0 13",
    "r1bq1rk1/ppp1nppp/4n3/3p3Q/3P4/1BP1B3/PP1N2PP/R4RK1 w - - 1 16",
    "4r1k1/r1q2ppp/ppp2n2/4P3/5Rb1/1N1BQ3/PPP3PP/R5K1 w - - 1 17",
    "2rqkb1r/ppp2p2/2npb1p1/1N1Nn2p/2P1PP2/8/PP2B1PP/R1BQK2R b KQ - 0 11",
    "r1bq1r1k/b1p1npp1/p2p3p/1p6/3PP3/1B2NN2/PP3PPP/R2Q1RK1 w - - 1 16",
    "3r1rk1/p5pp/bpp1pp2/8/q1PP1P2/b3P3/P2NQRPP/1R2B1K1 b - - 6 22",
    "r1q2rk1/2p1bppp/2Pp4/p6b/Q1PNp3/4B3/PP1R1PPP/2K4R w - - 2 18",
    "4k2r/1pb2ppp/1p2p3/1R1p4/3P4/2r1PN2/P4PPP/1R4K1 b - - 3 22",
    "3q2k1/pb3p1p/4pbp1/2r5/PpN2N2/1P2P2P/5PP1/Q2R2K1 b - - 4 26",
    "6k1/6p1/6Pp/ppp5/3pn2P/1P3K2/1PP2P2/3N4 b - - 0 1",
    "3b4/5kp1/1p1p1p1p/pP1PpP1P/P1P1P3/3KN3/8/8 w - - 0 1",
    "2K5/p7/7P/5pR1/8/5k2/r7/8 w - - 0 1",
    "8/6pk/1p6/8/PP3p1p/5P2/4KP1q/3Q4 w - - 0 1",
    "7k/3p2pp/4q3/8/4Q3/5Kp1/P6b/8 w - - 0 1",
    "8/2p5/8/2kPKp1p/2p4P/2P5/3P4/8 w - - 0 1",
    "8/1p3pp1/7p/5P1P/2k3P1/8/2K2P2/8 w - - 0 1",
    "8/pp2r1k1/2p1p3/3pP2p/1P1P1P1P/P5KR/8/8 w - - 0 1",
    "8/3p4/p1bk3p/Pp6/1Kp1PpPp/2P2P1P/2P5/5B2 b - - 0 1",
    "5k2/7R/4P2p/5K2/p1r2P1p/8/8/8 b - - 0 1",
    "6k1/6p1/P6p/r1N5/5p2/7P/1b3PP1/4R1K1 w - - 0 1",
    "1r3k2/4q3/2Pp3b/3Bp3/2Q2p2/1p1P2P1/1P2KP2/3N4 w - - 0 1",
    "6k1/4pp1p/3p2p1/P1pPb3/R7/1r2P1PP/3B1P2/6K1 w - - 0 1",
    "8/3p3B/5p2/5P2/p7/PP5b/k7/6K1 w - - 0 1",
];

pub struct BenchResult {
    pub nodes: u64,
    pub elapsed_ms: u128,
}

impl BenchResult {
    pub fn nps(&self) -> u64 {
        if self.elapsed_ms == 0 {
            return 0;
        }
        (u128::from(self.nodes) * 1000 / self.elapsed_ms) as u64
    }
}

/// Searches every bench position to `depth` from a cleared TT and fresh search state.
pub fn run(depth: usize, hash_mb: usize) -> Result<BenchResult, String> {
    let tt = SharedTransTable::new(hash_mb);
    let limits = SearchLimits {
        max_depth: depth,
        wait_for_stop: false,
    };
    let mut nodes = 0u64;
    let start = Instant::now();

    for fen in BENCH_FENS {
        let board = Board::from_fen(fen)?;
        tt.clear();
        let control = SearchControl::new(None);
        let mut threads = SearchThreads::new(1);
        nodes += search(&board, &tt, limits, &mut threads, &control).nodes;
    }

    Ok(BenchResult {
        nodes,
        elapsed_ms: start.elapsed().as_millis(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Color, Piece, PieceKind};

    #[test]
    fn bench_positions_are_legal() {
        for fen in BENCH_FENS {
            let board = Board::from_fen(fen).unwrap();
            for color in [Color::White, Color::Black] {
                let kings = board.piece_bb[Piece::from_kind(PieceKind::King, color).index()];
                assert_eq!(kings.count_ones(), 1, "king count for {fen}");
            }
            let opponent = board.turn.other();
            let opponent_king = board.king_square(opponent) as i32;
            assert!(
                !board.is_square_attacked(opponent_king, board.turn),
                "side not to move is in check: {fen}"
            );
        }
    }

    #[test]
    fn bench_node_count_is_deterministic() {
        crate::eval::ensure_built_in_loaded().unwrap();

        let first = run(5, BENCH_HASH_MB).unwrap();
        let second = run(5, BENCH_HASH_MB).unwrap();

        assert!(first.nodes > 0);
        assert_eq!(first.nodes, second.nodes);
    }
}
