#![allow(
    clippy::unreadable_literal,
    reason = "the attack tables and magics build.rs generates"
)]

use crate::types::Bitboard;

// The leaper attack tables, and for rooks and bishops the magics build.rs
// found with the attack tables they index.
include!(concat!(env!("OUT_DIR"), "/generated_attacks.rs"));

struct Magic {
    mask: Bitboard,
    magic: u64,
    shift: u32,
    offset: usize,
}

#[inline(always)]
pub fn get_rook_attacks(sq: usize, occupied: Bitboard) -> Bitboard {
    let magic = &ROOK_MAGICS[sq];
    let blockers = occupied & magic.mask;
    let index = (blockers.wrapping_mul(magic.magic) >> magic.shift) as usize;
    ROOK_ATTACKS[magic.offset + index]
}

#[inline(always)]
pub fn get_bishop_attacks(sq: usize, occupied: Bitboard) -> Bitboard {
    let magic = &BISHOP_MAGICS[sq];
    let blockers = occupied & magic.mask;
    let index = (blockers.wrapping_mul(magic.magic) >> magic.shift) as usize;
    BISHOP_ATTACKS[magic.offset + index]
}

#[inline(always)]
pub const fn knight_attacks_from(sq: usize) -> Bitboard {
    KNIGHT_ATTACKS[sq]
}

#[inline(always)]
pub const fn king_attacks_from(sq: usize) -> Bitboard {
    KING_ATTACKS[sq]
}
