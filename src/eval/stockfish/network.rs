//! Scalar forward pass of a Stockfish 18 network from fully refreshed accumulators.
//!
//! The refresh functions are the reference the incremental accumulators must reproduce.

use super::arch::{
    FC0_HIDDEN, FC0_OUTPUTS, FC1_INPUTS, FC1_INPUTS_PADDED, FC1_OUTPUTS, OUTPUT_SCALE,
    PSQT_BUCKETS, WEIGHT_SCALE_BITS,
};
use super::features::{active_psq_features, active_threat_features};
use super::net_file::{FeatureTransformer, LayerStack, Network, ThreatWeights};
use super::simd::{dot_u8_i8, pairwise_clipped_product, sparse_affine_16};
use crate::board::Board;
use crate::eval::PositionView;
use crate::types::Color;

/// One perspective's accumulated first-layer sums.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Accumulation {
    pub values: Vec<i16>,
    pub psqt: [i32; PSQT_BUCKETS],
}

impl Accumulation {
    fn new(initial: &[i16]) -> Self {
        Self {
            values: initial.to_vec(),
            psqt: [0; PSQT_BUCKETS],
        }
    }

    fn add_feature(&mut self, weights: &[i16], psqt: &[i32]) {
        for (value, &w) in self.values.iter_mut().zip(weights) {
            *value = value.wrapping_add(w);
        }
        for (sum, &w) in self.psqt.iter_mut().zip(psqt) {
            *sum += w;
        }
    }

    fn add_threat_feature(&mut self, weights: &[i8], psqt: &[i32]) {
        for (value, &w) in self.values.iter_mut().zip(weights) {
            *value = value.wrapping_add(i16::from(w));
        }
        for (sum, &w) in self.psqt.iter_mut().zip(psqt) {
            *sum += w;
        }
    }
}

pub fn refresh_psq(
    transformer: &FeatureTransformer,
    position: PositionView,
    perspective: Color,
) -> Accumulation {
    let half = transformer.half_dims;
    let mut features = Vec::with_capacity(32);
    active_psq_features(position, perspective, &mut features);
    let mut acc = Accumulation::new(&transformer.biases);
    for feature in features {
        acc.add_feature(
            &transformer.weights[feature * half..(feature + 1) * half],
            &transformer.psqt[feature * PSQT_BUCKETS..(feature + 1) * PSQT_BUCKETS],
        );
    }
    acc
}

pub fn refresh_threats(
    threats: &ThreatWeights,
    half: usize,
    position: PositionView,
    perspective: Color,
) -> Accumulation {
    let mut features = Vec::with_capacity(128);
    active_threat_features(position, perspective, &mut features);
    let mut acc = Accumulation::new(&vec![0; half]);
    for feature in features {
        acc.add_threat_feature(
            &threats.weights[feature * half..(feature + 1) * half],
            &threats.psqt[feature * PSQT_BUCKETS..(feature + 1) * PSQT_BUCKETS],
        );
    }
    acc
}

/// One perspective's first-layer sums, borrowed from wherever they are kept.
#[derive(Copy, Clone)]
pub struct SumsView<'a> {
    pub values: &'a [i16],
    pub psqt: &'a [i32; PSQT_BUCKETS],
}

impl<'a> From<&'a Accumulation> for SumsView<'a> {
    fn from(acc: &'a Accumulation) -> Self {
        Self {
            values: &acc.values,
            psqt: &acc.psqt,
        }
    }
}

