//! Reader for Stockfish 18 `.nnue` files (HalfKAv2_hm + FullThreats big net, HalfKAv2_hm small net).

use super::arch::{
    FC0_OUTPUTS, FC1_INPUTS_PADDED, FC1_OUTPUTS, LAYER_STACKS, PSQ_DIMENSIONS, PSQT_BUCKETS,
    THREAT_DIMENSIONS,
};
use super::simd::scramble_for_sparse;
use byteorder::{LittleEndian, ReadBytesExt};
use std::fmt;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

const FILE_VERSION: u32 = 0x7AF3_2F20;
const PSQ_FEATURE_HASH: u32 = 0x7f23_4cb8;
const THREAT_FEATURE_HASH: u32 = 0x8f23_4cb8;
const ARCH_INPUT_HASH: u32 = 0xEC42_E90D;
const AFFINE_HASH: u32 = 0xCC03_DAE4;
const CLIPPED_RELU_HASH: u32 = 0x538D_24C7;
const LEB128_MAGIC: &[u8; 17] = b"COMPRESSED_LEB128";

#[derive(Debug)]
pub enum NetLoadError {
    Io(std::io::Error),
    Format(String),
}

impl fmt::Display for NetLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::Format(msg) => write!(f, "invalid network file: {msg}"),
        }
    }
}

impl std::error::Error for NetLoadError {}

impl From<std::io::Error> for NetLoadError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

fn format_error(msg: impl Into<String>) -> NetLoadError {
    NetLoadError::Format(msg.into())
}


/// Weights of the threat half of the big net's feature transformer.
pub struct ThreatWeights {
    /// `[feature][neuron]`
    pub weights: Vec<i8>,
    /// `[feature][bucket]`
    pub psqt: Vec<i32>,
}

pub struct FeatureTransformer {
    pub half_dims: usize,
    pub biases: Vec<i16>,
    /// `[feature][neuron]`
    pub weights: Vec<i16>,
    /// `[feature][bucket]`
    pub psqt: Vec<i32>,
}

/// One of the eight output networks, selected by piece count.
pub struct LayerStack {
    pub fc0_biases: [i32; FC0_OUTPUTS],
    /// First-layer weights in the layout `simd::sparse_affine_16` reads.
    pub fc0_sparse: Vec<i8>,
    pub fc1_biases: [i32; FC1_OUTPUTS],
    /// `[output][input]`, padded input width.
    pub fc1_weights: [i8; FC1_OUTPUTS * FC1_INPUTS_PADDED],
    pub fc2_bias: i32,
    pub fc2_weights: [i8; FC1_OUTPUTS],
}

pub struct Network {
    pub transformer: FeatureTransformer,
    pub stacks: Vec<LayerStack>,
}

/// The big net: a `Network` plus the threat half of its feature transformer.
pub struct BigNetwork {
    pub net: Network,
    pub threats: ThreatWeights,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum NetKind {
    /// 1024 neurons per side, piece-square and threat inputs.
    Big,
    /// 128 neurons per side, piece-square inputs only.
    Small,
}

impl NetKind {
    pub const fn half_dims(self) -> usize {
        match self {
            Self::Big => 1024,
            Self::Small => 128,
        }
    }

    const fn uses_threats(self) -> bool {
        matches!(self, Self::Big)
    }
}

const fn affine_hash(prev: u32, outputs: u32) -> u32 {
    AFFINE_HASH.wrapping_add(outputs) ^ (prev >> 1) ^ (prev << 31)
}

const fn clipped_relu_hash(prev: u32) -> u32 {
    CLIPPED_RELU_HASH.wrapping_add(prev)
}

const fn transformer_hash(kind: NetKind) -> u32 {
    let features = if kind.uses_threats() {
        THREAT_FEATURE_HASH
    } else {
        PSQ_FEATURE_HASH
    };
    features ^ (kind.half_dims() as u32 * 2)
}

const fn layer_stack_hash(kind: NetKind) -> u32 {
    let mut hash = ARCH_INPUT_HASH ^ (kind.half_dims() as u32 * 2);
    hash = affine_hash(hash, FC0_OUTPUTS as u32);
    hash = clipped_relu_hash(hash);
    hash = affine_hash(hash, FC1_OUTPUTS as u32);
    hash = clipped_relu_hash(hash);
    affine_hash(hash, 1)
}

const fn network_hash(kind: NetKind) -> u32 {
    transformer_hash(kind) ^ layer_stack_hash(kind)
}

/// Signed LEB128 values packed in one length-prefixed block.
struct Leb128Block<'r, R: Read> {
    reader: &'r mut R,
    bytes_left: u32,
}

