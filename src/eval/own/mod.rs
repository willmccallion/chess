//! Our own network, trained with bullet on the engine's self-play games: a
//! `(768 → 128)×2 → 1` perspective network with SCReLU, loaded from a file at runtime.

mod accumulator;
mod network;

pub use accumulator::{AccumulatorStack, active_features};
pub use network::{EVAL_SCALE, HIDDEN, INPUTS, NetLoadError, Network, QA, QB};

use super::{Evaluator, IncrementalState, PositionView};
use crate::types::{Color, Move};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

/// Network path baked in at build time from `CHESS_NNUE_OWN`, if it was set.
pub fn default_net_path() -> PathBuf {
    PathBuf::from(option_env!("CHESS_NNUE_OWN").unwrap_or_default())
}

struct Loaded {
    net: Network,
    path: PathBuf,
}

/// Every network this process has read. One is kept once read, so switching
/// back to it reads nothing, and an evaluation that still uses it while the
/// evaluator is switched never sees it freed.
static LOADED: Mutex<Vec<&'static Loaded>> = Mutex::new(Vec::new());

/// The network in `path`, read from it unless this process already has.
pub fn load(path: &Path) -> Result<&'static Network, String> {
    let mut loaded = LOADED.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(&known) = loaded.iter().find(|known| known.path == path) {
        return Ok(&known.net);
    }
    if path.as_os_str().is_empty() {
        return Err(
            "no network file: pass --own-net, set the OwnEvalFile option, or build with CHESS_NNUE_OWN set"
                .to_string(),
        );
    }
    let net = Network::load(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let known: &'static Loaded = Box::leak(Box::new(Loaded {
        net,
        path: path.to_path_buf(),
    }));
    loaded.push(known);
    Ok(&known.net)
}

impl Evaluator for Network {
    type State = AccumulatorStack;

    fn new_state(&self) -> AccumulatorStack {
        AccumulatorStack::new()
    }

    fn evaluate(&self, state: &mut AccumulatorStack, position: PositionView) -> i32 {
        let [white, black] = state.current(self, position);
        match position.turn {
            Color::White => self.output(white, black),
            Color::Black => self.output(black, white),
        }
    }
}

impl IncrementalState for AccumulatorStack {
    #[inline]
    fn push_move(&mut self, before: PositionView, m: Move) {
        AccumulatorStack::push_move(self, before, m);
    }

    #[inline]
    fn pop_move(&mut self) {
        AccumulatorStack::pop_move(self);
    }

    fn reset(&mut self) {
        AccumulatorStack::reset(self);
    }
}

#[cfg(test)]
mod tests {
    use super::accumulator::feature_index;
    use super::network::tests::net_bytes;
    use super::*;
    use crate::board::Board;
    use crate::types::{MoveList, PieceKind};
    use rand::rngs::StdRng;
    use rand::{Rng, SeedableRng};

    /// A network with small pseudo-random weights, so every feature matters.
    fn random_net() -> Network {
        let weight = |a: usize, b: usize| ((a * 7919 + b * 104_729) % 61) as i16 - 30;
        let bytes = net_bytes(weight, |h| weight(h, 3), |h| weight(h, 11), 500);
        Network::from_bytes(&bytes).unwrap()
    }

    fn refreshed_eval(net: &Network, board: &Board) -> i32 {
        net.evaluate(&mut net.new_state(), board.position_view())
    }

    #[test]
    fn features_follow_bullets_chess768_layout() {
        let white_pawn_e2 = feature_index(Color::White, Color::White, PieceKind::Pawn, 12);
        let white_pawn_e2_for_black =
            feature_index(Color::Black, Color::White, PieceKind::Pawn, 12);
        let black_king_e8_for_black =
            feature_index(Color::Black, Color::Black, PieceKind::King, 60);

        assert_eq!(white_pawn_e2, 12);
        assert_eq!(white_pawn_e2_for_black, 384 + 52);
        assert_eq!(black_king_e8_for_black, 5 * 64 + 4);
    }

    #[test]
    fn incremental_evaluation_matches_a_fresh_one_along_random_games() {
        let net = random_net();
        let mut rng = StdRng::seed_from_u64(7);
        for _game in 0..20 {
            let mut board = Board::from_fen(crate::types::START_FEN).unwrap();
            let mut state = net.new_state();
            for _ply in 0..120 {
                let incremental = net.evaluate(&mut state, board.position_view());
                assert_eq!(
                    incremental,
                    refreshed_eval(&net, &board),
                    "{}",
                    board.to_fen()
                );
                let mut moves = MoveList::new();
                board.generate_legal_moves(&mut moves);
                if moves.is_empty() {
                    break;
                }
                let m = moves.get(rng.gen_range(0..moves.len()));
                state.push_move(board.position_view(), m);
                board.make_move(m);
            }
        }
    }

    #[test]
    fn unmade_moves_return_to_the_earlier_evaluation() {
        let net = random_net();
        let mut board =
            Board::from_fen("r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1")
                .unwrap();
        let mut state = net.new_state();
        let before = net.evaluate(&mut state, board.position_view());
        let mut moves = MoveList::new();
        board.generate_legal_moves(&mut moves);

        for i in 0..moves.len() {
            let m = moves.get(i);
            state.push_move(board.position_view(), m);
            let undo = board.make_move(m);
            net.evaluate(&mut state, board.position_view());
            board.unmake_move(m, undo);
            state.pop_move();
            assert_eq!(net.evaluate(&mut state, board.position_view()), before);
        }
    }

    #[test]
    fn colour_flipped_positions_evaluate_the_same() {
        let net = random_net();
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
            let a = refreshed_eval(&net, &Board::from_fen(fen).unwrap());
            let b = refreshed_eval(&net, &Board::from_fen(flipped).unwrap());
            assert_eq!(a, b, "{fen} vs {flipped}");
        }
    }
}
