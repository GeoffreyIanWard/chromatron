//! Windowed client. Assembles the sim world plus the view world and runs the
//! `WindowedDriver` (S03). Nothing here at M0 — the first frame is M1.

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    tracing::info!("chromatron-game: windowed client lands at M1");
    Ok(())
}