impl<'r, R: Read> Leb128Block<'r, R> {
    fn open(reader: &'r mut R) -> Result<Self, NetLoadError> {
        let mut magic = [0u8; LEB128_MAGIC.len()];
        reader.read_exact(&mut magic)?;
        if &magic != LEB128_MAGIC {
            return Err(format_error("missing LEB128 block marker"));
        }
        let bytes_left = reader.read_u32::<LittleEndian>()?;
        Ok(Self { reader, bytes_left })
    }

    fn next_byte(&mut self) -> Result<u8, NetLoadError> {
        if self.bytes_left == 0 {
            return Err(format_error("LEB128 block ended early"));
        }
        self.bytes_left -= 1;
        Ok(self.reader.read_u8()?)
    }

    fn next_i32(&mut self) -> Result<i32, NetLoadError> {
        let mut result: i32 = 0;
        let mut shift = 0u32;
        loop {
            let byte = self.next_byte()?;
            if shift < 32 {
                result |= i32::from(byte & 0x7f) << shift;
            }
            shift += 7;
            if byte & 0x80 == 0 {
                if shift < 32 && byte & 0x40 != 0 {
                    result |= !((1i32 << shift) - 1);
                }
                return Ok(result);
            }
        }
    }

    fn fill_i16(&mut self, out: &mut [i16]) -> Result<(), NetLoadError> {
        for value in out {
            let wide = self.next_i32()?;
            *value = i16::try_from(wide).map_err(|_| format_error("16-bit weight out of range"))?;
        }
        Ok(())
    }

    fn fill_i32(&mut self, out: &mut [i32]) -> Result<(), NetLoadError> {
        for value in out {
            *value = self.next_i32()?;
        }
        Ok(())
    }

    fn close(self) -> Result<(), NetLoadError> {
        if self.bytes_left == 0 {
            Ok(())
        } else {
            Err(format_error("LEB128 block has trailing bytes"))
        }
    }
}

fn expect_hash<R: Read>(reader: &mut R, expected: u32, what: &str) -> Result<(), NetLoadError> {
    let found = reader.read_u32::<LittleEndian>()?;
    if found == expected {
        Ok(())
    } else {
        Err(format_error(format!(
            "{what} hash {found:#010x}, expected {expected:#010x}"
        )))
    }
}

fn read_header<R: Read>(reader: &mut R, kind: NetKind) -> Result<String, NetLoadError> {
    let version = reader.read_u32::<LittleEndian>()?;
    if version != FILE_VERSION {
        return Err(format_error(format!("unsupported version {version:#010x}")));
    }
    expect_hash(reader, network_hash(kind), "network")?;
    let description_len = reader.read_u32::<LittleEndian>()? as usize;
    let mut description = vec![0u8; description_len];
    reader.read_exact(&mut description)?;
    Ok(String::from_utf8_lossy(&description).into_owned())
}

fn read_transformer<R: Read>(
    reader: &mut R,
    kind: NetKind,
) -> Result<(FeatureTransformer, Option<ThreatWeights>), NetLoadError> {
    expect_hash(reader, transformer_hash(kind), "feature transformer")?;
    let half_dims = kind.half_dims();
    let mut biases = vec![0i16; half_dims];
    let mut weights = vec![0i16; PSQ_DIMENSIONS * half_dims];
    let mut psqt = vec![0i32; PSQ_DIMENSIONS * PSQT_BUCKETS];

    let mut block = Leb128Block::open(reader)?;
    block.fill_i16(&mut biases)?;
    block.close()?;

    let threats = if kind.uses_threats() {
        let mut threat_weights = vec![0i8; THREAT_DIMENSIONS * half_dims];
        reader.read_i8_into(&mut threat_weights)?;

        let mut block = Leb128Block::open(reader)?;
        block.fill_i16(&mut weights)?;
        block.close()?;

        let mut threat_psqt = vec![0i32; THREAT_DIMENSIONS * PSQT_BUCKETS];
        let mut block = Leb128Block::open(reader)?;
        block.fill_i32(&mut threat_psqt)?;
        block.fill_i32(&mut psqt)?;
        block.close()?;
        Some(ThreatWeights {
            weights: threat_weights,
            psqt: threat_psqt,
        })
    } else {
        let mut block = Leb128Block::open(reader)?;
        block.fill_i16(&mut weights)?;
        block.close()?;
        let mut block = Leb128Block::open(reader)?;
        block.fill_i32(&mut psqt)?;
        block.close()?;
        // Stockfish doubles these on load so the small net's activations share the big net's scale.
        for w in biases.iter_mut().chain(weights.iter_mut()) {
            *w = w.wrapping_mul(2);
        }
        None
    };

    let transformer = FeatureTransformer {
        half_dims,
        biases,
        weights,
        psqt,
    };
    Ok((transformer, threats))
}

