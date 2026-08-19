//! The frame-time history behind the frame graph.
//!
//! A fixed ring of the last N frame times, plus the statistics worth looking at.
//! Separated from the drawing because *which* number the graph reports is the
//! part that can be wrong, and none of it needs a window.
//!
//! # Percentiles, not averages
//!
//! A mean frame time hides exactly the thing a frame graph is for. Sixty frames
//! at 8 ms and one at 200 ms averages to 11 ms and reads as fine; the 200 ms
//! frame is the one the player noticed. The graph reports the worst and the 99th
//! percentile alongside the median for that reason.

/// Frames of history kept.
///
/// Two seconds at 120 Hz: long enough to show a hitch after it happens and
/// short enough that the graph is about now rather than about the session.
pub const HISTORY: usize = 240;

/// A ring of recent frame times, in milliseconds.
#[derive(Debug, Clone)]
pub struct FrameGraph {
    samples: Vec<f32>,
    next: usize,
    filled: usize,
}

impl Default for FrameGraph {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameGraph {
    /// An empty history.
    pub fn new() -> Self {
        Self {
            samples: vec![0.0; HISTORY],
            next: 0,
            filled: 0,
        }
    }

    /// Records one frame.
    ///
    /// Non-finite and negative values are dropped rather than stored. A `NaN` in
    /// the ring poisons every percentile that touches it, and the first symptom
    /// is a frame graph that reads `NaN ms` forever with no clue which frame did
    /// it.
    pub fn push(&mut self, milliseconds: f32) {
        if !milliseconds.is_finite() || milliseconds < 0.0 {
            return;
        }

        if let Some(slot) = self.samples.get_mut(self.next) {
            *slot = milliseconds;
        }
        self.next = (self.next + 1) % HISTORY;
        self.filled = (self.filled + 1).min(HISTORY);
    }

    /// How many frames are recorded.
    pub const fn len(&self) -> usize {
        self.filled
    }

    /// Whether nothing has been recorded.
    pub const fn is_empty(&self) -> bool {
        self.filled == 0
    }

    /// The samples in chronological order, oldest first.
    ///
    /// Ordered because a graph drawn from the raw ring would show the newest
    /// frame in whatever column the write cursor happens to be at, and the
    /// history would appear to scroll backwards through itself.
    pub fn ordered(&self) -> Vec<f32> {
        if self.filled < HISTORY {
            return self.samples.get(..self.filled).unwrap_or_default().to_vec();
        }

        let (head, tail) = self.samples.split_at(self.next);
        let mut ordered = Vec::with_capacity(HISTORY);
        ordered.extend_from_slice(tail);
        ordered.extend_from_slice(head);
        ordered
    }

    /// The most recent frame time.
    pub fn last(&self) -> Option<f32> {
        if self.filled == 0 {
            return None;
        }
        let index = (self.next + HISTORY - 1) % HISTORY;
        self.samples.get(index).copied()
    }

    /// Median, 99th percentile, and worst, in milliseconds.
    pub fn summary(&self) -> Option<GraphSummary> {
        if self.filled == 0 {
            return None;
        }

        let mut sorted = self.ordered();
        sorted.sort_by(f32::total_cmp);

        Some(GraphSummary {
            median: percentile(&sorted, 0.50),
            p99: percentile(&sorted, 0.99),
            worst: sorted.last().copied().unwrap_or(0.0),
            frames: self.filled,
        })
    }
}

/// What the history says.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GraphSummary {
    /// Median frame time in milliseconds.
    pub median: f32,
    /// 99th percentile frame time in milliseconds.
    pub p99: f32,
    /// Worst frame time in milliseconds.
    pub worst: f32,
    /// Frames the summary covers.
    pub frames: usize,
}

