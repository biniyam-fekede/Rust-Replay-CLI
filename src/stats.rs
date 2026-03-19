use crate::models::{AggregatedStats, ReplayOutcome};
use hdrhistogram::Histogram;
use std::collections::HashMap;
use std::time::Instant;
use tokio::sync::mpsc;

pub struct StatsAccumulator {
    pub total_lines: u64,
    pub parse_errors: u64,
    pub filtered_count: u64,
    pub attempted: u64,
    pub succeeded: u64,
    pub failed: u64,
    pub status_distribution: HashMap<u16, u64>,
    pub error_categories: HashMap<String, u64>,
    pub histogram: Histogram<u64>,
    pub start_time: Instant,
}

impl StatsAccumulator {
    pub fn new() -> Self {
        let histogram = Histogram::<u64>::new_with_bounds(1, 60_000_000, 3)
            .expect("Failed to create histogram");
        StatsAccumulator {
            total_lines: 0,
            parse_errors: 0,
            filtered_count: 0,
            attempted: 0,
            succeeded: 0,
            failed: 0,
            status_distribution: HashMap::new(),
            error_categories: HashMap::new(),
            histogram,
            start_time: Instant::now(),
        }
    }

    pub fn record_outcome(&mut self, outcome: &ReplayOutcome) {
        self.attempted += 1;

        if outcome.was_dry_run {
            self.succeeded += 1;
            return;
        }

        match outcome.status {
            Some(status) => {
                *self.status_distribution.entry(status).or_insert(0) += 1;
                if (200..300).contains(&status) {
                    self.succeeded += 1;
                } else {
                    self.failed += 1;
                }
            }
            None => {
                self.failed += 1;
            }
        }

        if let Some(ref err) = outcome.error {
            let category = categorize_error(err);
            *self.error_categories.entry(category).or_insert(0) += 1;
        }

        // Record latency
        if outcome.latency_us > 0 {
            let val = outcome.latency_us.min(60_000_000);
            if outcome.latency_us > 60_000_000 {
                tracing::warn!(
                    "Latency {} µs exceeds histogram max; clamping to 60s",
                    outcome.latency_us
                );
            }
            if let Err(e) = self.histogram.record(val) {
                tracing::warn!("Failed to record histogram value {}: {}", val, e);
            }
        }
    }

    pub fn finalize(self) -> AggregatedStats {
        let elapsed = self.start_time.elapsed();
        let elapsed_secs = elapsed.as_secs_f64();
        let throughput_rps = if elapsed_secs > 0.0 {
            self.attempted as f64 / elapsed_secs
        } else {
            0.0
        };

        let p50 = self.histogram.value_at_quantile(0.50);
        let p95 = self.histogram.value_at_quantile(0.95);
        let p99 = self.histogram.value_at_quantile(0.99);

        AggregatedStats {
            total_lines: self.total_lines,
            parse_errors: self.parse_errors,
            filtered_count: self.filtered_count,
            attempted: self.attempted,
            succeeded: self.succeeded,
            failed: self.failed,
            status_distribution: self.status_distribution,
            error_categories: self.error_categories,
            elapsed_secs,
            throughput_rps,
            p50_us: p50,
            p95_us: p95,
            p99_us: p99,
        }
    }
}

fn categorize_error(err: &str) -> String {
    let lower = err.to_lowercase();
    if lower.contains("timeout") || lower.contains("timed out") {
        "timeout".to_string()
    } else if lower.contains("connection refused") {
        "connection-refused".to_string()
    } else if lower.contains("tls") || lower.contains("ssl") || lower.contains("certificate") {
        "tls".to_string()
    } else {
        "other".to_string()
    }
}

