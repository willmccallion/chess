//! Dimensions of the Stockfish 18 network architecture.

/// HalfKAv2_hm: 32 king buckets x 11 piece kinds x 64 squares.
pub const PSQ_DIMENSIONS: usize = 22_528;
pub const THREAT_DIMENSIONS: usize = 79_856;
pub const PSQT_BUCKETS: usize = 8;
pub const LAYER_STACKS: usize = 8;

/// First hidden layer: 15 neurons plus one forwarded straight to the output.
pub const FC0_OUTPUTS: usize = 16;
pub const FC0_HIDDEN: usize = 15;
pub const FC1_INPUTS: usize = FC0_HIDDEN * 2;
pub const FC1_INPUTS_PADDED: usize = 32;
pub const FC1_OUTPUTS: usize = 32;

pub const OUTPUT_SCALE: i32 = 16;
pub const WEIGHT_SCALE_BITS: u32 = 6;
