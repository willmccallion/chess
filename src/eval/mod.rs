//! Stockfish 18 NNUE evaluation.

mod arch;
pub mod features;
pub mod net_file;
pub mod network;
pub mod trace;

use net_file::NetKind;

/// Network path baked in at build time from the dev shell's `CHESS_NNUE_BIG`/`CHESS_NNUE_SMALL`.
pub fn default_net_path(kind: NetKind) -> Option<&'static str> {
    match kind {
        NetKind::Big => option_env!("CHESS_NNUE_BIG"),
        NetKind::Small => option_env!("CHESS_NNUE_SMALL"),
    }
}
