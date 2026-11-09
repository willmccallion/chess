#![allow(clippy::unreadable_literal, reason = "the keys build.rs generates")]

use crate::types::Piece;

// Include the pre-computed keys
include!(concat!(env!("OUT_DIR"), "/generated_zobrist.rs"));

// A single, global, pre-computed instance of the Zobrist keys
pub static ZOB: &Zobrist = &ZOBRIST_KEYS;

#[derive(Clone)]
pub struct Zobrist {
    pub piece: [[ZKey; 64]; 13],
    pub castle: [ZKey; 16],
    pub ep_file: [ZKey; 8],
    pub side: ZKey,
}

impl Zobrist {
    #[inline]
    pub const fn piece_key(&self, pc: Piece, sq: usize) -> ZKey {
        self.piece[pc.index()][sq]
    }
}
