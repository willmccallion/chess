//! The network's weights, read from the `quantised.bin` a bullet checkpoint saves, and its
//! output layer.

use std::fmt;
use std::path::Path;

/// Piece-square inputs: own and enemy halves of 6 piece kinds × 64 squares.
pub const INPUTS: usize = 768;
/// Width of each perspective's accumulator.
pub const HIDDEN: usize = 128;
/// Quantisation of the first layer's weights and biases.
pub const QA: i16 = 255;
/// Quantisation of the output weights.
pub const QB: i16 = 64;
const QA_I32: i32 = QA as i32;
const QB_I32: i32 = QB as i32;
/// Centipawns per unit of the network's output, the trainer's `eval_scale`. Chosen so the
/// network's centipawns match Stockfish 18's, which the search's margins were tuned for.
pub const EVAL_SCALE: i32 = 115;

/// Bullet clips every weight to ±1.98, so quantised weights stay within ±2 × their factor.
const MAX_FEATURE_WEIGHT: i32 = 2 * QA_I32;
const MAX_OUTPUT_WEIGHT: i32 = 2 * QB_I32;
/// Most pieces on a board, hence most active features per perspective.
const MAX_ACTIVE_FEATURES: i32 = 32;
/// Keeps evaluations clear of the search's mate scores, which start near 22 000 cp.
const MAX_EVAL_CP: i32 = 20_000;

// The bias, every active feature and one feature added before its removal.
const _: () = assert!(
    (MAX_ACTIVE_FEATURES + 2) * MAX_FEATURE_WEIGHT <= i16::MAX as i32,
    "the accumulator can overflow i16"
);
const _: () = assert!(
    QA_I32 as i64 * QA_I32 as i64 * MAX_OUTPUT_WEIGHT as i64 * 2 * HIDDEN as i64 <= i32::MAX as i64,
    "the output sum can overflow i32"
);

const WEIGHT_COUNT: usize = INPUTS * HIDDEN + HIDDEN + 2 * HIDDEN + 1;
/// Bullet pads the quantised file to a multiple of 64 bytes.
const FILE_BYTES: usize = (WEIGHT_COUNT * 2).div_ceil(64) * 64;

/// One accumulator's worth of values: a column of the first layer, its biases, or the sums.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C, align(64))]
pub struct Column(pub [i16; HIDDEN]);

impl Column {
    pub const ZERO: Self = Self([0; HIDDEN]);

    pub fn add(&mut self, other: &Self) {
        for (value, &delta) in self.0.iter_mut().zip(&other.0) {
            *value += delta;
        }
    }

    pub fn sub(&mut self, other: &Self) {
        for (value, &delta) in self.0.iter_mut().zip(&other.0) {
            *value -= delta;
        }
    }
}

pub struct Network {
    /// Column `i` is what input feature `i` adds to an accumulator.
    feature_weights: Box<[Column; INPUTS]>,
    pub(super) feature_bias: Column,
    /// Output weights for the side to move's accumulator, then the other side's.
    output_weights: [Column; 2],
    output_bias: i32,
}

#[derive(Debug)]
pub enum NetLoadError {
    Io(std::io::Error),
    /// The file is not a quantised `(768 → 128)×2 → 1` network.
    WrongSize {
        expected: usize,
        found: usize,
    },
    /// A weight is larger than bullet's clipping allows, so sums could overflow.
    WeightOutOfRange {
        layer: &'static str,
        value: i16,
    },
}

impl fmt::Display for NetLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::WrongSize { expected, found } => write!(
                f,
                "expected a {expected}-byte quantised (768 -> {HIDDEN})x2 -> 1 network, found {found} bytes"
            ),
            Self::WeightOutOfRange { layer, value } => write!(
                f,
                "{layer} weight {value} is outside the range weight clipping allows"
            ),
        }
    }
}

/// Little-endian `i16`s read in file order.
struct Values<'a> {
    pairs: std::slice::Iter<'a, [u8; 2]>,
    file_bytes: usize,
}

impl Values<'_> {
    fn next_bounded(&mut self, layer: &'static str, bound: i32) -> Result<i16, NetLoadError> {
        let &bytes = self.pairs.next().ok_or(NetLoadError::WrongSize {
            expected: FILE_BYTES,
            found: self.file_bytes,
        })?;
        let value = i16::from_le_bytes(bytes);
        if i32::from(value).abs() > bound {
            return Err(NetLoadError::WeightOutOfRange { layer, value });
        }
        Ok(value)
    }

    fn column(&mut self, layer: &'static str, bound: i32) -> Result<Column, NetLoadError> {
        let mut column = Column::ZERO;
        for value in &mut column.0 {
            *value = self.next_bounded(layer, bound)?;
        }
        Ok(column)
    }
}

