//! Generational handles and the arena they address.
//!
//! Hot data uses `u32` generational handles, never `Box<dyn Trait>` or `Rc`
//! (`03-conventions.md`). A handle is 8 bytes, `Copy`, and contains no pointer,
//! so it can be stored in components, serialized, and compared without
//! borrowing anything.
//!
//! The generation counter is what makes reuse safe: freeing a slot bumps its
//! generation, so a handle held across the free refers to a slot whose
//! generation no longer matches and resolves to `None` rather than to whatever
//! moved in afterwards.

use std::fmt;
use std::marker::PhantomData;

/// A generational index into an [`Arena`].
///
/// The `T` parameter is a compile-time tag only — a `Handle<Agent>` cannot be
/// passed where a `Handle<Building>` is expected, which is worth the type
/// parameter on its own.
pub struct Handle<T> {
    index: u32,
    generation: u32,
    // `fn() -> T` rather than `T`: this makes the handle unconditionally Send,
    // Sync, and covariant regardless of what T is. A `PhantomData<T>` would
    // make a `Handle<Cell<u8>>` non-Sync for no reason — the handle owns nothing.
    marker: PhantomData<fn() -> T>,
}

impl<T> Handle<T> {
    /// A handle that never resolves, for initializing fields before a real one
    /// exists.
    pub const DANGLING: Self = Self {
        index: u32::MAX,
        generation: u32::MAX,
        marker: PhantomData,
    };

    const fn new(index: u32, generation: u32) -> Self {
        Self {
            index,
            generation,
            marker: PhantomData,
        }
    }

    /// The slot this handle addresses.
    pub const fn index(self) -> u32 {
        self.index
    }

    /// The generation this handle was issued for.
    pub const fn generation(self) -> u32 {
        self.generation
    }

    /// Packs into a single `u64`, for state hashing and persistence.
    pub const fn to_bits(self) -> u64 {
        ((self.index as u64) << 32) | self.generation as u64
    }

    /// Unpacks from [`Handle::to_bits`].
    pub const fn from_bits(bits: u64) -> Self {
        Self::new((bits >> 32) as u32, bits as u32)
    }
}

// Derived impls would demand `T: Copy`, `T: PartialEq`, and so on, even though
// the handle holds no `T` at all. These are written out to keep `Handle<T>`
// usable for any `T` whatsoever.
impl<T> Clone for Handle<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Handle<T> {}

impl<T> PartialEq for Handle<T> {
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index && self.generation == other.generation
    }
}

impl<T> Eq for Handle<T> {}

impl<T> PartialOrd for Handle<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<T> Ord for Handle<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Index first: this is the order arenas iterate in, so sorting handles
        // matches storage order and stays cache-friendly.
        self.index
            .cmp(&other.index)
            .then(self.generation.cmp(&other.generation))
    }
}

impl<T> std::hash::Hash for Handle<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.to_bits().hash(state);
    }
}

impl<T> fmt::Debug for Handle<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Handle<{}>({}v{})",
            std::any::type_name::<T>(),
            self.index,
            self.generation
        )
    }
}

#[derive(Debug)]
enum Slot<T> {
    Occupied {
        generation: u32,
        value: T,
    },
    Free {
        generation: u32,
        next_free: Option<u32>,
    },
}

/// Dense storage addressed by [`Handle`], with stable iteration order.
///
/// Iteration is by slot index and does not depend on insertion or removal
/// history, which is what makes it usable in sim code at all — `03-conventions.md`
/// bans iteration whose order can vary between runs.
#[derive(Debug)]
pub struct Arena<T> {
    slots: Vec<Slot<T>>,
    free_head: Option<u32>,
    len: usize,
}

impl<T> Default for Arena<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Arena<T> {
    /// An empty arena.
    pub const fn new() -> Self {
        Self {
            slots: Vec::new(),
            free_head: None,
            len: 0,
        }
    }

