//! Repo invariants that the compiler cannot express.
//!
//! The checks themselves live in `tests/`. This crate exists to own them and to
//! give them a single dependency on `cargo_metadata`.
//!
//! Run with `cargo test -p ci-checks`.
