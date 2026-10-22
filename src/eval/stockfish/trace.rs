//! Text output matching Stockfish's `eval` command, for checking the port against it.

use super::net_file::BigNetwork;
use super::network::{NetOutput, bucket_for, evaluate_all_buckets};
use crate::board::Board;
use crate::eval::PositionView;
use crate::types::{Color, Piece};
use std::fmt::Write;

/// Stockfish's WDL-model scale `a` for the material on the board.
fn win_rate_scale(position: PositionView) -> f64 {
    let count = |pieces: [Piece; 2]| {
        pieces
            .iter()
            .map(|p| position.piece_bb[p.index()].count_ones() as i32)
            .sum::<i32>()
    };
    let material = count([Piece::WP, Piece::BP])
        + 3 * count([Piece::WN, Piece::BN])
        + 3 * count([Piece::WB, Piece::BB])
        + 5 * count([Piece::WR, Piece::BR])
        + 9 * count([Piece::WQ, Piece::BQ]);
    let m = f64::from(material.clamp(17, 78)) / 58.0;
    const AS: [f64; 4] = [
        -72.325_658_36,
        185.938_320_38,
        -144.588_621_93,
        416.449_504_46,
    ];
    ((AS[0] * m + AS[1]) * m + AS[2]) * m + AS[3]
}

/// Internal units to the centipawns Stockfish prints.
pub fn to_cp(value: i32, position: PositionView) -> i32 {
    (100.0 * f64::from(value) / win_rate_scale(position)).round() as i32
}

fn aligned_pawns(value: i32, board: &Board) -> String {
    let sign = match value.signum() {
        -1 => '-',
        1 => '+',
        _ => ' ',
    };
    let pawns = (0.01 * f64::from(to_cp(value, board.position_view()))).abs();
    format!("{sign}{pawns:6.2}")
}

/// The "NNUE network contributions" table and big-net evaluation from Stockfish's `eval`.
pub fn format_trace(big: &BigNetwork, board: &Board) -> String {
    let outputs = evaluate_all_buckets(&big.net, Some(&big.threats), board);
    let used = bucket_for(board.position_view());
    let side = if board.turn == Color::White {
        "White"
    } else {
        "Black"
    };
    let mut out = String::new();
    let _ = writeln!(out, " NNUE network contributions ({side} to move)");
    for (bucket, NetOutput { psqt, positional }) in outputs.iter().copied().enumerate() {
        let marker = if bucket == used {
            " <-- this bucket is used"
        } else {
            ""
        };
        let _ = writeln!(
            out,
            "|  {bucket}         |  {}   |  {}   |  {}   |{marker}",
            aligned_pawns(psqt, board),
            aligned_pawns(positional, board),
            aligned_pawns(psqt + positional, board),
        );
    }
    let NetOutput { psqt, positional } = outputs[used];
    let white_relative = if board.turn == Color::White {
        psqt + positional
    } else {
        -(psqt + positional)
    };
    let _ = writeln!(
        out,
        "NNUE evaluation        {:+.2} (white side)",
        0.01 * f64::from(to_cp(white_relative, board.position_view()))
    );
    out
}