/// The value at `fraction` through a sorted slice.
///
/// Nearest-rank rather than interpolated: the answer is then always a frame that
/// actually happened, which is what makes "the 99th percentile was 34 ms"
/// something you can go and look for.
fn percentile(sorted: &[f32], fraction: f32) -> f32 {
    if sorted.is_empty() {
        return 0.0;
    }
    let index = ((sorted.len() as f32 - 1.0) * fraction).round() as usize;
    sorted.get(index).copied().unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_graph_reports_nothing_rather_than_zero() {
        let graph = FrameGraph::new();
        assert!(graph.is_empty());
        assert_eq!(graph.summary(), None);
        assert_eq!(graph.last(), None);
        assert!(graph.ordered().is_empty());
    }

    #[test]
    fn samples_come_back_oldest_first_before_the_ring_wraps() {
        let mut graph = FrameGraph::new();
        for value in [1.0, 2.0, 3.0] {
            graph.push(value);
        }

        assert_eq!(graph.ordered(), vec![1.0, 2.0, 3.0]);
        assert_eq!(graph.last(), Some(3.0));
        assert_eq!(graph.len(), 3);
    }

    #[test]
    fn samples_stay_in_order_after_the_ring_wraps() {
        // The bug this catches: drawing straight from the ring puts the newest
        // frame at the write cursor, and the graph appears to scroll backwards
        // through itself once every two seconds.
        let mut graph = FrameGraph::new();
        for index in 0..(HISTORY + 5) {
            graph.push(index as f32);
        }

        let ordered = graph.ordered();
        assert_eq!(ordered.len(), HISTORY);
        assert_eq!(
            ordered.first().copied(),
            Some(5.0),
            "the oldest surviving sample should be first"
        );
        assert_eq!(
            ordered.last().copied(),
            Some((HISTORY + 4) as f32),
            "the newest sample should be last"
        );
        assert_eq!(graph.last(), Some((HISTORY + 4) as f32));

        // And it is genuinely monotonic, not merely right at the ends.
        for pair in ordered.windows(2) {
            let [earlier, later] = pair else { continue };
            assert!(later > earlier, "history should read forwards in time");
        }
    }

    #[test]
    fn a_nan_never_enters_the_history() {
        // One NaN makes every percentile NaN forever, and the graph reads
        // `NaN ms` with no indication of which frame poisoned it.
        let mut graph = FrameGraph::new();
        graph.push(5.0);
        graph.push(f32::NAN);
        graph.push(f32::INFINITY);
        graph.push(-1.0);
        graph.push(7.0);

        assert_eq!(graph.len(), 2, "only the two real samples should be kept");
        let summary = graph.summary().expect("there are samples");
        assert!(summary.median.is_finite());
        assert!(summary.worst.is_finite());
    }

    #[test]
    fn the_summary_reports_the_spike_rather_than_averaging_it_away() {
        // The whole reason percentiles: sixty good frames and one terrible one
        // averages to "fine", and the terrible one is the only one anybody saw.
        let mut graph = FrameGraph::new();
        for _ in 0..99 {
            graph.push(8.0);
        }
        graph.push(200.0);

        let summary = graph.summary().expect("there are samples");
        assert!((summary.median - 8.0).abs() < f32::EPSILON);
        assert!(
            (summary.worst - 200.0).abs() < f32::EPSILON,
            "the spike must survive into the summary, got {}",
            summary.worst
        );
        assert_eq!(summary.frames, 100);

        let mean: f32 = 8.0 * 0.99 + 200.0 * 0.01;
        assert!(
            summary.worst > mean * 10.0,
            "the mean would have hidden this: mean {mean}, worst {}",
            summary.worst
        );
    }

    #[test]
    fn a_percentile_is_always_a_frame_that_happened() {
        // Nearest-rank, not interpolated: "the p99 was 34 ms" should name a
        // frame you can go and find, not a number between two of them.
        let sorted = [1.0, 2.0, 3.0, 4.0, 100.0];
        for fraction in [0.0, 0.25, 0.5, 0.99, 1.0] {
            let value = percentile(&sorted, fraction);
            assert!(
                sorted.contains(&value),
                "p{fraction} gave {value}, which is not one of the samples"
            );
        }
        assert!((percentile(&sorted, 0.0) - 1.0).abs() < f32::EPSILON);
        assert!((percentile(&sorted, 1.0) - 100.0).abs() < f32::EPSILON);
    }

    #[test]
    fn the_history_is_bounded() {
        let mut graph = FrameGraph::new();
        for _ in 0..(HISTORY * 3) {
            graph.push(1.0);
        }
        assert_eq!(graph.len(), HISTORY);
        assert_eq!(graph.ordered().len(), HISTORY);
    }
}
