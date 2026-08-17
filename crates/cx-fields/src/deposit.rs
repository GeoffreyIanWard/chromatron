//! The deposit buffer — the one path from entities into fields.
//!
//! Entities never hold a reference into field storage (`ADR-0003`). They push
//! `(field, chunk, cell, value, op)` into a buffer, drained at a fixed point in
//! the tick (`FieldDeposit`).
//!
//! The ordering rule is what makes it deterministic: entries are sorted by
//! `(field, chunk, cell, op)` before being applied, so parallel producers cannot
//! change the result by finishing in a different order. Without the sort, two
//! `Add`s to one cell would still agree, but an `Add` and a `Set` racing would
//! not — and that difference would show up as a divergence thousands of ticks
//! later.

use cx_core::math::ChunkCoord;

use crate::store::{FieldId, FieldStore};

/// How a deposit combines with the existing value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DepositOp {
    /// Add to the current value.
    Add,
    /// Replace the current value.
    Set,
    /// Keep whichever is larger.
    Max,
}

/// One queued write.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Deposit {
    /// Target field.
    pub field: FieldId,
    /// Target chunk.
    pub chunk: ChunkCoord,
    /// Chunk-local cell x.
    pub x: u32,
    /// Chunk-local cell z.
    pub z: u32,
    /// The value to combine.
    pub value: f32,
    /// How to combine it.
    pub op: DepositOp,
}

/// Accumulates deposits during a tick, applies them at `FieldDeposit`.
///
/// Preallocated and reused: allocation inside a tick is banned
/// (`03-conventions.md`), so [`DepositBuffer::drain_into`] clears without
/// releasing capacity.
#[derive(Debug, Default)]
pub struct DepositBuffer {
    entries: Vec<Deposit>,
}

impl DepositBuffer {
    /// An empty buffer.
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// A buffer sized up front.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: Vec::with_capacity(capacity),
        }
    }

    /// Queues a write.
    pub fn push(&mut self, deposit: Deposit) {
        self.entries.push(deposit);
    }

    /// How many deposits are queued.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is queued.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Merges another buffer's entries. Per-thread buffers combine this way.
    pub fn merge(&mut self, other: &mut DepositBuffer) {
        self.entries.append(&mut other.entries);
    }

    /// Applies every deposit in deterministic order, then clears.
    ///
    /// The sort is the determinism guarantee. `sort_unstable_by` is safe here
    /// because the key includes the op, so entries that compare equal are
    /// genuinely interchangeable.
    pub fn drain_into(&mut self, store: &mut FieldStore) {
        self.entries.sort_unstable_by(|a, b| {
            a.field
                .cmp(&b.field)
                .then(a.chunk.cmp(&b.chunk))
                .then(a.z.cmp(&b.z))
                .then(a.x.cmp(&b.x))
                .then(a.op.cmp(&b.op))
                // Bit pattern, not value: f32 is not Ord, and two deposits
                // differing only in value must still have a total order or the
                // sort is not deterministic.
                .then(a.value.to_bits().cmp(&b.value.to_bits()))
        });

        for deposit in &self.entries {
            let current = store.get(deposit.field, deposit.chunk, deposit.x, deposit.z);
            let combined = match deposit.op {
                DepositOp::Add => current + deposit.value,
                DepositOp::Set => deposit.value,
                DepositOp::Max => current.max(deposit.value),
            };
            store.set(deposit.field, deposit.chunk, deposit.x, deposit.z, combined);
        }

        // Keeps capacity: the next tick reuses this allocation.
        self.entries.clear();
    }
}
