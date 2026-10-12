//! Lock-free shared transposition table.
//!
//! Each slot stores `key ^ data` next to `data`; a torn write from a racing thread
//! fails the key check on the next probe instead of returning a corrupt entry.

use crate::types::{Move, ZKey};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::{_MM_HINT_T0, _mm_prefetch};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Bound {
    Exact = 0,
    Lower = 1,
    Upper = 2,
}

impl Bound {
    #[inline(always)]
    fn from_bits(bits: u64) -> Self {
        match bits {
            0 => Bound::Exact,
            1 => Bound::Lower,
            _ => Bound::Upper,
        }
    }
}

const SCORE_SHIFT: u64 = 0;
const EVAL_SHIFT: u64 = 16;
const MOVE_SHIFT: u64 = 32;
const DEPTH_SHIFT: u64 = 48;
const BOUND_SHIFT: u64 = 56;
const AGE_SHIFT: u64 = 58;
const PV_SHIFT: u64 = 63;

const SCORE_MASK: u64 = 0xFFFF;
const EVAL_MASK: u64 = 0xFFFF;
const MOVE_MASK: u64 = 0xFFFF;
const DEPTH_MASK: u64 = 0xFF;
const BOUND_MASK: u64 = 0x3;
const AGE_MASK: u64 = 0x1F;

const NO_EVAL: i16 = i16::MIN;
const HASHFULL_SAMPLE_CLUSTERS: usize = 1000;

/// A decoded entry, copied out of the table.
#[derive(Copy, Clone, Debug)]
pub struct TTEntry {
    data: u64,
}

impl TTEntry {
    fn pack(
        depth: i16,
        score: i32,
        eval: Option<i32>,
        bound: Bound,
        best_move: Option<Move>,
        is_pv: bool,
        age: u8,
    ) -> Self {
        let packed_score = u64::from(score as i16 as u16);
        let packed_eval = u64::from(eval.map_or(NO_EVAL, |e| e as i16) as u16);
        let packed_move = u64::from(best_move.map_or(0u16, u16::from));
        let packed_depth = u64::from(depth as u8);
        let data = (packed_score << SCORE_SHIFT)
            | (packed_eval << EVAL_SHIFT)
            | (packed_move << MOVE_SHIFT)
            | (packed_depth << DEPTH_SHIFT)
            | ((bound as u64) << BOUND_SHIFT)
            | ((u64::from(age) & AGE_MASK) << AGE_SHIFT)
            | (u64::from(is_pv) << PV_SHIFT);
        Self { data }
    }

    #[inline(always)]
    pub fn score(&self) -> i32 {
        i32::from(((self.data >> SCORE_SHIFT) & SCORE_MASK) as u16 as i16)
    }

    #[inline(always)]
    pub fn eval(&self) -> Option<i32> {
        let eval = ((self.data >> EVAL_SHIFT) & EVAL_MASK) as u16 as i16;
        (eval != NO_EVAL).then_some(i32::from(eval))
    }

    #[inline(always)]
    pub fn depth(&self) -> i16 {
        i16::from(((self.data >> DEPTH_SHIFT) & DEPTH_MASK) as u8)
    }

    #[inline(always)]
    pub fn best_move(&self) -> Option<Move> {
        let packed_move = ((self.data >> MOVE_SHIFT) & MOVE_MASK) as u16;
        (packed_move != 0).then(|| packed_move.into())
    }

    #[inline(always)]
    pub fn bound(&self) -> Bound {
        Bound::from_bits((self.data >> BOUND_SHIFT) & BOUND_MASK)
    }

    /// Whether the position was on a principal variation when stored.
    #[inline(always)]
    pub fn is_pv(&self) -> bool {
        (self.data >> PV_SHIFT) & 1 == 1
    }

    #[inline(always)]
    fn age(&self) -> u8 {
        ((self.data >> AGE_SHIFT) & AGE_MASK) as u8
    }
}

#[derive(Default)]
struct Slot {
    key_xor_data: AtomicU64,
    data: AtomicU64,
}

impl Slot {
    #[inline(always)]
    fn load(&self) -> (ZKey, u64) {
        let data = self.data.load(Ordering::Relaxed);
        let key = self.key_xor_data.load(Ordering::Relaxed) ^ data;
        (key, data)
    }

