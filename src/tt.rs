use crate::types::{Move, ZKey};
use num_cpus;
use std::sync::{Arc, Mutex};

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
    fn from_u8(val: u8) -> Self {
        match val {
            0 => Bound::Exact,
            1 => Bound::Lower,
            _ => Bound::Upper,
        }
    }
}

// A compact 16-Byte TTEntry.
#[derive(Copy, Clone, Debug, Default)]
#[repr(C)]
pub struct TTEntry {
    pub key: ZKey,
    data: u64,
}

// Bitfield layout for the 64-bit data field:
const SCORE_SHIFT: u64 = 0;
const MOVE_SHIFT: u64 = 16;
const DEPTH_SHIFT: u64 = 32;
const AGE_SHIFT: u64 = 40;
const BOUND_SHIFT: u64 = 48;

const SCORE_MASK: u64 = 0xFFFF;
const MOVE_MASK: u64 = 0xFFFF;
const DEPTH_MASK: u64 = 0xFF;
const AGE_MASK: u64 = 0xFF;
const BOUND_MASK: u64 = 0x3;

impl TTEntry {
    fn new(
        key: ZKey,
        depth: i16,
        score: i32,
        bound: Bound,
        best_move: Option<Move>,
        age: u8,
    ) -> Self {
        let packed_score = (score as i16) as u16 as u64;
        let packed_depth = (depth as u8) as u64;
        let packed_move = best_move.map_or(0u16, |m| m.into()) as u64;
        let packed_age = age as u64;
        let packed_bound = bound as u8 as u64;

        let data = (packed_score << SCORE_SHIFT)
            | (packed_move << MOVE_SHIFT)
            | (packed_depth << DEPTH_SHIFT)
            | (packed_age << AGE_SHIFT)
            | (packed_bound << BOUND_SHIFT);

        Self { key, data }
    }

    #[inline(always)]
    pub fn score(&self) -> i32 {
        (((self.data >> SCORE_SHIFT) & SCORE_MASK) as i16) as i32
    }
    #[inline(always)]
    pub fn depth(&self) -> i16 {
        ((self.data >> DEPTH_SHIFT) & DEPTH_MASK) as u8 as i16
    }
    #[inline(always)]
    pub fn best_move(&self) -> Option<Move> {
        let packed_move = ((self.data >> MOVE_SHIFT) & MOVE_MASK) as u16;
        if packed_move == 0 {
            None
        } else {
            Some(packed_move.into())
        }
    }
    #[inline(always)]
    pub fn age(&self) -> u8 {
        ((self.data >> AGE_SHIFT) & AGE_MASK) as u8
    }
    #[inline(always)]
    pub fn bound(&self) -> Bound {
        Bound::from_u8(((self.data >> BOUND_SHIFT) & BOUND_MASK) as u8)
    }
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.key == 0
    }
}

const CLUSTER_SIZE: usize = 4;

#[derive(Copy, Clone, Debug, Default)]
#[repr(align(64))] // Align cluster to a 64-byte cache line
pub struct TTCluster {
    entries: [TTEntry; CLUSTER_SIZE],
}

pub struct TransTable {
    slots: Vec<TTCluster>,
    mask: usize,
    age: u8,
}

impl TransTable {
    fn with_mb(mb: usize) -> Self {
        let bytes = mb.saturating_mul(1024 * 1024).max(64);
        let slot_size = std::mem::size_of::<TTCluster>();
        let slots_count = (bytes / slot_size).max(1).next_power_of_two();
        let mask = slots_count - 1;
        Self {
            slots: vec![TTCluster::default(); slots_count],
            mask,
            age: 0,
        }
    }

    #[inline]
    fn tick_age(&mut self) {
        self.age = self.age.wrapping_add(1);
    }
    #[inline]
    fn idx(&self, key: ZKey) -> usize {
        (key as usize) & self.mask
    }

    #[inline]
    fn clear(&mut self) {
        self.slots
            .iter_mut()
            .for_each(|s| *s = TTCluster::default());
        self.tick_age();
    }

    #[inline]
    fn probe(&self, key: ZKey) -> Option<TTEntry> {
        let idx = self.idx(key);

        // Prefetch the cluster into cache
        #[cfg(target_arch = "x86_64")]
        unsafe {
            let ptr = self.slots.as_ptr().add(idx);
            _mm_prefetch(ptr as *const i8, _MM_HINT_T0);
        }

        let cluster = &self.slots[idx];
        for entry in &cluster.entries {
            if entry.key == key {
                return Some(*entry);
            }
        }
        None
    }

