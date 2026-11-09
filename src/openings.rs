//! Loading start positions for engine matches from EPD opening suites.

use crate::game_runner::StartPosition;
use rand::Rng;
use rand::SeedableRng;
use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

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

/// Picks `count` openings from the EPD file at `path` uniformly at random
/// (reservoir sampling) in shuffled order.
pub fn sample_openings(path: &Path, count: usize, seed: u64) -> Result<Vec<StartPosition>, String> {
    let source = path.display().to_string();
    let file = File::open(path).map_err(|e| format!("cannot open openings '{source}': {e}"))?;
    sample_from(BufReader::new(file), &source, count, seed)
}

/// [`sample_openings`] from the EPD lines of `epd`, which `source` names in
/// errors.
fn sample_from(
    epd: impl BufRead,
    source: &str,
    count: usize,
    seed: u64,
) -> Result<Vec<StartPosition>, String> {
    let mut rng = StdRng::seed_from_u64(seed);
    let mut reservoir: Vec<String> = Vec::with_capacity(count);
    let mut seen = 0usize;

    for line in epd.lines() {
        let line = line.map_err(|e| format!("reading openings '{source}': {e}"))?;
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
        return Err(format!("no positions found in openings file '{source}'"));
    }
    reservoir.shuffle(&mut rng);
    reservoir
        .iter()
        .map(|fen| StartPosition::parse(fen).map_err(|e| format!("{source}: {e}")))
        .collect()
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
    fn an_opening_that_is_no_legal_position_is_refused_by_name() {
        let epd = "8/8/8/8/8/8/8/8 w - -\n";

        let refused = sample_from(epd.as_bytes(), "suite.epd", 4, 1).err();

        assert!(refused.is_some_and(|e| e.starts_with("suite.epd: invalid start position")));
    }

    #[test]
    fn every_sampled_opening_reads_as_a_position() {
        let epd = "rnbqkbnr/pppp1ppp/8/4p3/4P3/8/PPPP1PPP/RNBQKBNR w KQkq e6\n# skip\n";

        let sampled = sample_from(epd.as_bytes(), "suite.epd", 4, 1).unwrap();

        let fens: Vec<&str> = sampled.iter().map(StartPosition::fen).collect();
        assert_eq!(
            fens,
            ["rnbqkbnr/pppp1ppp/8/4p3/4P3/8/PPPP1PPP/RNBQKBNR w KQkq e6 0 1"]
        );
    }

    #[test]
    fn epd_blank_comment_and_short_lines_are_skipped() {
        assert_eq!(epd_to_fen("   "), None);
        assert_eq!(epd_to_fen("# comment"), None);
        assert_eq!(epd_to_fen("8/8/8/8/8/8/8/8 w"), None);
    }
}
