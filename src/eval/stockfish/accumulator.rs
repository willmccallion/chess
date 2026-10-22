//! First-layer sums for every position on the current line, updated lazily from the
//! piece and threat changes each move records.

use super::arch::PSQT_BUCKETS;
use super::features::{SfPiece, active_threat_features, king_square, psq_index, threat_index};
use super::net_file::{FeatureTransformer, ThreatWeights};
use super::network::{PerspectiveSums, SumsView};
use super::simd::{update_rows, update_rows_from};
use super::threat_delta::{MAX_THREAT_DELTAS, ThreatDeltas};
use crate::eval::{MAX_PIECE_CHANGES, PositionView};
use crate::types::{Bitboard, Color, Piece, StackVec, UpdateBody};

pub const BIG_HALF: usize = 1024;
pub const SMALL_HALF: usize = 128;

/// Pieces that can differ between a cached king-square entry and the board.
const MAX_REFRESH_CHANGES: usize = 64;

#[derive(Clone, Copy)]
#[repr(C, align(64))]
pub struct Accumulator<const N: usize> {
    pub values: [i16; N],
    pub psqt: [i32; PSQT_BUCKETS],
}

impl<const N: usize> Accumulator<N> {
    const ZERO: Self = Self {
        values: [0; N],
        psqt: [0; PSQT_BUCKETS],
    };

    fn view(&self) -> SumsView<'_> {
        SumsView {
            values: &self.values,
            psqt: &self.psqt,
        }
    }

    fn reset_to(&mut self, biases: &[i16]) {
        self.values.copy_from_slice(biases);
        self.psqt = [0; PSQT_BUCKETS];
    }

    fn apply_psq(&mut self, ft: &FeatureTransformer, adds: &[usize], subs: &[usize]) {
        update_rows(&mut self.values, ft.weights.as_slice(), adds, subs);
        apply_psqt(&mut self.psqt, &ft.psqt, adds, subs);
    }

    fn apply_threats(&mut self, weights: &ThreatWeights, adds: &[usize], subs: &[usize]) {
        update_rows(&mut self.values, weights.weights.as_slice(), adds, subs);
        apply_psqt(&mut self.psqt, &weights.psqt, adds, subs);
    }

    /// `self = parent` plus the given piece-square changes.
    fn psq_from(&mut self, parent: &Self, ft: &FeatureTransformer, adds: &[usize], subs: &[usize]) {
        update_rows_from(
            &parent.values,
            &mut self.values,
            ft.weights.as_slice(),
            adds,
            subs,
        );
        self.psqt = parent.psqt;
        apply_psqt(&mut self.psqt, &ft.psqt, adds, subs);
    }

    /// `self = parent` plus the given threat changes.
    fn threats_from(
        &mut self,
        parent: &Self,
        weights: &ThreatWeights,
        adds: &[usize],
        subs: &[usize],
    ) {
        update_rows_from(
            &parent.values,
            &mut self.values,
            weights.weights.as_slice(),
            adds,
            subs,
        );
        self.psqt = parent.psqt;
        apply_psqt(&mut self.psqt, &weights.psqt, adds, subs);
    }
}

fn apply_psqt(psqt: &mut [i32; PSQT_BUCKETS], weights: &[i32], adds: &[usize], subs: &[usize]) {
    for (&row, sign) in adds
        .iter()
        .map(|r| (r, 1))
        .chain(subs.iter().map(|r| (r, -1)))
    {
        if let Some(row) = weights.get(row * PSQT_BUCKETS..(row + 1) * PSQT_BUCKETS) {
            for (sum, &w) in psqt.iter_mut().zip(row) {
                *sum += sign * w;
            }
        }
    }
}

/// Feature indices to add and remove in one accumulator update.
#[derive(Default)]
struct FeatureChanges<const N: usize> {
    adds: StackVec<usize, N>,
    subs: StackVec<usize, N>,
}

impl<const N: usize> FeatureChanges<N> {
    fn clear(&mut self) {
        self.adds.clear();
        self.subs.clear();
    }

    fn push(&mut self, feature: usize, add: bool) {
        if add {
            self.adds.push(feature);
        } else {
            self.subs.push(feature);
        }
    }
}