    #[inline]
    fn store(&mut self, key: ZKey, depth: i16, score: i32, bound: Bound, best_move: Option<Move>) {
        let i = self.idx(key);
        let cluster = &mut self.slots[i];
        let new_entry = TTEntry::new(key, depth, score, bound, best_move, self.age);

        for entry in &mut cluster.entries {
            if entry.key == key {
                if self.age == entry.age() || new_entry.depth() >= entry.depth() {
                    *entry = new_entry;
                }
                return;
            }
        }

        for entry in &mut cluster.entries {
            if entry.is_empty() {
                *entry = new_entry;
                return;
            }
        }

        let mut replace_idx = 0;
        let mut worst_quality = i32::MAX;
        for (i, entry) in cluster.entries.iter().enumerate() {
            let quality = (entry.depth() as i32) * 2 - (self.age.wrapping_sub(entry.age()) as i32);
            if quality < worst_quality {
                worst_quality = quality;
                replace_idx = i;
            }
        }
        cluster.entries[replace_idx] = new_entry;
    }

    #[inline]
    fn stats(&self) -> (usize, usize) {
        let filled = self
            .slots
            .iter()
            .map(|c| c.entries.iter().filter(|e| !e.is_empty()).count())
            .sum();
        (filled, self.slots.len() * CLUSTER_SIZE)
    }
}

#[derive(Clone)]
pub struct SharedTransTable {
    shards: Vec<Arc<Mutex<TransTable>>>,
    shard_mask: usize,
}

impl SharedTransTable {
    pub fn new(size_mb: usize) -> Self {
        let shard_count = Self::pick_shard_count();
        let (per_shard, remainder) = if shard_count == 0 {
            (size_mb, 0)
        } else {
            (size_mb / shard_count, size_mb % shard_count)
        };
        let mut shards = Vec::with_capacity(shard_count.max(1));
        let count = shard_count.max(1);
        for i in 0..count {
            shards.push(Arc::new(Mutex::new(TransTable::with_mb(
                (per_shard + if i < remainder { 1 } else { 0 }).max(1),
            ))));
        }
        Self {
            shards,
            shard_mask: count.saturating_sub(1),
        }
    }

    #[inline]
    fn pick_shard_count() -> usize {
        (num_cpus::get().max(1) / 8 + 1).next_power_of_two().min(8)
    }

    #[inline]
    fn shard_index(&self, key: ZKey) -> usize {
        let mut x = key;
        x ^= x >> 33;
        x = x.wrapping_mul(0xff51afd7ed558ccd);
        x ^= x >> 33;
        x = x.wrapping_mul(0xc4ceb9fe1a85ec53);
        x ^= x >> 33;
        (x as usize) & self.shard_mask
    }

    #[inline]
    fn shard_for(&self, key: ZKey) -> &Arc<Mutex<TransTable>> {
        let idx = if self.shards.len().is_power_of_two() {
            self.shard_index(key)
        } else {
            (key as usize) % self.shards.len()
        };
        &self.shards[idx]
    }

    #[inline]
    pub fn probe(&self, key: ZKey) -> Option<TTEntry> {
        self.shard_for(key).lock().unwrap().probe(key)
    }

    #[inline]
    pub fn store(&self, key: ZKey, depth: i16, score: i32, bound: Bound, best_move: Option<Move>) {
        self.shard_for(key)
            .lock()
            .unwrap()
            .store(key, depth, score, bound, best_move);
    }

    #[inline]
    pub fn clear(&self) {
        for shard in &self.shards {
            shard.lock().unwrap().clear();
        }
    }

    #[inline]
    pub fn tick_age(&self) {
        for shard in &self.shards {
            shard.lock().unwrap().tick_age();
        }
    }

    #[inline]
    pub fn hashfull_permill(&self) -> u32 {
        let (filled_total, slots_total) = self
            .shards
            .iter()
            .map(|s| s.lock().unwrap().stats())
            .fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1));
        if slots_total == 0 {
            0
        } else {
            ((filled_total * 1000) / slots_total) as u32
        }
    }
}