    #[inline(always)]
    fn store(&self, key: ZKey, data: u64) {
        self.key_xor_data.store(key ^ data, Ordering::Relaxed);
        self.data.store(data, Ordering::Relaxed);
    }

    #[inline(always)]
    fn is_empty(&self) -> bool {
        self.data.load(Ordering::Relaxed) == 0 && self.key_xor_data.load(Ordering::Relaxed) == 0
    }

    fn clear(&self) {
        self.key_xor_data.store(0, Ordering::Relaxed);
        self.data.store(0, Ordering::Relaxed);
    }
}

const CLUSTER_SIZE: usize = 4;

#[derive(Default)]
#[repr(align(64))]
struct Cluster {
    slots: [Slot; CLUSTER_SIZE],
}

struct TransTable {
    clusters: Box<[Cluster]>,
    age: AtomicU8,
}

/// Age distance in the 5-bit wrapping generation counter.
#[inline(always)]
fn age_distance(current: u8, entry: u8) -> i32 {
    i32::from(current.wrapping_sub(entry) & AGE_MASK as u8)
}

impl TransTable {
    fn with_mb(mb: usize) -> Self {
        let bytes = mb.saturating_mul(1024 * 1024);
        let cluster_count = (bytes / std::mem::size_of::<Cluster>()).max(1);
        let clusters = (0..cluster_count).map(|_| Cluster::default()).collect();
        Self {
            clusters,
            age: AtomicU8::new(0),
        }
    }

    #[inline(always)]
    fn cluster(&self, key: ZKey) -> &Cluster {
        let index = ((u128::from(key) * self.clusters.len() as u128) >> 64) as usize;
        &self.clusters[index]
    }

    #[inline(always)]
    fn current_age(&self) -> u8 {
        self.age.load(Ordering::Relaxed) & AGE_MASK as u8
    }
}

/// Cheaply clonable handle to one table shared by all search threads.
#[derive(Clone)]
pub struct SharedTransTable {
    table: Arc<TransTable>,
}

impl SharedTransTable {
    pub fn new(size_mb: usize) -> Self {
        Self {
            table: Arc::new(TransTable::with_mb(size_mb)),
        }
    }

    /// Starts loading the cluster for `key` so a later probe hits cache.
    #[inline(always)]
    pub fn prefetch(&self, key: ZKey) {
        #[cfg(target_arch = "x86_64")]
        {
            let cluster: *const Cluster = self.table.cluster(key);
            // SAFETY: prefetch is a hint and never faults; the pointer is a live cluster.
            unsafe { _mm_prefetch(cluster.cast::<i8>(), _MM_HINT_T0) };
        }
        #[cfg(not(target_arch = "x86_64"))]
        let _ = key;
    }

    #[inline]
    pub fn probe(&self, key: ZKey) -> Option<TTEntry> {
        self.table
            .cluster(key)
            .slots
            .iter()
            .map(Slot::load)
            .find(|&(slot_key, _)| slot_key == key)
            .map(|(_, data)| TTEntry { data })
    }

    #[inline]
    pub fn store(
        &self,
        key: ZKey,
        depth: i16,
        score: i32,
        eval: Option<i32>,
        bound: Bound,
        best_move: Option<Move>,
        is_pv: bool,
    ) {
        let age = self.table.current_age();
        let new_entry = TTEntry::pack(depth, score, eval, bound, best_move, is_pv, age);
        let slots = &self.table.cluster(key).slots;

        for slot in slots {
            let (slot_key, data) = slot.load();
            if slot_key == key {
                let existing = TTEntry { data };
                if existing.age() == age || new_entry.depth() >= existing.depth() {
                    let kept_move = best_move.or(existing.best_move());
                    let entry = TTEntry::pack(depth, score, eval, bound, kept_move, is_pv, age);
                    slot.store(key, entry.data);
                }
                return;
            }
        }

        if let Some(slot) = slots.iter().find(|slot| slot.is_empty()) {
            slot.store(key, new_entry.data);
            return;
        }

        let replacement_quality = |slot: &Slot| {
            let entry = TTEntry {
                data: slot.load().1,
            };
            i32::from(entry.depth()) * 2 - age_distance(age, entry.age())
        };
        let mut victim = &slots[0];
        let mut worst_quality = replacement_quality(victim);
        for slot in &slots[1..] {
            let quality = replacement_quality(slot);
            if quality < worst_quality {
                worst_quality = quality;
                victim = slot;
            }
        }
        victim.store(key, new_entry.data);
    }