impl Network {
    pub fn load(path: &Path) -> Result<Self, NetLoadError> {
        let bytes = std::fs::read(path).map_err(NetLoadError::Io)?;
        Self::from_bytes(&bytes)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, NetLoadError> {
        if bytes.len() != FILE_BYTES {
            return Err(NetLoadError::WrongSize {
                expected: FILE_BYTES,
                found: bytes.len(),
            });
        }
        let mut values = Values {
            pairs: bytes.as_chunks::<2>().0.iter(),
            file_bytes: bytes.len(),
        };
        let mut feature_weights = Box::new([Column::ZERO; INPUTS]);
        for column in &mut feature_weights {
            *column = values.column("feature", MAX_FEATURE_WEIGHT)?;
        }
        let feature_bias = values.column("feature bias", MAX_FEATURE_WEIGHT)?;
        let output_weights = [
            values.column("output", MAX_OUTPUT_WEIGHT)?,
            values.column("output", MAX_OUTPUT_WEIGHT)?,
        ];
        let output_bias = values.next_bounded("output bias", MAX_OUTPUT_WEIGHT * QA_I32)?;
        Ok(Self {
            feature_weights,
            feature_bias,
            output_weights,
            output_bias: i32::from(output_bias),
        })
    }

    pub(super) fn feature(&self, index: usize) -> &Column {
        &self.feature_weights[index]
    }

    /// Centipawns for the side to move, from its accumulator and the other side's.
    pub fn output(&self, us: &Column, them: &Column) -> i32 {
        let sum =
            screlu_dot(us, &self.output_weights[0]) + screlu_dot(them, &self.output_weights[1]);
        let output = i64::from(sum / QA_I32 + self.output_bias);
        let cp = output * i64::from(EVAL_SCALE) / i64::from(QA_I32 * QB_I32);
        cp.clamp(-i64::from(MAX_EVAL_CP), i64::from(MAX_EVAL_CP)) as i32
    }
}

/// `Σ clamp(a, 0, QA)² × w`, in units of QA² × QB.
fn screlu_dot(accumulator: &Column, weights: &Column) -> i32 {
    accumulator
        .0
        .iter()
        .zip(&weights.0)
        .map(|(&a, &w)| {
            let clipped = i32::from(a).clamp(0, QA_I32);
            clipped * clipped * i32::from(w)
        })
        .sum()
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// A file in bullet's layout from the given layer values, padded as bullet pads it.
    pub fn net_bytes(
        feature: impl Fn(usize, usize) -> i16,
        bias: impl Fn(usize) -> i16,
        output: impl Fn(usize) -> i16,
        output_bias: i16,
    ) -> Vec<u8> {
        let mut values = Vec::with_capacity(WEIGHT_COUNT);
        for input in 0..INPUTS {
            values.extend((0..HIDDEN).map(|h| feature(input, h)));
        }
        values.extend((0..HIDDEN).map(bias));
        values.extend((0..2 * HIDDEN).map(output));
        values.push(output_bias);
        let mut bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        bytes.resize(FILE_BYTES, 0);
        bytes
    }

    #[test]
    fn a_file_of_the_wrong_size_is_rejected() {
        let mut bytes = net_bytes(|_, _| 0, |_| 0, |_| 0, 0);
        bytes.truncate(FILE_BYTES - 64);

        let result = Network::from_bytes(&bytes);

        assert!(matches!(result, Err(NetLoadError::WrongSize { .. })));
    }

    #[test]
    fn weights_beyond_the_clipping_range_are_rejected() {
        let bytes = net_bytes(|input, _| if input == 5 { 600 } else { 0 }, |_| 0, |_| 0, 0);

        let result = Network::from_bytes(&bytes);

        assert!(matches!(
            result,
            Err(NetLoadError::WeightOutOfRange { value: 600, .. })
        ));
    }

    #[test]
    fn the_output_follows_bullets_quantised_inference() {
        let bytes = net_bytes(|_, _| 0, |_| 0, |h| if h < HIDDEN { 64 } else { -32 }, 1000);
        let net = Network::from_bytes(&bytes).unwrap();
        let mut us = Column::ZERO;
        let mut them = Column::ZERO;
        us.0[0] = 255;
        us.0[1] = 300;
        us.0[2] = -40;
        them.0[0] = 100;

        let cp = net.output(&us, &them);

        let sum = 2 * 255 * 255 * 64 - 100 * 100 * 32;
        let expected = (sum / QA_I32 + 1000) * EVAL_SCALE / (QA_I32 * QB_I32);
        assert_eq!(cp, expected);
    }
}
