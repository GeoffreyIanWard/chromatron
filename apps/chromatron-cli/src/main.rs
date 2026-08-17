//! Headless entry point: batch runs, parameter sweeps, benchmarks, and the
//! architecture graph export (S21).
//!
//! Never constructs a view world (`ADR-0002`).

use std::path::PathBuf;

use anyhow::Context as _;
use clap::{Parser, Subcommand};
use cx_module::Registry;

#[derive(Parser)]
#[command(
    name = "chromatron",
    about = "Headless runner for the CHROMATRON engine"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Export the resolved architecture graph as JSON (S21).
    ///
    /// Builds the module set for a profile and serializes what resolution
    /// produced. Runs no ticks, so it is fast enough for every commit.
    Graph {
        /// Named profile to resolve (S20).
        #[arg(long, default_value = "minimal")]
        profile: String,

        /// Where to write. Defaults to stdout.
        #[arg(long)]
        out: Option<PathBuf>,

        /// Compare against a previously exported graph and report differences.
        #[arg(long)]
        baseline: Option<PathBuf>,
    },
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    match Cli::parse().command {
        Command::Graph {
            profile,
            out,
            baseline,
        } => graph(&profile, out, baseline),
    }
}

fn graph(profile: &str, out: Option<PathBuf>, baseline: Option<PathBuf>) -> anyhow::Result<()> {
    // Profile membership lives in cx-sim: naming modules is the facade's job,
    // and cx-module must not depend on the subsystems implementing its trait.
    let selected = cx_sim::by_name(profile).with_context(|| {
        format!(
            "unknown profile `{profile}`; known profiles are {}",
            cx_sim::NAMES.join(", ")
        )
    })?;

    let mut registry = Registry::new();
    selected.register_into(&mut registry);

    let resolved = registry
        .resolve()
        .map_err(|error| anyhow::anyhow!("module resolution failed: {error}"))?;

    let payload = cx_module::export(&resolved);

    if let Some(path) = baseline {
        let previous = std::fs::read_to_string(&path)
            .with_context(|| format!("reading baseline {}", path.display()))?;
        report_diff(&previous, &payload);
    }

    match out {
        Some(path) => {
            std::fs::write(&path, &payload)
                .with_context(|| format!("writing {}", path.display()))?;
            tracing::info!(path = %path.display(), "graph written");
        }
        None => print!("{payload}"),
    }

    Ok(())
}

/// Reports added and removed lines between two exports.
///
/// Line-based because the export is deterministic and one element per line: a
/// structural diff would be more precise and is not yet worth the dependency.
/// Annotates rather than failing, per S21 — a check that blocks every legitimate
/// architecture change gets switched off within a month.
fn report_diff(previous: &str, current: &str) {
    let before: Vec<&str> = previous.lines().collect();
    let after: Vec<&str> = current.lines().collect();

    let removed: Vec<&str> = before
        .iter()
        .filter(|line| !after.contains(line))
        .copied()
        .collect();
    let added: Vec<&str> = after
        .iter()
        .filter(|line| !before.contains(line))
        .copied()
        .collect();

    if removed.is_empty() && added.is_empty() {
        tracing::info!("graph unchanged against baseline");
        return;
    }

    for line in &removed {
        tracing::warn!(change = "removed", element = line.trim());
    }
    for line in &added {
        tracing::warn!(change = "added", element = line.trim());
    }
    tracing::warn!(
        removed = removed.len(),
        added = added.len(),
        "graph changed against baseline"
    );
}
