//! Stockfish 18 NNUE evaluation.

pub mod accumulator;
mod arch;
pub mod features;
pub mod net_file;
pub mod network;
pub mod simd;
pub mod threat_delta;
pub mod trace;

use super::{Evaluator, IncrementalState, PositionView};
use crate::board::Board;
use crate::types::{Color, Move, Piece, PieceKind};
use accumulator::AccumulatorStack;
use arch::OUTPUT_SCALE;
use net_file::{BigNetwork, NetKind, NetLoadError, Network, load_big_network, load_small_network};
use network::{NetOutput, PerspectiveSums, bucket_for, transform};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Network path baked in at build time from the dev shell's `CHESS_NNUE_BIG`/`CHESS_NNUE_SMALL`.
pub fn default_net_path(kind: NetKind) -> Option<&'static str> {
    match kind {
        NetKind::Big => option_env!("CHESS_NNUE_BIG"),
        NetKind::Small => option_env!("CHESS_NNUE_SMALL"),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetPaths {
    pub big: PathBuf,
    pub small: PathBuf,
}

impl NetPaths {
    pub fn built_in() -> Option<Self> {
        Some(Self {
            big: PathBuf::from(default_net_path(NetKind::Big)?),
            small: PathBuf::from(default_net_path(NetKind::Small)?),
        })
    }

    /// The built-in paths, empty where none was set at build time.
    pub fn built_in_or_empty() -> Self {
        Self {
            big: PathBuf::from(default_net_path(NetKind::Big).unwrap_or_default()),
            small: PathBuf::from(default_net_path(NetKind::Small).unwrap_or_default()),
        }
    }
}

pub struct Networks {
    big: BigNetwork,
    small: Network,
    paths: NetPaths,
}

static NETWORKS: OnceLock<Networks> = OnceLock::new();

/// The networks, once loaded.
pub fn loaded() -> Option<&'static Networks> {
    NETWORKS.get()
}

fn load_error(path: &Path, error: NetLoadError) -> String {
    format!("{}: {error}", path.display())
}

/// Loads the networks on first use. They cannot be replaced afterwards, so asking for
/// different files once loaded is an error.
pub fn ensure_loaded(paths: &NetPaths) -> Result<(), String> {
    if let Some(loaded) = NETWORKS.get() {
        return if loaded.paths == *paths {
            Ok(())
        } else {
            Err(format!(
                "networks already loaded from {} and {}",
                loaded.paths.big.display(),
                loaded.paths.small.display()
            ))
        };
    }
    let big = load_big_network(&paths.big).map_err(|e| load_error(&paths.big, e))?;
    let small = load_small_network(&paths.small).map_err(|e| load_error(&paths.small, e))?;
    let _ = NETWORKS.set(Networks {
        big,
        small,
        paths: paths.clone(),
    });
    Ok(())
}

fn run_net(net: &Network, sums: &PerspectiveSums, side_to_move: Color, bucket: usize) -> NetOutput {
    let mut transformed = [0u8; accumulator::BIG_HALF];
    let transformed = &mut transformed[..net.transformer.half_dims];
    let psqt = transform(sums, side_to_move, bucket, transformed);
    let positional = net.stacks[bucket].propagate(transformed);
    NetOutput {
        psqt: psqt / OUTPUT_SCALE,
        positional: positional / OUTPUT_SCALE,
    }
}

const PAWN_VALUE: i32 = 208;
const NON_PAWN_VALUES: [(Piece, Piece, i32); 4] = [
    (Piece::WN, Piece::BN, 781),
    (Piece::WB, Piece::BB, 825),
    (Piece::WR, Piece::BR, 1276),
    (Piece::WQ, Piece::BQ, 2538),
];

fn count(position: PositionView, piece: Piece) -> i32 {
    position.piece_bb[piece.index()].count_ones() as i32
}

/// Non-pawn material of (white, black) in Stockfish's units.
fn non_pawn_material(position: PositionView) -> (i32, i32) {
    NON_PAWN_VALUES
        .iter()
        .fold((0, 0), |(white, black), &(wp, bp, value)| {
            (
                white + value * count(position, wp),
                black + value * count(position, bp),
            )
        })
}