    pub fn clear(&self) {
        for cluster in self.table.clusters.iter() {
            for slot in &cluster.slots {
                slot.clear();
            }
        }
        self.tick_age();
    }

    pub fn tick_age(&self) {
        self.table.age.fetch_add(1, Ordering::Relaxed);
    }

    /// Fill rate in permille, estimated from the first clusters.
    pub fn hashfull_permill(&self) -> u32 {
        let sample =
            &self.table.clusters[..self.table.clusters.len().min(HASHFULL_SAMPLE_CLUSTERS)];
        let age = self.table.current_age();
        let filled = sample
            .iter()
            .flat_map(|cluster| cluster.slots.iter())
            .filter(|slot| {
                !slot.is_empty()
                    && TTEntry {
                        data: slot.load().1,
                    }
                    .age()
                        == age
            })
            .count();
        (filled * 1000 / (sample.len() * CLUSTER_SIZE)) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Move;

    #[test]
    fn stored_entry_round_trips_all_fields() {
        let tt = SharedTransTable::new(1);
        let mv = Move::quiet(12, 28);

        tt.store(
            0xDEAD_BEEF_1234_5678,
            17,
            -1234,
            Some(56),
            Bound::Lower,
            Some(mv),
            true,
        );
        let entry = tt.probe(0xDEAD_BEEF_1234_5678).unwrap();

        assert_eq!(entry.depth(), 17);
        assert_eq!(entry.score(), -1234);
        assert_eq!(entry.eval(), Some(56));
        assert_eq!(entry.bound(), Bound::Lower);
        assert_eq!(entry.best_move(), Some(mv));
        assert!(entry.is_pv());
    }

    #[test]
    fn storing_without_a_move_keeps_the_previous_move() {
        let tt = SharedTransTable::new(1);
        let mv = Move::quiet(12, 28);

        tt.store(5, 6, 10, None, Bound::Lower, Some(mv), false);
        tt.store(5, 7, -3, None, Bound::Upper, None, false);

        let entry = tt.probe(5).unwrap();
        assert_eq!(entry.best_move(), Some(mv));
        assert_eq!(entry.bound(), Bound::Upper);
    }

    #[test]
    fn missing_eval_and_move_are_none() {
        let tt = SharedTransTable::new(1);
        tt.store(42, 3, 0, None, Bound::Upper, None, false);
        let entry = tt.probe(42).unwrap();
        assert_eq!(entry.eval(), None);
        assert_eq!(entry.best_move(), None);
    }

    #[test]
    fn probe_misses_unknown_key_and_clear_empties_table() {
        let tt = SharedTransTable::new(1);
        tt.store(7, 1, 10, None, Bound::Exact, None, false);
        assert!(tt.probe(8).is_none());
        tt.clear();
        assert!(tt.probe(7).is_none());
    }

    #[test]
    fn shallower_store_of_same_key_from_older_search_is_ignored() {
        let tt = SharedTransTable::new(1);
        tt.store(99, 10, 1, None, Bound::Exact, None, false);
        tt.tick_age();
        tt.store(99, 4, 2, None, Bound::Exact, None, false);
        assert_eq!(tt.probe(99).unwrap().depth(), 10);
    }

    #[test]
    fn full_cluster_replaces_shallowest_entry() {
        let tt = SharedTransTable::new(1);
        // Small keys all map to cluster 0: the index uses the key's high bits.
        let keys: Vec<u64> = (1..=5).collect();
        for (depth, &key) in [9, 3, 7, 8].iter().zip(&keys) {
            tt.store(key, *depth, 0, None, Bound::Exact, None, false);
        }
        tt.store(keys[4], 5, 0, None, Bound::Exact, None, false);
        assert!(
            tt.probe(keys[1]).is_none(),
            "depth-3 entry should be evicted"
        );
        assert!(tt.probe(keys[4]).is_some());
    }
}
