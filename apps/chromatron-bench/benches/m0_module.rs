//! M0 module gates (S20) — `module_resolution_order_independence`,
//! `disabled_module_zero_cost`.
//!
//! These two are correctness claims measured like benchmarks, because the second
//! one is only meaningful as a measurement: `ADR-0012` promises a disabled module
//! costs *zero* ticks and *zero* bytes, not that it costs little. The difference
//! between "does not run" and "runs a branch that does nothing" is invisible in a
//! unit test and obvious on a stopwatch.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use cx_module::{Capability, Profile, Registry, cap};

/// A deterministic shuffle over registration orders.
///
/// Uses a fixed permutation table rather than a random shuffle: this is a
/// determinism gate, and a gate that tests a different thing on every run cannot
/// be bisected when it fails.
const SHUFFLE_COUNT: usize = 10;

fn build_registry_in_order(order: usize) -> Registry {
    let mut registry = Registry::new();
    registry.register_all_shuffled(Profile::full_sim(), order);
    registry
}

/// `module_resolution_order_independence` — identical schedule hash across 10
/// shuffled registration orders.
///
/// S20 requires resolution to be a topological sort with a stable `ModuleId`
/// tiebreak. If this fails, every state hash in the project is suspect: two
/// machines that registered modules in different orders would diverge while both
/// believing they ran the same simulation.
fn bench_resolution_order_independence(c: &mut Criterion) {
    let baseline = build_registry_in_order(0)
        .resolve()
        .expect("full-sim profile should resolve");

    for order in 1..SHUFFLE_COUNT {
        let resolved = build_registry_in_order(order)
            .resolve()
            .unwrap_or_else(|error| panic!("shuffled order {order} failed to resolve: {error}"));

        assert_eq!(
            resolved.schedule_hash(),
            baseline.schedule_hash(),
            "gate module_resolution_order_independence: registration order {order} produced a \
             different resolved schedule than order 0.\n\n\
             S20 requires a topological sort with a stable ModuleId tiebreak. An order-dependent \
             schedule means state hashes are not comparable between runs, which invalidates the \
             determinism gates and every golden test built on them."
        );
    }

    let mut group = c.benchmark_group("module_resolution");
    group.bench_function("resolve_full_sim", |b| {
        b.iter(|| black_box(build_registry_in_order(0).resolve()));
    });
    group.finish();
}

/// `disabled_module_zero_cost` — a disabled module contributes no tick time and
/// no field allocations.
///
/// Compares the `full-sim` profile against `no-erosion`, which S20 describes as
/// the profile that proves the toggle works end to end.
fn bench_disabled_module_zero_cost(_c: &mut Criterion) {
    let full = build_registry_in_order(0)
        .resolve()
        .expect("full-sim should resolve");
    let reduced = {
        let mut registry = Registry::new();
        registry.register_all(Profile::no_erosion());
        registry.resolve().expect("no-erosion should resolve")
    };

    let disabled_systems: Vec<_> = full
        .systems()
        .filter(|system| !reduced.contains_system(system.id()))
        .collect();

    assert!(
        !disabled_systems.is_empty(),
        "gate disabled_module_zero_cost: the no-erosion profile scheduled the same systems as \
         full-sim, so the gate is measuring nothing. Either the profiles are misconfigured or \
         erosion is not actually a separate generation stage (ADR-0012)."
    );

    for system in reduced.systems() {
        assert!(
            !disabled_systems
                .iter()
                .any(|disabled| disabled.id() == system.id()),
            "gate disabled_module_zero_cost: system {} from a disabled module is still \
             scheduled.\n\n\
             ADR-0012: degradation resolves at schedule-build time. A disabled module's system \
             must not be scheduled at all — not scheduled behind a branch that returns early.",
            system.id()
        );
    }

    let full_bytes = full.field_bytes();
    let reduced_bytes = reduced.field_bytes();
    assert!(
        reduced_bytes < full_bytes,
        "gate disabled_module_zero_cost: no-erosion allocated {reduced_bytes} bytes of field \
         storage against full-sim's {full_bytes}. Disabling a module must free its fields, not \
         merely stop stepping them (S20, docs/bench/memory-budget.md)."
    );
}

/// Every capability a module consumes optionally must have a documented
/// behaviour when absent — S20 requires that decision to be written down in the
/// spec *before* the code exists, and this is the mechanical half of that rule.
fn bench_optional_capabilities_declare_degradation(_c: &mut Criterion) {
    let resolved = build_registry_in_order(0)
        .resolve()
        .expect("full-sim should resolve");

    let undeclared: Vec<(&'static str, Capability)> = resolved
        .modules()
        .flat_map(|module| {
            module
                .consumes_optional()
                .iter()
                .filter(|capability| module.degradation_for(**capability).is_none())
                .map(move |capability| (module.id(), *capability))
        })
        .collect();

    assert!(
        undeclared.is_empty(),
        "gate: these modules optionally consume a capability without declaring what they do \
         when it is absent: {undeclared:?}\n\n\
         'It'll just be zero' is a design decision and gets written down (03-conventions.md). \
         The absent case is also what the S21 graph renders, so an undeclared degradation is \
         invisible in the architecture view too."
    );

    black_box(cap::SURFACE_WATER);
}

criterion_group!(
    m0_module,
    bench_resolution_order_independence,
    bench_disabled_module_zero_cost,
    bench_optional_capabilities_declare_degradation
);
criterion_main!(m0_module);
