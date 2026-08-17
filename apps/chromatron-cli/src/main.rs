//! Headless entry point: batch runs, parameter sweeps, benchmarks, and the
//! architecture graph export (S21).
//!
//! Never constructs a view world (`ADR-0002`).

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    tracing::info!("chromatron-cli: no subcommands implemented yet (M0)");
    Ok(())
}
