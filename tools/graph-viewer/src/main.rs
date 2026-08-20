//! The S21 isometric architecture viewer.
//!
//! Turns an exported graph into a standalone HTML page. Not engine code: this
//! lives under `tools/`, ships in nothing, and is on neither side of the
//! firewall.
//!
//! ```text
//! chromatron-cli graph --profile full-sim --out graph.json
//! graph-viewer --graph graph.json --out graph.html
//! open graph.html
//! ```

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;

mod diff;
mod graph;
mod layout;
mod render;

use crate::diff::Diff;
use crate::graph::Graph;

/// Renders an exported architecture graph.
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Cli {
    /// The graph to render, from `chromatron-cli graph`.
    #[arg(long)]
    graph: PathBuf,

    /// Where to write the page. Defaults to the graph's name with `.html`.
    #[arg(long)]
    out: Option<PathBuf>,

    /// A previously exported graph to compare against.
    #[arg(long)]
    baseline: Option<PathBuf>,

    /// Exit non-zero if the graph breaks a rule rather than merely changing.
    ///
    /// Off by default, per S21: a diff that blocks merges on every legitimate
    /// architecture change gets switched off within a month. The only rule that
    /// hard-fails is `ADR-0011`'s writer limit on `ELEVATION`.
    #[arg(long)]
    strict: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let source = std::fs::read_to_string(&cli.graph)
        .with_context(|| format!("reading {}", cli.graph.display()))?;
    let graph = Graph::parse(&source).with_context(|| format!("in {}", cli.graph.display()))?;

    let baseline = match &cli.baseline {
        Some(path) => {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("reading baseline {}", path.display()))?;
            Some(Graph::parse(&text).with_context(|| format!("in baseline {}", path.display()))?)
        }
        None => None,
    };

    let comparison = baseline
        .as_ref()
        .map(|before| Diff::between(before, &graph));

    let page = render::render(&graph, &layout::layout(&graph), comparison.as_ref());

    let out = cli.out.unwrap_or_else(|| cli.graph.with_extension("html"));
    std::fs::write(&out, &page).with_context(|| format!("writing {}", out.display()))?;

    println!("wrote {}", out.display());

    if let Some(comparison) = &comparison {
        // Reported on the console as well as on the page: a pull request
        // comment is read far more often than an artifact is opened.
        if comparison.is_empty() {
            println!("no architectural change against the baseline");
        } else {
            println!("{}", strip_tags(&comparison.summary_html()));
        }
    }

    let violations = Diff::violations(&graph);
    for violation in &violations {
        eprintln!("error: {violation}");
    }

    if cli.strict && !violations.is_empty() {
        anyhow::bail!("{} rule violation(s)", violations.len());
    }

    Ok(())
}

/// The summary without its markup, for the console.
fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut inside = false;
    for character in html.chars() {
        match character {
            '<' => inside = true,
            '>' => inside = false,
            other if !inside => out.push(other),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_are_stripped_for_the_console() {
        assert_eq!(strip_tags("<b>Changed.</b> 1 added"), "Changed. 1 added");
        assert_eq!(strip_tags("plain"), "plain");
    }
}