/// The last piece-square sums seen per (perspective, king square), so a king move only
/// applies the pieces that differ ("Finny table").
#[derive(Clone)]
struct RefreshCache<const N: usize> {
    entries: Vec<CacheEntry<N>>,
}

#[derive(Clone)]
struct CacheEntry<const N: usize> {
    acc: Accumulator<N>,
    pieces: [Bitboard; 13],
    valid: bool,
}

impl<const N: usize> RefreshCache<N> {
    fn new() -> Self {
        let empty = CacheEntry {
            acc: Accumulator::ZERO,
            pieces: [0; 13],
            valid: false,
        };
        Self {
            entries: vec![empty; 2 * 64],
        }
    }

    fn refresh(
        &mut self,
        ft: &FeatureTransformer,
        position: PositionView,
        perspective: Color,
        target: &mut Accumulator<N>,
    ) {
        let king_sq = king_square(position, perspective);
        let entry = &mut self.entries[perspective as usize * 64 + king_sq];
        if !entry.valid {
            entry.acc.reset_to(&ft.biases);
            entry.pieces = [0; 13];
            entry.valid = true;
        }
        let mut changes = FeatureChanges::<MAX_REFRESH_CHANGES>::default();
        for (index, (&current, cached)) in position
            .piece_bb
            .iter()
            .zip(entry.pieces.iter_mut())
            .enumerate()
        {
            let Some(piece) = PIECES_BY_INDEX[index].and_then(SfPiece::new) else {
                continue;
            };
            for (mut squares, add) in [(current & !*cached, true), (*cached & !current, false)] {
                while squares != 0 {
                    let sq = squares.trailing_zeros() as usize;
                    squares &= squares - 1;
                    changes.push(psq_index(perspective, sq, piece, king_sq), add);
                }
            }
            *cached = current;
        }
        entry
            .acc
            .apply_psq(ft, changes.adds.as_slice(), changes.subs.as_slice());
        *target = entry.acc;
    }
}

const PIECES_BY_INDEX: [Option<Piece>; 13] = [
    None,
    Some(Piece::WP),
    Some(Piece::WN),
    Some(Piece::WB),
    Some(Piece::WR),
    Some(Piece::WQ),
    Some(Piece::WK),
    Some(Piece::BP),
    Some(Piece::BN),
    Some(Piece::BB),
    Some(Piece::BR),
    Some(Piece::BQ),
    Some(Piece::BK),
];

fn refresh_threats<const N: usize>(
    weights: &ThreatWeights,
    position: PositionView,
    perspective: Color,
    target: &mut Accumulator<N>,
) {
    let mut features = Vec::with_capacity(128);
    active_threat_features(position, perspective, &mut features);
    *target = Accumulator::ZERO;
    target.apply_threats(weights, &features, &[]);
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum Part {
    BigPsq,
    BigThreats,
    SmallPsq,
}

/// Which network's piece-square sums to update.
#[derive(Copy, Clone, PartialEq, Eq)]
enum PsqNet {
    Big,
    Small,
}

impl PsqNet {
    const fn part(self) -> Part {
        match self {
            Self::Big => Part::BigPsq,
            Self::Small => Part::SmallPsq,
        }
    }
}

/// What one move changed, and the sums of the position after it once computed.
#[derive(Clone)]
struct Ply {
    psq_updates: StackVec<UpdateBody, MAX_PIECE_CHANGES>,
    /// Side whose king moved; its piece-square sums must be refreshed.
    king_mover: Option<Color>,
    /// Per perspective: its king changed board half, so its threat sums must be rebuilt.
    threat_refresh: [bool; 2],
    threats: ThreatDeltas,
    big_psq: [Accumulator<BIG_HALF>; 2],
    big_threats: [Accumulator<BIG_HALF>; 2],
    small_psq: [Accumulator<SMALL_HALF>; 2],
    computed: [[bool; 2]; 3],
}

impl Ply {
    fn new() -> Self {
        Self {
            psq_updates: StackVec::default(),
            king_mover: None,
            threat_refresh: [false; 2],
            threats: ThreatDeltas::default(),
            big_psq: [Accumulator::ZERO; 2],
            big_threats: [Accumulator::ZERO; 2],
            small_psq: [Accumulator::ZERO; 2],
            computed: [[false; 2]; 3],
        }
    }

    fn requires_refresh(&self, part: Part, perspective: Color) -> bool {
        match part {
            Part::BigPsq | Part::SmallPsq => self.king_mover == Some(perspective),
            Part::BigThreats => {
                self.threat_refresh[perspective as usize] || self.threats.overflowed
            }
        }
    }
}

/// Where `Board::make_move` records a move's changes.
pub struct PlyRecord<'a> {
    pub psq_updates: &'a mut StackVec<UpdateBody, MAX_PIECE_CHANGES>,
    pub threats: &'a mut ThreatDeltas,
    king_mover: &'a mut Option<Color>,
    threat_refresh: &'a mut [bool; 2],
}