/// Material balance for the side to move, which picks the small net in lopsided positions.
fn simple_eval(position: PositionView) -> i32 {
    let (white_npm, black_npm) = non_pawn_material(position);
    let white = PAWN_VALUE * count(position, Piece::WP) + white_npm;
    let black = PAWN_VALUE * count(position, Piece::BP) + black_npm;
    match position.turn {
        Color::White => white - black,
        Color::Black => black - white,
    }
}

const SMALL_NET_THRESHOLD: i32 = 962;
const SMALL_NET_RECHECK: i32 = 277;

/// Stockfish 18's `Eval::evaluate` with zero optimism, in its internal units.
fn stockfish_eval(nets: &Networks, stack: &mut AccumulatorStack, position: PositionView) -> i32 {
    let halfmove_clock = position.halfmove_clock;
    let bucket = bucket_for(position);
    let side_to_move = position.turn;

    let big = |stack: &mut AccumulatorStack| {
        let sums = stack.big(&nets.big.net.transformer, &nets.big.threats, position);
        run_net(&nets.big.net, &sums, side_to_move, bucket)
    };
    let blend = |out: NetOutput| (125 * out.psqt + 131 * out.positional) / 128;

    let use_small = simple_eval(position).abs() > SMALL_NET_THRESHOLD;
    let mut output = if use_small {
        let sums = stack.small(&nets.small.transformer, position);
        run_net(&nets.small, &sums, side_to_move, bucket)
    } else {
        big(stack)
    };
    if use_small && blend(output).abs() < SMALL_NET_RECHECK {
        output = big(stack);
    }

    let mut nnue = i64::from(blend(output));
    let complexity = i64::from((output.psqt - output.positional).abs());
    nnue -= nnue * complexity / 18_236;

    let (white_npm, black_npm) = non_pawn_material(position);
    let pawns = count(position, Piece::WP) + count(position, Piece::BP);
    let material = i64::from(534 * pawns + white_npm + black_npm);
    let mut value = nnue * (77_871 + material) / 77_871;
    value -= value * i64::from(halfmove_clock) / 199;
    value as i32
}

impl Evaluator for Networks {
    type State = AccumulatorStack;

    fn new_state(&self) -> AccumulatorStack {
        AccumulatorStack::new()
    }

    fn evaluate(&self, state: &mut AccumulatorStack, position: PositionView) -> i32 {
        trace::to_cp(stockfish_eval(self, state, position), position)
    }
}

impl IncrementalState for AccumulatorStack {
    #[inline]
    fn push_move(&mut self, before: PositionView, m: Move) {
        let mut record = self.push();
        threat_delta::record_move(before, m, record.threats);
        super::record_piece_changes(before, m, record.psq_updates);
        if before.piece_on[m.from as usize].kind() == Some(PieceKind::King) {
            record.king_moved(before.turn, m.from as usize, m.to as usize);
        }
    }

    #[inline]
    fn pop_move(&mut self) {
        self.pop();
    }

    fn reset(&mut self) {
        AccumulatorStack::reset(self);
    }
}

