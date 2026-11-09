use crate::board::Board;
use crate::types::MoveList;

fn perft_inner(b: &mut Board, depth: usize) -> u64 {
    if depth == 0 {
        return 1;
    }

    let mut moves = MoveList::new();
    b.generate_legal_moves(&mut moves);
    if depth == 1 {
        return moves.len() as u64;
    }

    let mut nodes = 0u64;
    for &m in moves.as_slice() {
        let u = b.make_move(m);
        nodes += perft_inner(b, depth - 1);
        b.unmake_move(m, u);
    }
    nodes
}

pub fn perft(b: &mut Board, depth: usize) -> u64 {
    perft_inner(b, depth)
}

pub fn divide(b: &mut Board, depth: usize) {
    let mut moves = MoveList::new();
    b.generate_legal_moves(&mut moves);
    let mut total = 0u64;

    for &m in moves.as_slice() {
        let u = b.make_move(m);
        let n = perft_inner(b, depth - 1);
        b.unmake_move(m, u);
        total += n;

        let from_file = (m.from % 8) + b'a';
        let from_rank = (m.from / 8) + b'1';
        let to_file = (m.to % 8) + b'a';
        let to_rank = (m.to / 8) + b'1';
        println!(
            "{}{}{}{}: {}",
            from_file as char, from_rank as char, to_file as char, to_rank as char, n
        );
    }
    println!("Total: {total}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Move;

    /// Published node counts (chessprogramming.org "Perft Results").
    const SUITE: [(&str, usize, u64); 6] = [
        (
            "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1",
            5,
            4_865_609,
        ),
        (
            "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
            4,
            4_085_603,
        ),
        ("8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1", 5, 674_624),
        (
            "r3k2r/Pppp1ppp/1b3nbN/nP6/BBP1P3/q4N2/Pp1P2PP/R2Q1RK1 w kq - 0 1",
            4,
            422_333,
        ),
        (
            "rnbq1k1r/pp1Pbppp/2p5/8/2B5/8/PPP1NnPP/RNBQK2R w KQ - 1 8",
            4,
            2_103_487,
        ),
        (
            "r4rk1/1pp1qppp/p1np1n2/2b1p1B1/2B1P1b1/P1NP1N2/1PP1QPPP/R4RK1 w - - 0 10",
            4,
            3_894_594,
        ),
    ];

    #[test]
    fn every_move_two_plies_into_the_suite_survives_the_u16_encoding() {
        for (fen, _, _) in SUITE {
            let root = Board::from_fen(fen).unwrap();
            let mut first = MoveList::new();
            root.generate_legal_moves(&mut first);
            for &m in first.as_slice() {
                assert_eq!(Move::from(u16::from(m)), m, "{fen}");
                let mut child = root.clone();
                child.make_move(m);
                let mut second = MoveList::new();
                child.generate_legal_moves(&mut second);
                for &reply in second.as_slice() {
                    assert_eq!(Move::from(u16::from(reply)), reply, "{}", child.to_fen());
                }
            }
        }
    }

    #[test]
    fn perft_matches_published_node_counts() {
        for (fen, depth, expected) in SUITE {
            let mut board = Board::from_fen(fen).unwrap();
            assert_eq!(
                perft(&mut board, depth),
                expected,
                "perft({depth}) of {fen}"
            );
        }
    }
}