impl PlyRecord<'_> {
    /// Notes that `color`'s king moved between `from` and `to`.
    pub fn king_moved(&mut self, color: Color, from: usize, to: usize) {
        *self.king_mover = Some(color);
        if (from % 8 < 4) != (to % 8 < 4) {
            self.threat_refresh[color as usize] = true;
        }
    }
}

pub struct AccumulatorStack {
    plies: Vec<Ply>,
    top: usize,
    big_cache: RefreshCache<BIG_HALF>,
    small_cache: RefreshCache<SMALL_HALF>,
}

impl Clone for AccumulatorStack {
    fn clone(&self) -> Self {
        Self {
            plies: self.plies[..=self.top].to_vec(),
            top: self.top,
            big_cache: self.big_cache.clone(),
            small_cache: self.small_cache.clone(),
        }
    }
}

impl Default for AccumulatorStack {
    fn default() -> Self {
        Self::new()
    }
}

impl AccumulatorStack {
    pub fn new() -> Self {
        Self {
            plies: vec![Ply::new()],
            top: 0,
            big_cache: RefreshCache::new(),
            small_cache: RefreshCache::new(),
        }
    }

    /// Forgets the line: the current position becomes an uncomputed root.
    pub fn reset(&mut self) {
        self.top = 0;
        self.plies[0].computed = [[false; 2]; 3];
    }