pub async fn run_stats_aggregator(
    mut rx: mpsc::Receiver<ReplayOutcome>,
    mut acc: StatsAccumulator,
) -> StatsAccumulator {
    while let Some(outcome) = rx.recv().await {
        acc.record_outcome(&outcome);
    }
    acc
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ReplayOutcome;

    fn make_outcome(status: Option<u16>, latency_us: u64, error: Option<&str>) -> ReplayOutcome {
        ReplayOutcome {
            status,
            latency_us,
            error: error.map(|s| s.to_string()),
            was_dry_run: false,
        }
    }

    fn make_dry_run_outcome() -> ReplayOutcome {
        ReplayOutcome {
            status: None,
            latency_us: 0,
            error: None,
            was_dry_run: true,
        }
    }

    // ── Basic counting ────────────────────────────────────────────────────────

    #[test]
    fn test_successful_request_increments_succeeded() {
        let mut acc = StatsAccumulator::new();
        acc.record_outcome(&make_outcome(Some(200), 1000, None));
        assert_eq!(acc.attempted, 1);
        assert_eq!(acc.succeeded, 1);
        assert_eq!(acc.failed, 0);
    }

    #[test]
    fn test_non_2xx_increments_failed() {
        let mut acc = StatsAccumulator::new();
        acc.record_outcome(&make_outcome(Some(404), 500, None));
        acc.record_outcome(&make_outcome(Some(500), 800, None));
        assert_eq!(acc.attempted, 2);
        assert_eq!(acc.succeeded, 0);
        assert_eq!(acc.failed, 2);
    }

    #[test]
    fn test_2xx_boundary_values() {
        let mut acc = StatsAccumulator::new();
        acc.record_outcome(&make_outcome(Some(200), 100, None));
        acc.record_outcome(&make_outcome(Some(201), 100, None));
        acc.record_outcome(&make_outcome(Some(299), 100, None));
        acc.record_outcome(&make_outcome(Some(300), 100, None)); // NOT 2xx
        assert_eq!(acc.succeeded, 3);
        assert_eq!(acc.failed, 1);
    }

    #[test]
    fn test_transport_error_no_status_increments_failed() {
        let mut acc = StatsAccumulator::new();
        acc.record_outcome(&make_outcome(None, 200, Some("connection refused")));
        assert_eq!(acc.attempted, 1);
        assert_eq!(acc.succeeded, 0);
        assert_eq!(acc.failed, 1);
    }

    // ── Dry run ───────────────────────────────────────────────────────────────

    #[test]
    fn test_dry_run_increments_attempted_and_succeeded() {
        let mut acc = StatsAccumulator::new();
        acc.record_outcome(&make_dry_run_outcome());
        assert_eq!(acc.attempted, 1);
        assert_eq!(acc.succeeded, 1);
        assert_eq!(acc.failed, 0);
        // Histogram should not record dry-run latency
        assert_eq!(acc.histogram.len(), 0);
    }

    // ── Status distribution ───────────────────────────────────────────────────

    #[test]
    fn test_status_distribution() {
        let mut acc = StatsAccumulator::new();
        acc.record_outcome(&make_outcome(Some(200), 100, None));
        acc.record_outcome(&make_outcome(Some(200), 200, None));
        acc.record_outcome(&make_outcome(Some(404), 50, None));
        assert_eq!(acc.status_distribution[&200], 2);
        assert_eq!(acc.status_distribution[&404], 1);
    }

    // ── Error categorization ──────────────────────────────────────────────────

    #[test]
    fn test_error_category_timeout() {
        let mut acc = StatsAccumulator::new();
        acc.record_outcome(&make_outcome(None, 30_000_000, Some("request timed out after 30s")));
        assert_eq!(acc.error_categories.get("timeout").copied(), Some(1));
    }

    #[test]
    fn test_error_category_connection_refused() {
        let mut acc = StatsAccumulator::new();
        acc.record_outcome(&make_outcome(None, 10, Some("tcp connect error: connection refused")));
        assert_eq!(acc.error_categories.get("connection-refused").copied(), Some(1));
    }

    #[test]
    fn test_error_category_tls() {
        let mut acc = StatsAccumulator::new();
        acc.record_outcome(&make_outcome(None, 100, Some("TLS handshake failed: invalid certificate")));
        assert_eq!(acc.error_categories.get("tls").copied(), Some(1));
    }

    #[test]
    fn test_error_category_other() {
        let mut acc = StatsAccumulator::new();
        acc.record_outcome(&make_outcome(None, 100, Some("some unexpected I/O error")));
        assert_eq!(acc.error_categories.get("other").copied(), Some(1));
    }

    // ── Histogram and latency ─────────────────────────────────────────────────

    #[test]
    fn test_latency_recorded_in_histogram() {
        let mut acc = StatsAccumulator::new();
        acc.record_outcome(&make_outcome(Some(200), 5000, None));
        acc.record_outcome(&make_outcome(Some(200), 10000, None));
        assert_eq!(acc.histogram.len(), 2);
    }

    #[test]
    fn test_zero_latency_not_recorded_in_histogram() {
        // Guard: `if outcome.latency_us > 0` → 0 is skipped
        let mut acc = StatsAccumulator::new();
        acc.record_outcome(&make_outcome(Some(200), 0, None));
        assert_eq!(acc.histogram.len(), 0);
    }

    #[test]
    fn test_latency_clamped_at_max() {
        let mut acc = StatsAccumulator::new();
        // 70 seconds > 60_000_000 µs max → clamped before recording.
        // hdrhistogram uses power-of-2 buckets with 3 significant figures, so
        // the stored value rounds to the nearest bucket (≤ 1% of 60_000_000).
        acc.record_outcome(&make_outcome(Some(200), 70_000_000, None));
        assert_eq!(acc.histogram.len(), 1);
        let recorded = acc.histogram.value_at_quantile(1.0);
        assert!(
            recorded >= 59_400_000 && recorded <= 60_600_000,
            "expected ~60_000_000 µs, got {} µs",
            recorded
        );
    }

    // ── finalize / percentiles ────────────────────────────────────────────────

    #[test]
    fn test_finalize_computes_throughput() {
        let mut acc = StatsAccumulator::new();
        for _ in 0..100 {
            acc.record_outcome(&make_outcome(Some(200), 1000, None));
        }
        let stats = acc.finalize();
        assert_eq!(stats.attempted, 100);
        assert_eq!(stats.succeeded, 100);
        assert!(stats.throughput_rps > 0.0);
    }

    #[test]
    fn test_finalize_percentiles_monotonic() {
        let mut acc = StatsAccumulator::new();
        for i in 1..=100u64 {
            acc.record_outcome(&make_outcome(Some(200), i * 1000, None));
        }
        let stats = acc.finalize();
        assert!(stats.p50_us <= stats.p95_us, "p50 must be <= p95");
        assert!(stats.p95_us <= stats.p99_us, "p95 must be <= p99");
        assert!(stats.p50_us > 0);
    }

    #[test]
    fn test_finalize_with_no_requests() {
        let acc = StatsAccumulator::new();
        let stats = acc.finalize();
        assert_eq!(stats.attempted, 0);
        assert_eq!(stats.succeeded, 0);
        assert_eq!(stats.failed, 0);
        assert_eq!(stats.p50_us, 0);
        assert_eq!(stats.p95_us, 0);
        assert_eq!(stats.p99_us, 0);
    }

    // ── Async aggregator ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_run_stats_aggregator() {
        let (tx, rx) = mpsc::channel(16);
        let acc = StatsAccumulator::new();

        tokio::spawn(async move {
            for i in 0..5u16 {
                let _ = tx
                    .send(make_outcome(Some(200 + i), (i as u64 + 1) * 1000, None))
                    .await;
            }
            // tx dropped here → rx sees EOF
        });

        let result = run_stats_aggregator(rx, acc).await;
        assert_eq!(result.attempted, 5);
        assert_eq!(result.succeeded, 5);
    }
}
