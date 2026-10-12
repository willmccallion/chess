//! Loading start positions for engine matches from EPD opening suites.

use rand::Rng;
use rand::SeedableRng;
use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use std::fs::File;
use std::io::{BufRead, BufReader};

/// Turns an EPD line into a FEN by keeping the four position fields.
pub fn epd_to_fen(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let fields: Vec<&str> = trimmed.split_whitespace().take(4).collect();
    if fields.len() < 4 {
        return None;
    }
    Some(format!("{} 0 1", fields.join(" ")))
}

/// Picks `count` openings uniformly at random (reservoir sampling) in shuffled order.
pub fn sample_openings(path: &str, count: usize, seed: u64) -> Result<Vec<String>, String> {
    let file = File::open(path).map_err(|e| format!("cannot open openings '{path}': {e}"))?;
    let mut rng = StdRng::seed_from_u64(seed);
    let mut reservoir: Vec<String> = Vec::with_capacity(count);
    let mut seen = 0usize;

    for line in BufReader::new(file).lines() {
        let line = line.map_err(|e| format!("reading openings '{path}': {e}"))?;
        let Some(fen) = epd_to_fen(&line) else {
            continue;
        };
        if reservoir.len() < count {
            reservoir.push(fen);
        } else {
            let slot = rng.gen_range(0..=seen);
            if slot < count {
                reservoir[slot] = fen;
            }
        }
        seen += 1;
    }

    if reservoir.is_empty() {
        return Err(format!("no positions found in openings file '{path}'"));
    }
    reservoir.shuffle(&mut rng);
    Ok(reservoir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epd_line_keeps_position_fields_and_drops_operations() {
        let line = "rnbqkbnr/pppp1ppp/8/4p3/4P3/8/PPPP1PPP/RNBQKBNR w KQkq e6 c0 \"x\"; id \"y\";";
        assert_eq!(
            epd_to_fen(line).as_deref(),
            Some("rnbqkbnr/pppp1ppp/8/4p3/4P3/8/PPPP1PPP/RNBQKBNR w KQkq e6 0 1")
        );
    }

    #[test]
    fn epd_blank_comment_and_short_lines_are_skipped() {
        assert_eq!(epd_to_fen("   "), None);
        assert_eq!(epd_to_fen("# comment"), None);
        assert_eq!(epd_to_fen("8/8/8/8/8/8/8/8 w"), None);
    }
}