fn read_layer_stack<R: Read>(reader: &mut R, kind: NetKind) -> Result<LayerStack, NetLoadError> {
    expect_hash(reader, layer_stack_hash(kind), "layer stack")?;
    let half_dims = kind.half_dims();

    let mut fc0_biases = [0i32; FC0_OUTPUTS];
    reader.read_i32_into::<LittleEndian>(&mut fc0_biases)?;
    let mut fc0_weights = vec![0i8; FC0_OUTPUTS * half_dims];
    reader.read_i8_into(&mut fc0_weights)?;

    let mut fc1_biases = [0i32; FC1_OUTPUTS];
    reader.read_i32_into::<LittleEndian>(&mut fc1_biases)?;
    let mut fc1_weights = [0i8; FC1_OUTPUTS * FC1_INPUTS_PADDED];
    reader.read_i8_into(&mut fc1_weights)?;

    let fc2_bias = reader.read_i32::<LittleEndian>()?;
    let mut fc2_weights = [0i8; FC1_OUTPUTS];
    reader.read_i8_into(&mut fc2_weights)?;

    let fc0_sparse = scramble_for_sparse(&fc0_weights, half_dims, FC0_OUTPUTS);
    Ok(LayerStack {
        fc0_biases,
        fc0_sparse,
        fc1_biases,
        fc1_weights,
        fc2_bias,
        fc2_weights,
    })
}

/// Parses a complete network of the given kind and checks that nothing follows it.
fn read_network<R: Read>(
    reader: &mut R,
    kind: NetKind,
) -> Result<(Network, Option<ThreatWeights>), NetLoadError> {
    read_header(reader, kind)?;
    let (transformer, threats) = read_transformer(reader, kind)?;
    let stacks = (0..LAYER_STACKS)
        .map(|_| read_layer_stack(reader, kind))
        .collect::<Result<Vec<_>, _>>()?;

    let mut trailing = [0u8; 1];
    if reader.read(&mut trailing)? != 0 {
        return Err(format_error("unexpected data after the last layer"));
    }
    let net = Network {
        transformer,
        stacks,
    };
    Ok((net, threats))
}

fn open(path: &Path) -> Result<BufReader<File>, NetLoadError> {
    Ok(BufReader::with_capacity(1 << 20, File::open(path)?))
}

pub fn load_big_network(path: &Path) -> Result<BigNetwork, NetLoadError> {
    let (net, threats) = read_network(&mut open(path)?, NetKind::Big)?;
    let threats = threats.ok_or_else(|| format_error("big network has no threat weights"))?;
    Ok(BigNetwork { net, threats })
}

pub fn load_small_network(path: &Path) -> Result<Network, NetLoadError> {
    let (net, _) = read_network(&mut open(path)?, NetKind::Small)?;
    Ok(net)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leb128_block(values: &[i32]) -> Vec<u8> {
        let mut payload = Vec::new();
        for &value in values {
            let mut v = value;
            loop {
                let byte = (v & 0x7f) as u8;
                v >>= 7;
                let done = if byte & 0x40 == 0 { v == 0 } else { v == -1 };
                payload.push(if done { byte } else { byte | 0x80 });
                if done {
                    break;
                }
            }
        }
        let mut block = LEB128_MAGIC.to_vec();
        block.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        block.extend_from_slice(&payload);
        block
    }

    #[test]
    fn leb128_round_trips_signed_values() {
        let values = [0, 1, -1, 63, 64, -64, -65, 300, -300, i32::from(i16::MAX), i32::from(i16::MIN), 1 << 20, -(1 << 25)];
        let bytes = leb128_block(&values);
        let mut cursor = bytes.as_slice();

        let mut decoded = [0i32; 13];
        let mut block = Leb128Block::open(&mut cursor).unwrap();
        block.fill_i32(&mut decoded).unwrap();
        block.close().unwrap();

        assert_eq!(decoded, values);
    }

    #[test]
    fn leb128_rejects_a_block_with_unread_bytes() {
        let bytes = leb128_block(&[5, 6]);
        let mut cursor = bytes.as_slice();
        let mut one = [0i32; 1];

        let mut block = Leb128Block::open(&mut cursor).unwrap();
        block.fill_i32(&mut one).unwrap();

        assert!(block.close().is_err());
    }

    #[test]
    fn architecture_hashes_match_stockfish_18_files() {
        let big = std::env::var("CHESS_NNUE_BIG").expect("run inside the dev shell");
        let small = std::env::var("CHESS_NNUE_SMALL").expect("run inside the dev shell");

        let big = load_big_network(Path::new(&big)).unwrap();
        let small = load_small_network(Path::new(&small)).unwrap();

        assert_eq!(big.threats.weights.len(), THREAT_DIMENSIONS * 1024);
        assert_eq!(big.net.stacks.len(), LAYER_STACKS);
        assert_eq!(small.transformer.biases.len(), 128);
    }
}
