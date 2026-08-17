//! State hashing — the mechanism behind every determinism claim in the doc set.
//!
//! A 128-bit digest per tick over authoritative sim state: registered components
//! and registered fields (S14). Two runs that agree tick by tick agree
//! completely; the first tick where they differ is where a divergence bug lives.
//!
//! # Order independence is the whole design
//!
//! Systems iterate entities in unspecified order, and that order changes with
//! thread count and archetype layout. A hash that depended on it would report
//! divergence between two runs that simulated identically — a detector that
//! cries wolf gets switched off, and then it is not a detector.
//!
//! So per-entity hashes are combined **commutatively**, with `wrapping_add`
//! rather than XOR. XOR is the obvious choice and the wrong one: two entities
//! with identical component values cancel to zero under XOR, so a world with two
//! identical agents hashes the same as a world with none.
//!
//! # Comparable only within one module set
//!
//! A hash carries the module-set fingerprint it was taken under (`ADR-0012`).
//! The same seed with erosion on and off is a different world, so comparing
//! across configurations must fail loudly rather than report a spurious
//! divergence.

use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use cx_core::hash::{combine, mix64};
use cx_ecs::SimWorld;
use cx_fields::{FieldId, FieldStore};

/// A 128-bit digest of authoritative sim state.
///
/// Comparable only against hashes taken with the same module set — the
/// fingerprint is folded in, so a mismatched comparison fails as a difference
/// rather than silently agreeing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct StateHash(pub u128);

impl StateHash {
    /// The hash of nothing.
    pub const EMPTY: StateHash = StateHash(0);

    /// Short form for logs and bisect output.
    pub fn short(self) -> String {
        format!("{:016x}", (self.0 >> 64) as u64)
    }
}

impl std::fmt::Display for StateHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:032x}", self.0)
    }
}

/// A value that contributes to the state hash.
///
/// Implemented per component rather than derived, so that adding a field to a
/// component is a deliberate decision about whether it is authoritative state or
/// a cache. Cached data must not be hashed: it is recomputed on load and would
/// make a save-load round trip look like a divergence.
pub trait StateHashable {
    /// A stable digest of this value.
    ///
    /// Must not depend on addresses, iteration order, or wall-clock time. Floats
    /// hash by bit pattern — `to_bits`, never a cast — so that `-0.0` and `0.0`
    /// are distinguishable and NaN is stable.
    fn state_hash(&self) -> u64;
}

impl StateHashable for f32 {
    fn state_hash(&self) -> u64 {
        mix64(self.to_bits() as u64)
    }
}

impl StateHashable for u32 {
    fn state_hash(&self) -> u64 {
        mix64(*self as u64)
    }
}

impl StateHashable for u64 {
    fn state_hash(&self) -> u64 {
        mix64(*self)
    }
}

impl StateHashable for i32 {
    fn state_hash(&self) -> u64 {
        mix64(*self as u32 as u64)
    }
}

impl StateHashable for cx_core::glam::Vec3 {
    fn state_hash(&self) -> u64 {
        let mut hash = mix64(self.x.to_bits() as u64);
        hash = combine(hash, self.y.to_bits() as u64);
        combine(hash, self.z.to_bits() as u64)
    }
}

type ComponentHasher = fn(&mut SimWorld) -> u128;

/// Computes state hashes over a registered set of components and fields.
#[derive(Default)]
pub struct StateHasher {
    fingerprint: u64,
    components: Vec<(&'static str, ComponentHasher)>,
    fields: Vec<FieldId>,
}

impl std::fmt::Debug for StateHasher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StateHasher")
            .field("fingerprint", &self.fingerprint)
            .field("components", &self.components.len())
            .field("fields", &self.fields.len())
            .finish()
    }
}

impl StateHasher {
    /// A hasher for a given module set.
    ///
    /// The fingerprint is the resolved schedule hash from `cx-module`. Folding it
    /// in is what makes a cross-configuration comparison fail rather than
    /// mislead.
    pub fn new(module_fingerprint: u64) -> Self {
        Self {
            fingerprint: module_fingerprint,
            components: Vec::new(),
            fields: Vec::new(),
        }
    }

