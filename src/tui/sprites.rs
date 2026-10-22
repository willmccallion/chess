use crate::types::PieceKind;

const GRID: usize = 16;
const SAMPLES_PER_AXIS: usize = 4;

const PAWN: [&str; GRID] = [
    "................",
    "................",
    "................",
    ".......##.......",
    "......####......",
    "......####......",
    ".......##.......",
    "......####......",
    ".....######.....",
    "......####......",
    "......####......",
    ".....######.....",
    "....########....",
    "...##########...",
    "...##########...",
    "................",
];

const KNIGHT: [&str; GRID] = [
    "................",
    "......#..#......",
    "......#####.....",
    ".....#######....",
    "....#########...",
    "...##########...",
    "..###########...",
    ".############...",
    ".####..######...",
    "..##...######...",
    "......#######...",
    ".....########...",
    "....#########...",
    "...###########..",
    "..############..",
    "................",
];

const BISHOP: [&str; GRID] = [
    "................",
    ".......##.......",
    "......####......",
    ".......##.......",
    "......####......",
    ".....######.....",
    ".....######.....",
    ".....######.....",
    "......####......",
    ".......##.......",
    "......####......",
    ".....######.....",
    "...##########...",
    "...##########...",
    "................",
    "................",
];

const ROOK: [&str; GRID] = [
    "................",
    "..###.####.###..",
    "..###.####.###..",
    "..############..",
    "...##########...",
    "....########....",
    "....########....",
    "....########....",
    "....########....",
    "....########....",
    "...##########...",
    "..############..",
    "..############..",
    ".##############.",
    ".##############.",
    "................",
];

const QUEEN: [&str; GRID] = [
    "................",
    ".##..##..##..##.",
    ".##..##..##..##.",
    "..#..##..##..#..",
    "..############..",
    "..############..",
    "...##########...",
    "....########....",
    "....########....",
    "...##########...",
    "..############..",
    "..############..",
    ".##############.",
    ".##############.",
    "................",
    "................",
];

const KING: [&str; GRID] = [
    ".......##.......",
    ".....######.....",
    ".......##.......",
    "......####......",
    "...##########...",
    "..############..",
    "..############..",
    "...##########...",
    "....########....",
    "....########....",
    "...##########...",
    "..############..",
    "..############..",
    ".##############.",
    ".##############.",
    "................",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pixel {
    Empty,
    Fill,
    Outline,
}

fn silhouette(kind: PieceKind) -> &'static [&'static str; GRID] {
    match kind {
        PieceKind::Pawn => &PAWN,
        PieceKind::Knight => &KNIGHT,
        PieceKind::Bishop => &BISHOP,
        PieceKind::Rook => &ROOK,
        PieceKind::Queen => &QUEEN,
        PieceKind::King => &KING,
    }
}

/// The grid column or row under `position`, a fraction of the way across; positions mirrored
/// about the centre always land in mirrored cells, so symmetric pieces stay symmetric.
fn grid_cell(position: f64) -> usize {
    let scaled = position * GRID as f64;
    if position < 0.5 {
        scaled as usize
    } else {
        GRID - 1 - (GRID as f64 - scaled) as usize
    }
}

/// Fraction of the silhouette covering pixel (x, y) of a `width` × `height` image.
fn coverage(rows: &[&str; GRID], x: usize, y: usize, width: usize, height: usize) -> f64 {
    let sample = |pixel: usize, sub: usize, size: usize| {
        (pixel as f64 + (sub as f64 + 0.5) / SAMPLES_PER_AXIS as f64) / size as f64
    };
    let mut inside = 0;
    for sy in 0..SAMPLES_PER_AXIS {
        for sx in 0..SAMPLES_PER_AXIS {
            let gx = grid_cell(sample(x, sx, width));
            let gy = grid_cell(sample(y, sy, height));
            if rows[gy].as_bytes()[gx] == b'#' {
                inside += 1;
            }
        }
    }
    f64::from(inside) / (SAMPLES_PER_AXIS * SAMPLES_PER_AXIS) as f64
}

/// The piece scaled to `width` × `height` pixels, rows top to bottom, with a one-pixel
/// outline wherever the piece meets the background.
pub fn render(kind: PieceKind, width: usize, height: usize) -> Vec<Vec<Pixel>> {
    let rows = silhouette(kind);
    let inside: Vec<Vec<bool>> = (0..height)
        .map(|y| {
            (0..width)
                .map(|x| coverage(rows, x, y, width, height) >= 0.5)
                .collect()
        })
        .collect();
    let is_inside = |x: isize, y: isize| {
        usize::try_from(y)
            .ok()
            .zip(usize::try_from(x).ok())
            .and_then(|(y, x)| inside.get(y)?.get(x).copied())
            .unwrap_or(false)
    };
    (0..height as isize)
        .map(|y| {
            (0..width as isize)
                .map(|x| {
                    if !is_inside(x, y) {
                        Pixel::Empty
                    } else if [(x - 1, y), (x + 1, y), (x, y - 1), (x, y + 1)]
                        .iter()
                        .all(|&(nx, ny)| is_inside(nx, ny))
                    {
                        Pixel::Fill
                    } else {
                        Pixel::Outline
                    }
                })
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const KINDS: [PieceKind; 6] = [
        PieceKind::Pawn,
        PieceKind::Knight,
        PieceKind::Bishop,
        PieceKind::Rook,
        PieceKind::Queen,
        PieceKind::King,
    ];

    #[test]
    fn silhouettes_are_square_grids() {
        for kind in KINDS {
            for row in silhouette(kind) {
                assert_eq!(row.len(), GRID, "{kind:?}");
            }
        }
    }

    #[test]
    fn scaled_pieces_fill_the_requested_size_and_keep_an_interior() {
        for kind in KINDS {
            for (width, height) in [(7, 6), (9, 8), (13, 12), (21, 20)] {
                let pixels = render(kind, width, height);

                assert_eq!(pixels.len(), height);
                assert!(pixels.iter().all(|row| row.len() == width));
                let fill = pixels
                    .iter()
                    .flatten()
                    .filter(|&&p| p == Pixel::Fill)
                    .count();
                assert!(
                    fill > 0 || width < 9,
                    "{kind:?} at {width}x{height} has no interior"
                );
            }
        }
    }
}