/// Both perspectives' sums, indexed by `Color as usize`.
pub struct PerspectiveSums<'a> {
    pub psq: [SumsView<'a>; 2],
    pub threats: Option<[SumsView<'a>; 2]>,
}

const BIG_NET_CLIP: i16 = 255;
const SMALL_NET_CLIP: i16 = 127 * 2;

/// Pairwise-multiplied, clipped first-layer output for the side to move first; returns
/// the material (PSQT) term for `bucket`.
pub fn transform(
    sums: &PerspectiveSums,
    side_to_move: Color,
    bucket: usize,
    output: &mut [u8],
) -> i32 {
    let perspectives = [side_to_move, side_to_move.other()];
    let us = perspectives[0] as usize;
    let them = perspectives[1] as usize;
    let mut psqt = sums.psq[us].psqt[bucket] - sums.psq[them].psqt[bucket];
    if let Some(threats) = &sums.threats {
        psqt += threats[us].psqt[bucket] - threats[them].psqt[bucket];
    }
    psqt /= 2;

    let half = sums.psq[us].values.len();
    let pair = half / 2;
    for (p, perspective) in perspectives.into_iter().enumerate() {
        let psq = sums.psq[perspective as usize].values;
        let threat = sums
            .threats
            .as_ref()
            .map(|t| t[perspective as usize].values);
        let clip = if threat.is_some() {
            BIG_NET_CLIP
        } else {
            SMALL_NET_CLIP
        };
        pairwise_clipped_product(
            &psq[..pair],
            &psq[pair..half],
            threat.map(|t| (&t[..pair], &t[pair..half])),
            clip,
            &mut output[p * pair..(p + 1) * pair],
        );
    }
    psqt
}

fn clipped_relu(value: i32) -> u8 {
    (value >> WEIGHT_SCALE_BITS).clamp(0, 127) as u8
}

fn squared_clipped_relu(value: i32) -> u8 {
    ((i64::from(value) * i64::from(value)) >> (2 * WEIGHT_SCALE_BITS + 7)).min(127) as u8
}

impl LayerStack {
    /// Positional term (before output scaling) from the transformed features.
    pub fn propagate(&self, input: &[u8]) -> i32 {
        let fc0 = sparse_affine_16(input, &self.fc0_sparse, &self.fc0_biases);

        let mut hidden = [0u8; FC1_INPUTS_PADDED];
        for i in 0..FC0_HIDDEN {
            hidden[i] = squared_clipped_relu(fc0[i]);
            hidden[FC0_HIDDEN + i] = clipped_relu(fc0[i]);
        }
        debug_assert_eq!(FC1_INPUTS, 2 * FC0_HIDDEN);

        let mut fc1 = [0u8; FC1_OUTPUTS];
        for (out, (bias, row)) in fc1.iter_mut().zip(
            self.fc1_biases
                .iter()
                .zip(self.fc1_weights.chunks_exact(FC1_INPUTS_PADDED)),
        ) {
            *out = clipped_relu(bias + dot_u8_i8(&hidden, row));
        }

        let fc2 = self.fc2_bias + dot_u8_i8(&fc1, &self.fc2_weights);

        // fc0's last neuron skips to the output: 127 << 6 in it means 600 * OUTPUT_SCALE.
        let forwarded =
            fc0[FC0_OUTPUTS - 1] * (600 * OUTPUT_SCALE) / (127 * (1 << WEIGHT_SCALE_BITS));
        fc2 + forwarded
    }
}

/// Material and positional terms in Stockfish's internal units, side to move relative.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct NetOutput {
    pub psqt: i32,
    pub positional: i32,
}

/// Layer stack chosen by the number of pieces on the board.
pub fn bucket_for(position: PositionView) -> usize {
    (position.occupied.count_ones() as usize - 1) / 4
}

pub fn evaluate_bucket(
    net: &Network,
    sums: &PerspectiveSums,
    side_to_move: Color,
    bucket: usize,
) -> NetOutput {
    let mut transformed = vec![0u8; net.transformer.half_dims];
    let psqt = transform(sums, side_to_move, bucket, &mut transformed);
    let positional = net.stacks[bucket].propagate(&transformed);
    NetOutput {
        psqt: psqt / OUTPUT_SCALE,
        positional: positional / OUTPUT_SCALE,
    }
}

/// Every bucket's output for `board`, computed from scratch; `threats` only for the big net.
pub fn evaluate_all_buckets(
    net: &Network,
    threats: Option<&ThreatWeights>,
    board: &Board,
) -> Vec<NetOutput> {
    let position = board.position_view();
    let half = net.transformer.half_dims;
    let psq = [
        refresh_psq(&net.transformer, position, Color::White),
        refresh_psq(&net.transformer, position, Color::Black),
    ];
    let threat_sums = threats.map(|threats| {
        [
            refresh_threats(threats, half, position, Color::White),
            refresh_threats(threats, half, position, Color::Black),
        ]
    });
    let sums = PerspectiveSums {
        psq: [SumsView::from(&psq[0]), SumsView::from(&psq[1])],
        threats: threat_sums
            .as_ref()
            .map(|[white, black]| [SumsView::from(white), SumsView::from(black)]),
    };
    (0..net.stacks.len())
        .map(|bucket| evaluate_bucket(net, &sums, board.turn, bucket))
        .collect()
}