    /// Registers a component type as authoritative state.
    pub fn register_component<T>(&mut self, name: &'static str) -> &mut Self
    where
        T: Component + StateHashable,
    {
        self.components.push((name, hash_component::<T>));
        // Sorted by name so registration order cannot reach the digest — the
        // same discipline module resolution uses.
        self.components.sort_by_key(|(name, _)| *name);
        self
    }

    /// Registers a field as authoritative state.
    pub fn register_field(&mut self, field: FieldId) -> &mut Self {
        self.fields.push(field);
        self.fields.sort_unstable();
        self.fields.dedup();
        self
    }

    /// How many components are registered.
    pub fn component_count(&self) -> usize {
        self.components.len()
    }

    /// Hashes the entity world.
    pub fn hash_world(&self, world: &mut SimWorld) -> StateHash {
        let mut digest = mix64(self.fingerprint) as u128;

        for (name, hasher) in &self.components {
            // Component contributions are folded in registration (sorted) order,
            // while entities within a component combine commutatively. Order
            // matters between components and must not matter within one.
            let mut per_component = mix64(name_hash(name)) as u128;
            per_component = per_component.wrapping_add(hasher(world));
            digest = mix_u128(digest, per_component);
        }

        StateHash(digest)
    }

    /// Hashes dense field state.
    ///
    /// Cells are hashed in index order because a field array *is* ordered — the
    /// commutative rule applies to entities, whose iteration order is genuinely
    /// unspecified, not to arrays, where position is part of the value.
    pub fn hash_fields(&self, store: &FieldStore) -> StateHash {
        let mut digest = mix64(self.fingerprint) as u128;

        for field in &self.fields {
            let mut per_field = mix64(field.0 as u64) as u128;

            for chunk in store.chunks() {
                let Some(storage) = store.chunk(*field, *chunk) else {
                    continue;
                };
                let mut per_chunk = mix64(chunk.x as u32 as u64);
                per_chunk = combine(per_chunk, chunk.z as u32 as u64);
                for value in storage.front() {
                    per_chunk = combine(per_chunk, value.to_bits() as u64);
                }
                per_field = mix_u128(per_field, per_chunk as u128);
            }

            digest = mix_u128(digest, per_field);
        }

        StateHash(digest)
    }

    /// Hashes entities and fields together — the per-tick digest.
    pub fn hash_all(&self, world: &mut SimWorld, store: &FieldStore) -> StateHash {
        let entities = self.hash_world(world);
        let fields = self.hash_fields(store);
        StateHash(mix_u128(entities.0, fields.0))
    }
}

/// Folds every entity holding `T` into one commutative digest.
fn hash_component<T: Component + StateHashable>(world: &mut SimWorld) -> u128 {
    let mut query = world.query::<(Entity, &T)>();
    let mut total: u128 = 0;

    for (entity, value) in query.iter(world.inner()) {
        // The entity is folded in so that two entities with equal values do not
        // collapse into one another, and so that moving a value between entities
        // is visible.
        let mut per_entity = mix64(entity.to_bits());
        per_entity = combine(per_entity, value.state_hash());
        total = total.wrapping_add(per_entity as u128);
    }

    total
}

fn name_hash(name: &str) -> u64 {
    let mut hash = mix64(name.len() as u64);
    for byte in name.as_bytes() {
        hash = combine(hash, *byte as u64);
    }
    hash
}

/// Non-commutative 128-bit combine, for parts whose order is meaningful.
fn mix_u128(accumulator: u128, value: u128) -> u128 {
    let high = mix64((accumulator >> 64) as u64 ^ (value >> 64) as u64);
    let low = combine(accumulator as u64, value as u64);
    ((high as u128) << 64) | low as u128
}

#[cfg(test)]
mod tests {
    use super::*;
    use cx_ecs::{SimWorld, WorldConfig};