    /// Opens the entry for the next move.
    pub fn push(&mut self) -> PlyRecord<'_> {
        self.top += 1;
        if self.plies.len() == self.top {
            self.plies.push(Ply::new());
        }
        let ply = &mut self.plies[self.top];
        ply.computed = [[false; 2]; 3];
        ply.king_mover = None;
        ply.threat_refresh = [false; 2];
        ply.psq_updates.clear();
        PlyRecord {
            psq_updates: &mut ply.psq_updates,
            threats: &mut ply.threats,
            king_mover: &mut ply.king_mover,
            threat_refresh: &mut ply.threat_refresh,
        }
    }

    pub fn pop(&mut self) {
        self.top = self.top.saturating_sub(1);
    }

    /// Big-net sums of the current position.
    pub fn big(
        &mut self,
        ft: &FeatureTransformer,
        threats: &ThreatWeights,
        position: PositionView,
    ) -> PerspectiveSums<'_> {
        for perspective in [Color::White, Color::Black] {
            self.ensure_psq(PsqNet::Big, perspective, ft, position);
            self.ensure_threats(perspective, threats, position);
        }
        let ply = &self.plies[self.top];
        PerspectiveSums {
            psq: [ply.big_psq[0].view(), ply.big_psq[1].view()],
            threats: Some([ply.big_threats[0].view(), ply.big_threats[1].view()]),
        }
    }

    /// Small-net sums of the current position.
    pub fn small(
        &mut self,
        ft: &FeatureTransformer,
        position: PositionView,
    ) -> PerspectiveSums<'_> {
        for perspective in [Color::White, Color::Black] {
            self.ensure_psq(PsqNet::Small, perspective, ft, position);
        }
        let ply = &self.plies[self.top];
        PerspectiveSums {
            psq: [ply.small_psq[0].view(), ply.small_psq[1].view()],
            threats: None,
        }
    }

    /// The latest ply whose `part` is computed for `perspective` with only incremental
    /// changes after it, or `None` if the current position must be refreshed.
    fn find_base(&self, part: Part, perspective: Color) -> Option<usize> {
        let mut base = self.top;
        while !self.plies[base].computed[part as usize][perspective as usize] {
            if base == 0 || self.plies[base].requires_refresh(part, perspective) {
                return None;
            }
            base -= 1;
        }
        Some(base)
    }

    fn ensure_psq(
        &mut self,
        net: PsqNet,
        perspective: Color,
        ft: &FeatureTransformer,
        position: PositionView,
    ) {
        let p = perspective as usize;
        let part = net.part();
        let Some(base) = self.find_base(part, perspective) else {
            let ply = &mut self.plies[self.top];
            match net {
                PsqNet::Small => {
                    self.small_cache
                        .refresh(ft, position, perspective, &mut ply.small_psq[p])
                }
                PsqNet::Big => {
                    self.big_cache
                        .refresh(ft, position, perspective, &mut ply.big_psq[p])
                }
            }
            ply.computed[part as usize][p] = true;
            return;
        };
        let king_sq = king_square(position, perspective);
        let mut changes = FeatureChanges::default();
        for index in base + 1..=self.top {
            let (before, after) = self.plies.split_at_mut(index);
            let (previous, ply) = (&before[index - 1], &mut after[0]);
            match net {
                PsqNet::Small => {
                    psq_changes(&ply.psq_updates, perspective, king_sq, &mut changes);
                    ply.small_psq[p].psq_from(
                        &previous.small_psq[p],
                        ft,
                        changes.adds.as_slice(),
                        changes.subs.as_slice(),
                    );
                }
                PsqNet::Big => {
                    psq_changes(&ply.psq_updates, perspective, king_sq, &mut changes);
                    ply.big_psq[p].psq_from(
                        &previous.big_psq[p],
                        ft,
                        changes.adds.as_slice(),
                        changes.subs.as_slice(),
                    );
                }
            }
            ply.computed[part as usize][p] = true;
        }
    }

    fn ensure_threats(
        &mut self,
        perspective: Color,
        weights: &ThreatWeights,
        position: PositionView,
    ) {
        let p = perspective as usize;
        let part = Part::BigThreats as usize;
        let Some(base) = self.find_base(Part::BigThreats, perspective) else {
            let ply = &mut self.plies[self.top];
            refresh_threats(weights, position, perspective, &mut ply.big_threats[p]);
            ply.computed[part][p] = true;
            return;
        };
        let king_sq = king_square(position, perspective);
        let mut changes = FeatureChanges::default();
        for index in base + 1..=self.top {
            let (before, after) = self.plies.split_at_mut(index);
            let (previous, ply) = (&before[index - 1], &mut after[0]);
            threat_changes(&ply.threats, perspective, king_sq, &mut changes);
            ply.big_threats[p].threats_from(
                &previous.big_threats[p],
                weights,
                changes.adds.as_slice(),
                changes.subs.as_slice(),
            );
            ply.computed[part][p] = true;
        }
    }
}

fn psq_changes(
    updates: &StackVec<UpdateBody, MAX_PIECE_CHANGES>,
    perspective: Color,
    king_sq: usize,
    changes: &mut FeatureChanges<MAX_PIECE_CHANGES>,
) {
    changes.clear();
    for update in updates.as_slice() {
        if let Some(piece) = SfPiece::new(update.piece) {
            changes.push(
                psq_index(perspective, update.sq, piece, king_sq),
                update.add,
            );
        }
    }
}

fn threat_changes(
    deltas: &ThreatDeltas,
    perspective: Color,
    king_sq: usize,
    changes: &mut FeatureChanges<MAX_THREAT_DELTAS>,
) {
    changes.clear();
    for delta in deltas.list.as_slice() {
        let (Some(attacker), Some(attacked)) =
            (SfPiece::new(delta.attacker), SfPiece::new(delta.attacked))
        else {
            continue;
        };
        let from = usize::from(delta.from);
        let to = usize::from(delta.to);
        if let Some(feature) = threat_index(perspective, attacker, from, to, attacked, king_sq) {
            changes.push(feature, delta.add);
        }
    }
}
