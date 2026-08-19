//! What the curated profiles actually resolve to (S20, S21).
//!
//! These are the checks that stop a profile from quietly becoming a different
//! configuration than its name and documentation claim — which matters most for
//! benchmarks, where a profile silently losing a module turns a regression into
//! an improvement.

// An integration test is its own crate, so the lib's `cfg_attr(test, ...)`
// exception does not reach it and the sim lint set applies in full. Tests are
// the documented exception (`03-conventions.md`).
#![allow(clippy::expect_used, clippy::panic)]

use cx_module::{Registry, Resolved, writers_of};

fn resolve(name: &str) -> Resolved {
    let profile = cx_sim::by_name(name).expect("the profile exists");
    let mut registry = Registry::new();
    profile.register_into(&mut registry);
    registry.resolve().expect("the profile should resolve")
}

#[test]
fn every_named_profile_resolves() {
    // A profile that does not resolve is a startup failure for whoever selects
    // it, discovered at run time rather than here.
    for name in cx_sim::NAMES {
        let profile = cx_sim::by_name(name).expect("the name is in NAMES");
        let mut registry = Registry::new();
        profile.register_into(&mut registry);

        assert!(
            registry.resolve().is_ok(),
            "profile `{name}` failed to resolve"
        );
    }
}

/// **`ADR-0011` permits exactly two writers to `ELEVATION`.**
///
/// Generation (S07) and terrain edits (S19). A third is a defect rather than a
/// change, and S21 makes this the one graph assertion that hard-fails rather
/// than merely annotating.
///
/// S19 does not exist yet, so today there is one. The check is written as an
/// upper bound plus a named expectation, so it fails when a *third* appears
/// rather than needing to be edited when the second does.
#[test]
fn elevation_has_no_more_than_the_two_permitted_writers() {
    let resolved = resolve("full-sim");
    let writers = writers_of(&resolved, "ELEVATION");

    assert!(
        writers.contains(&"generate_elevation"),
        "generation must be a declared writer of ELEVATION, found {writers:?}"
    );
    assert!(
        writers.len() <= 2,
        "ADR-0011 permits exactly two writers of ELEVATION — generation and \
         terrain edits — but found {}: {writers:?}",
        writers.len()
    );
}

#[test]
fn terrain_is_more_than_minimal() {
    // The profiles were identical until worldgen became a module, which made
    // `--profile terrain` a name that resolved to something else entirely.
    let minimal = resolve("minimal");
    let terrain = resolve("terrain");

    assert!(
        terrain.modules().count() > minimal.modules().count(),
        "terrain should add modules to minimal"
    );
    assert!(
        terrain.systems().count() > minimal.systems().count(),
        "terrain should add systems to minimal"
    );
}

#[test]
fn a_profile_resolves_identically_every_time() {
    // The property the S21 export and the S20 schedule hash both rest on. Two
    // resolutions of one profile must agree, or a graph diff reports changes
    // that are really just resolution order.
    let first = resolve("full-sim");
    let second = resolve("full-sim");

    assert_eq!(cx_module::export(&first), cx_module::export(&second));
}

#[test]
fn an_unknown_profile_is_an_error_rather_than_a_default() {
    // A typo in `--profile` silently resolving to something else is how a
    // benchmark ends up measuring a configuration nobody chose.
    assert!(cx_sim::by_name("terrian").is_none());
    assert!(cx_sim::by_name("").is_none());
}