    #[derive(Component, Clone, Copy)]
    struct Health(f32);

    impl StateHashable for Health {
        fn state_hash(&self) -> u64 {
            self.0.state_hash()
        }
    }

    fn hasher() -> StateHasher {
        let mut hasher = StateHasher::new(0xfeed);
        hasher.register_component::<Health>("Health");
        hasher
    }

    #[test]
    fn identical_worlds_hash_identically() {
        let build = || {
            let mut world = SimWorld::new(WorldConfig::default());
            world.spawn_batch((0..100).map(|i| Health(i as f32)));
            world
        };

        let hasher = hasher();
        assert_eq!(
            hasher.hash_world(&mut build()),
            hasher.hash_world(&mut build())
        );
    }

    #[test]
    fn a_changed_value_changes_the_hash() {
        let hasher = hasher();

        let mut world = SimWorld::new(WorldConfig::default());
        world.spawn_batch((0..100).map(|i| Health(i as f32)));
        let before = hasher.hash_world(&mut world);

        let mut query = world.query::<&mut Health>();
        if let Some(mut health) = query.iter_mut(world.inner_mut()).next() {
            health.0 += 1.0;
        }

        assert_ne!(
            before,
            hasher.hash_world(&mut world),
            "a mutation must be visible"
        );
    }

    #[test]
    fn two_identical_entities_do_not_cancel() {
        // The reason the combine is wrapping_add and not XOR. Under XOR these
        // two worlds would hash the same, and a world with a pair of identical
        // agents would be indistinguishable from an empty one.
        let hasher = hasher();

        let mut empty = SimWorld::new(WorldConfig::default());
        let mut pair = SimWorld::new(WorldConfig::default());
        pair.spawn_batch((0..2).map(|_| Health(1.0)));

        assert_ne!(hasher.hash_world(&mut empty), hasher.hash_world(&mut pair));
    }

    #[test]
    fn the_module_fingerprint_separates_configurations() {
        let build = || {
            let mut world = SimWorld::new(WorldConfig::default());
            world.spawn_batch((0..10).map(|i| Health(i as f32)));
            world
        };

        let mut a = StateHasher::new(1);
        a.register_component::<Health>("Health");
        let mut b = StateHasher::new(2);
        b.register_component::<Health>("Health");

        assert_ne!(
            a.hash_world(&mut build()),
            b.hash_world(&mut build()),
            "the same state under a different module set must not compare equal"
        );
    }

    #[test]
    fn registration_order_does_not_reach_the_digest() {
        #[derive(Component, Clone, Copy)]
        struct Armour(f32);
        impl StateHashable for Armour {
            fn state_hash(&self) -> u64 {
                self.0.state_hash()
            }
        }

        let build = || {
            let mut world = SimWorld::new(WorldConfig::default());
            world.spawn_batch((0..10).map(|i| (Health(i as f32), Armour(i as f32 * 2.0))));
            world
        };

        let mut forward = StateHasher::new(7);
        forward.register_component::<Health>("Health");
        forward.register_component::<Armour>("Armour");

        let mut reverse = StateHasher::new(7);
        reverse.register_component::<Armour>("Armour");
        reverse.register_component::<Health>("Health");

        assert_eq!(
            forward.hash_world(&mut build()),
            reverse.hash_world(&mut build())
        );
    }

    #[test]
    fn float_hashing_distinguishes_signed_zero() {
        // to_bits rather than a cast, so -0.0 and 0.0 are distinguishable and
        // NaN is stable rather than incomparable.
        assert_ne!(0.0f32.state_hash(), (-0.0f32).state_hash());
        assert_eq!(f32::NAN.state_hash(), f32::NAN.state_hash());
    }

    #[test]
    fn short_form_is_stable_and_shorter() {
        let hash = StateHash(0x0123_4567_89ab_cdef_0000_0000_0000_0000);
        assert_eq!(hash.short(), "0123456789abcdef");
        assert_eq!(hash.short().len(), 16);
    }
}