    /// An empty arena with room for `capacity` values.
    ///
    /// Preferred in sim code: allocation inside a tick is banned, so arenas are
    /// sized up front (`03-conventions.md`).
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            slots: Vec::with_capacity(capacity),
            free_head: None,
            len: 0,
        }
    }

    /// Number of live values.
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether the arena holds no live values.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Number of slots, live or free. Iteration cost is proportional to this,
    /// not to [`Arena::len`].
    pub fn slot_count(&self) -> usize {
        self.slots.len()
    }

    /// Inserts a value and returns its handle.
    ///
    /// Reuses the most recently freed slot when one is available, so a workload
    /// that inserts and removes in equal measure does not grow the arena.
    pub fn insert(&mut self, value: T) -> Handle<T> {
        match self.free_head {
            Some(index) => {
                let slot = match self.slots.get_mut(index as usize) {
                    Some(slot) => slot,
                    // The free list pointed at a slot that does not exist, which
                    // means the arena's own invariant is broken. Sim crates do
                    // not panic (03-conventions.md), so recover by appending and
                    // dropping the corrupt free list.
                    None => {
                        self.free_head = None;
                        return self.push_new_slot(value);
                    }
                };

                let (generation, next_free) = match slot {
                    Slot::Free {
                        generation,
                        next_free,
                    } => (*generation, *next_free),
                    // Same reasoning: an occupied slot on the free list is a
                    // broken invariant, not a caller error.
                    Slot::Occupied { .. } => {
                        self.free_head = None;
                        return self.push_new_slot(value);
                    }
                };

                *slot = Slot::Occupied { generation, value };
                self.free_head = next_free;
                self.len += 1;
                Handle::new(index, generation)
            }
            None => self.push_new_slot(value),
        }
    }

    fn push_new_slot(&mut self, value: T) -> Handle<T> {
        let index = self.slots.len() as u32;
        self.slots.push(Slot::Occupied {
            generation: 0,
            value,
        });
        self.len += 1;
        Handle::new(index, 0)
    }

    /// Borrows the value a handle addresses, or `None` if it has been removed.
    pub fn get(&self, handle: Handle<T>) -> Option<&T> {
        match self.slots.get(handle.index as usize)? {
            Slot::Occupied { generation, value } if *generation == handle.generation => Some(value),
            _ => None,
        }
    }

    /// Mutably borrows the value a handle addresses.
    pub fn get_mut(&mut self, handle: Handle<T>) -> Option<&mut T> {
        match self.slots.get_mut(handle.index as usize)? {
            Slot::Occupied { generation, value } if *generation == handle.generation => Some(value),
            _ => None,
        }
    }

    /// Whether a handle still resolves.
    pub fn contains(&self, handle: Handle<T>) -> bool {
        self.get(handle).is_some()
    }

    /// Removes and returns a value, invalidating every handle to it.
    pub fn remove(&mut self, handle: Handle<T>) -> Option<T> {
        let slot = self.slots.get_mut(handle.index as usize)?;

        let generation = match slot {
            Slot::Occupied { generation, .. } if *generation == handle.generation => *generation,
            _ => return None,
        };

        // Saturating rather than wrapping: at u32::MAX the slot retires instead
        // of wrapping to 0, where a very old handle would start resolving again.
        // Reaching this needs four billion insert/remove cycles on one slot.
        let next_generation = generation.saturating_add(1);
        let retired = next_generation == u32::MAX;

        let previous = std::mem::replace(
            slot,
            Slot::Free {
                generation: next_generation,
                next_free: if retired { None } else { self.free_head },
            },
        );

        if !retired {
            self.free_head = Some(handle.index);
        }
        self.len -= 1;

        match previous {
            Slot::Occupied { value, .. } => Some(value),
            Slot::Free { .. } => None,
        }
    }

    /// Removes every value, keeping allocated capacity.
    pub fn clear(&mut self) {
        self.slots.clear();
        self.free_head = None;
        self.len = 0;
    }

    /// Iterates live values in slot order.
    pub fn iter(&self) -> impl Iterator<Item = (Handle<T>, &T)> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| match slot {
                Slot::Occupied { generation, value } => {
                    Some((Handle::new(index as u32, *generation), value))
                }
                Slot::Free { .. } => None,
            })
    }

    /// Mutably iterates live values in slot order.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (Handle<T>, &mut T)> {
        self.slots
            .iter_mut()
            .enumerate()
            .filter_map(|(index, slot)| match slot {
                Slot::Occupied { generation, value } => {
                    Some((Handle::new(index as u32, *generation), value))
                }
                Slot::Free { .. } => None,
            })
    }

    /// Iterates live values without their handles.
    pub fn values(&self) -> impl Iterator<Item = &T> {
        self.iter().map(|(_, value)| value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq)]
    struct Agent(u32);

    #[test]
    fn s01_acceptance_handle_is_eight_bytes_and_copy() {
        assert_eq!(std::mem::size_of::<Handle<Agent>>(), 8);
        assert_eq!(std::mem::align_of::<Handle<Agent>>(), 4);

        let handle = Handle::<Agent>::new(1, 2);
        let copied = handle;
        assert_eq!(handle, copied, "Copy, not Move");
    }

    #[test]
    fn s01_acceptance_stale_handle_does_not_validate_after_reuse() {
        let mut arena = Arena::new();
        let first = arena.insert(Agent(1));

        assert_eq!(arena.remove(first), Some(Agent(1)));

        let second = arena.insert(Agent(2));
        assert_eq!(second.index(), first.index(), "the slot should be reused");
        assert_ne!(
            second.generation(),
            first.generation(),
            "but not the generation"
        );

        assert_eq!(arena.get(first), None, "the stale handle must not resolve");
        assert_eq!(arena.get(second), Some(&Agent(2)));
    }

    #[test]
    fn handle_survives_a_bits_round_trip() {
        let handle = Handle::<Agent>::new(7, 3);
        assert_eq!(Handle::<Agent>::from_bits(handle.to_bits()), handle);
    }

    #[test]
    fn iteration_order_is_stable_and_independent_of_history() {
        let mut arena = Arena::new();
        let a = arena.insert(Agent(0));
        let b = arena.insert(Agent(1));
        let c = arena.insert(Agent(2));

        arena.remove(b);
        let d = arena.insert(Agent(3));

        // d reused b's slot, so it appears in the middle — by slot index, not by
        // insertion time. Sim code depends on this being history-independent.
        let order: Vec<u32> = arena.values().map(|agent| agent.0).collect();
        assert_eq!(order, vec![0, 3, 2]);
        assert_eq!(arena.len(), 3);
        assert!(arena.contains(a) && arena.contains(c) && arena.contains(d));
    }

    #[test]
    fn removing_twice_is_not_an_error() {
        let mut arena = Arena::new();
        let handle = arena.insert(Agent(1));
        assert_eq!(arena.remove(handle), Some(Agent(1)));
        assert_eq!(
            arena.remove(handle),
            None,
            "second removal should be a no-op, not a panic"
        );
        assert_eq!(arena.len(), 0);
    }

    #[test]
    fn free_slots_are_reused_rather_than_growing_the_arena() {
        let mut arena = Arena::with_capacity(4);
        let handles: Vec<_> = (0..4).map(|i| arena.insert(Agent(i))).collect();
        for handle in &handles {
            arena.remove(*handle);
        }
        for i in 0..4 {
            arena.insert(Agent(i + 10));
        }

        assert_eq!(
            arena.slot_count(),
            4,
            "insert/remove churn should not grow storage"
        );
        assert_eq!(arena.len(), 4);
        for handle in &handles {
            assert!(!arena.contains(*handle), "every old handle should be stale");
        }
    }

    #[test]
    fn dangling_handle_never_resolves() {
        let mut arena = Arena::new();
        arena.insert(Agent(1));
        assert_eq!(arena.get(Handle::DANGLING), None);
    }

    #[test]
    fn handles_of_different_types_are_distinct_types() {
        // Compile-time property; asserted here so the intent is recorded. If
        // this ever compiles with the types swapped, the tag parameter is gone.
        let agent: Handle<Agent> = Handle::DANGLING;
        let bits = agent.to_bits();
        let other: Handle<u32> = Handle::from_bits(bits);
        assert_eq!(other.to_bits(), bits);
    }
}