/// The big net's per-bucket table and the final evaluation, as Stockfish's `eval` prints them.
pub fn trace_text(board: &Board) -> Option<String> {
    let nets = NETWORKS.get()?;
    let mut text = trace::format_trace(&nets.big, board);
    let position = board.position_view();
    let internal = stockfish_eval(nets, &mut nets.new_state(), position);
    let white_relative = if board.turn == Color::White {
        internal
    } else {
        -internal
    };
    text.push_str(&format!(
        "Final evaluation       {:+.2} (white side)\n",
        0.01 * f64::from(trace::to_cp(white_relative, position))
    ));
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::evaluate;
    use crate::types::MoveList;
    use network::{Accumulation, SumsView, refresh_psq, refresh_threats};
    use rand::rngs::StdRng;
    use rand::{Rng, SeedableRng};

    fn nets() -> &'static Networks {
        crate::eval::EvalOptions::with_kind(crate::eval::EvalKind::Stockfish)
            .load()
            .unwrap();
        NETWORKS.get().unwrap()
    }

    fn assert_same(view: SumsView, expected: &Accumulation, what: &str, fen: &str) {
        assert_eq!(
            view.values,
            expected.values.as_slice(),
            "{what} values differ in {fen}"
        );
        assert_eq!(view.psqt, &expected.psqt, "{what} psqt differs in {fen}");
    }

    fn assert_incremental_matches_refresh(board: &mut Board) {
        let nets = nets();
        let fen = board.to_fen();
        let position = board.position_view();
        let colors = [Color::White, Color::Black];
        let big_psq = colors.map(|c| refresh_psq(&nets.big.net.transformer, position, c));
        let big_threats = colors.map(|c| refresh_threats(&nets.big.threats, 1024, position, c));
        let small_psq = colors.map(|c| refresh_psq(&nets.small.transformer, position, c));

        let (state, position) = board.eval_state_and_view();
        let crate::eval::EvalState::Stockfish(stack) = state else {
            panic!("board was created before the networks loaded");
        };
        let big = stack.big(&nets.big.net.transformer, &nets.big.threats, position);
        let threats = big.threats.as_ref().unwrap();
        for p in 0..2 {
            assert_same(big.psq[p], &big_psq[p], "big psq", &fen);
            assert_same(threats[p], &big_threats[p], "big threats", &fen);
        }
        let small = stack.small(&nets.small.transformer, position);
        for p in 0..2 {
            assert_same(small.psq[p], &small_psq[p], "small psq", &fen);
        }
    }

    fn random_move(board: &mut Board, rng: &mut StdRng) -> Option<crate::types::Move> {
        let mut moves = MoveList::new();
        board.generate_legal_moves(&mut moves);
        (moves.len() > 0).then(|| moves.get(rng.gen_range(0..moves.len())))
    }

    fn walk(board: &mut Board, depth: usize, rng: &mut StdRng) {
        if rng.gen_bool(0.4) {
            assert_incremental_matches_refresh(board);
        }
        if depth == 0 {
            return;
        }
        for _ in 0..3 {
            let Some(m) = random_move(board, rng) else {
                return;
            };
            let undo = board.make_move(m);
            walk(board, depth - 1, rng);
            board.unmake_move(m, undo);
        }
    }

    #[test]
    fn incremental_accumulators_match_a_full_refresh() {
        let starts = [
            crate::types::START_FEN,
            "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
            "r3k2r/Pppp1ppp/1b3nbN/nP6/BBP1P3/q4N2/Pp1P2PP/R2Q1RK1 w kq - 0 1",
            "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1",
            "rnbq1k1r/pp1Pbppp/2p5/8/2B5/8/PPP1NnPP/RNBQK2R w KQ - 1 8",
        ];
        nets();
        let mut rng = StdRng::seed_from_u64(11);
        for fen in starts {
            let mut board = Board::from_fen(fen).unwrap();
            walk(&mut board, 4, &mut rng);

            for _game in 0..6 {
                let mut board = Board::from_fen(fen).unwrap();
                for _ply in 0..150 {
                    if rng.gen_bool(0.3) {
                        assert_incremental_matches_refresh(&mut board);
                    }
                    let Some(m) = random_move(&mut board, &mut rng) else {
                        break;
                    };
                    board.make_move(m);
                }
                assert_incremental_matches_refresh(&mut board);
            }
        }
    }

    #[test]
    fn evaluation_is_symmetric_under_color_flip() {
        nets();
        let pairs = [
            (
                "r1bqkb1r/pppp1ppp/2n2n2/4p3/2B1P3/5N2/PPPP1PPP/RNBQK2R w KQkq - 4 4",
                "rnbqk2r/pppp1ppp/5n2/2b1p3/4P3/2N2N2/PPPP1PPP/R1BQKB1R b KQkq - 4 4",
            ),
            (
                "8/5k2/8/3K4/8/8/1P6/8 b - - 0 1",
                "8/1p6/8/8/3k4/8/5K2/8 w - - 0 1",
            ),
        ];
        for (fen, flipped) in pairs {
            let a = evaluate(&mut Board::from_fen(fen).unwrap());
            let b = evaluate(&mut Board::from_fen(flipped).unwrap());
            assert_eq!(a, b, "{fen} vs {flipped}");
        }
    }
}
