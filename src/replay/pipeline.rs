//! Core replay pipeline.
//!
//! Orchestrates line reading, parsing, filtering, sanitization, rate-limiting,
//! bounded concurrency, and statistics aggregation into a single async loop.
//!
//! All strategy objects (sender, sanitizer) are injected via factories so the
//! pipeline has no direct knowledge of HTTP, dry-run, or sanitization policy.

use crate::config::Config;
use crate::filters::FilterChain;
use crate::input::LineStream;
use crate::models::{ReplayOutcome, ReplayRecord};
use crate::replay::sender::SenderFactory;
use crate::replay::worker::dispatch;
use crate::sanitize::SanitizerFactory;
use crate::stats::StatsAccumulator;
use futures::StreamExt;
use governor::{Quota, RateLimiter};
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Semaphore};
use tokio_util::sync::CancellationToken;

pub async fn run_pipeline(
    config: Arc<Config>,
    mut stream: LineStream,
    filter_chain: FilterChain,
    cancel_token: CancellationToken,
    interrupted: &mut bool,
) -> Result<StatsAccumulator, crate::error::TrafficReplayerError> {
    // ── Strategy objects built via factories ──────────────────────────────────
    let sender: Arc<dyn crate::replay::sender::RequestSender> =
        Arc::from(SenderFactory::from_config(&config)?);
    let sanitizer = Arc::new(SanitizerFactory::from_config(&config));

    // ── Concurrency bound ─────────────────────────────────────────────────────
    let semaphore = Arc::new(Semaphore::new(config.concurrency as usize));

    // ── Stats aggregator on a bounded mpsc channel (no lock on hot path) ─────
    let (stats_tx, stats_rx) = mpsc::channel::<ReplayOutcome>(config.concurrency as usize * 4);
    let (agg_result_tx, mut agg_result_rx) = mpsc::channel::<StatsAccumulator>(1);

    tokio::spawn(async move {
        let result =
            crate::stats::run_stats_aggregator(stats_rx, StatsAccumulator::new()).await;
        let _ = agg_result_tx.send(result).await;
    });

    // ── Rate limiter (optional) ───────────────────────────────────────────────
    let rate_limiter = config.rate.map(|r| {
        let quota = Quota::per_second(NonZeroU32::new(r).expect("rate > 0"));
        Arc::new(RateLimiter::direct(quota))
    });

    // ── Local counters (parse / filter; kept off the mpsc channel) ───────────
    let mut acc = StatsAccumulator::new();
    let mut records_dispatched: u64 = 0;

    // ── Main dispatch loop ────────────────────────────────────────────────────
    loop {
        // Respect max-records limit
        if let Some(max) = config.max_records {
            if records_dispatched >= max {
                break;
            }
        }

        // Race line-read against SIGINT cancellation
        let line = tokio::select! {
            biased;
            _ = cancel_token.cancelled() => {
                *interrupted = true;
                break;
            }
            line = stream.next() => line,
        };

        let line = match line {
            None => break, // EOF
            Some(Err(e)) => {
                tracing::warn!("IO error reading line: {}", e);
                acc.total_lines += 1;
                acc.parse_errors += 1;
                continue;
            }
            Some(Ok(l)) => l,
        };

        acc.total_lines += 1;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Parse
        let record: ReplayRecord = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("Parse error: {}", e);
                acc.parse_errors += 1;
                continue;
            }
        };

        // Filter
        if !filter_chain.matches(&record) {
            acc.filtered_count += 1;
            continue;
        }

        records_dispatched += 1;

        // Sanitize (inside spawned task to keep the dispatch loop tight)
        let san_record = match sanitizer.sanitize(record, &config.base_url) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("Sanitization error: {}", e);
                acc.parse_errors += 1;
                continue;
            }
        };

        // Rate limit
        if let Some(ref limiter) = rate_limiter {
            limiter.until_ready().await;
        }

        // Acquire semaphore permit (blocks when at concurrency limit)
        let permit = semaphore.clone().acquire_owned().await.unwrap();

        let sender_clone = Arc::clone(&sender);
        let tx = stats_tx.clone();

        tokio::spawn(async move {
            let outcome = dispatch(sender_clone.as_ref(), san_record).await;
            let _ = tx.send(outcome).await;
            drop(permit); // releases the semaphore slot
        });
    }

    // ── Drain phase ───────────────────────────────────────────────────────────
    drop(stats_tx); // signal the aggregator that no more outcomes are coming

    let drain_timeout = Duration::from_millis(config.drain_timeout_ms);
    let drain = tokio::time::timeout(drain_timeout, async {
        // Acquiring all N permits means every spawned task has finished
        let _all = semaphore.acquire_many(config.concurrency).await;
    });

    if drain.await.is_err() {
        tracing::warn!(
            "Drain timeout exceeded; some in-flight requests may not have completed"
        );
    }

    // ── Merge counters and return ─────────────────────────────────────────────
    let mut agg = agg_result_rx
        .recv()
        .await
        .unwrap_or_else(StatsAccumulator::new);

    agg.total_lines = acc.total_lines;
    agg.parse_errors = acc.parse_errors;
    agg.filtered_count = acc.filtered_count;

    Ok(agg)
}
