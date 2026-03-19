//! HTTP request dispatch strategies.
//!
//! # Design patterns
//!   * **Strategy**              — `RequestSender` is the strategy interface.
//!     `HttpSender` (real network I/O) and `DryRunSender` (no-op) are
//!     interchangeable implementations selected at startup.
//!   * **Factory**               — `SenderFactory::from_config` encapsulates
//!     the selection logic; callers receive `Box<dyn RequestSender>`.
//!   * **Open/Closed Principle** — add new dispatch strategies (gRPC, WebSocket,
//!     throttled sender …) by implementing `RequestSender`; the pipeline and
//!     existing strategies are never modified.

use crate::config::Config;
use crate::error::TrafficReplayerError;
use crate::models::{ReplayOutcome, SanitizedReplayRecord};
use crate::replay::client::build_client;
use futures::future::BoxFuture;
use reqwest::Client;
use std::time::Instant;

// ── RequestSender trait (Strategy) ────────────────────────────────────────────

/// Strategy for dispatching (or simulating) a single replay request.
///
/// The returned future is `'static` so that it can be handed to
/// `tokio::spawn` without borrowing `self`.  Implementations that need
/// shared state (e.g. `reqwest::Client`) clone an `Arc` or `Client`
/// inside `send` before moving into the future.
pub trait RequestSender: Send + Sync + 'static {
    fn send(&self, record: SanitizedReplayRecord) -> BoxFuture<'static, ReplayOutcome>;
}

// ── HttpSender ────────────────────────────────────────────────────────────────

/// Sends real HTTP requests via a shared `reqwest::Client`.
///
/// `reqwest::Client` is an `Arc` wrapper internally, so cloning it is cheap.
pub struct HttpSender {
    client: Client,
}

impl HttpSender {
    pub fn new(client: Client) -> Self {
        HttpSender { client }
    }
}

impl RequestSender for HttpSender {
    fn send(&self, record: SanitizedReplayRecord) -> BoxFuture<'static, ReplayOutcome> {
        // Clone is O(1) — Client wraps Arc<_>
        let client = self.client.clone();

        Box::pin(async move {
            let method = match reqwest::Method::from_bytes(record.method.as_bytes()) {
                Ok(m) => m,
                Err(e) => {
                    return ReplayOutcome {
                        status: None,
                        latency_us: 0,
                        error: Some(format!("Invalid method {}: {}", record.method, e)),
                        was_dry_run: false,
                    };
                }
            };

            let mut builder = client.request(method, &record.url).headers(record.headers);
            if let Some(body) = record.body {
                builder = builder.body(body);
            }

            let start = Instant::now();
            match builder.send().await {
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    let latency_us = start.elapsed().as_micros() as u64;
                    tracing::debug!(
                        method = %record.method,
                        url    = %record.url,
                        status,
                        latency_us,
                        "request complete"
                    );
                    ReplayOutcome {
                        status: Some(status),
                        latency_us,
                        error: None,
                        was_dry_run: false,
                    }
                }
                Err(e) => {
                    let latency_us = start.elapsed().as_micros() as u64;
                    tracing::debug!(
                        method = %record.method,
                        url    = %record.url,
                        error  = %e,
                        "request failed"
                    );
                    ReplayOutcome {
                        status: None,
                        latency_us,
                        error: Some(e.to_string()),
                        was_dry_run: false,
                    }
                }
            }
        })
    }
}

// ── DryRunSender ──────────────────────────────────────────────────────────────

/// No-op sender used with `--dry-run`.
///
/// Parses, filters, and sanitizes records but never opens a network socket.
/// Records a latency of 0 µs and increments `succeeded` in stats.
pub struct DryRunSender;

impl RequestSender for DryRunSender {
    fn send(&self, record: SanitizedReplayRecord) -> BoxFuture<'static, ReplayOutcome> {
        tracing::debug!(
            method = %record.method,
            url    = %record.url,
            "dry-run: skipping network send"
        );
        Box::pin(async move {
            ReplayOutcome {
                status: None,
                latency_us: 0,
                error: None,
                was_dry_run: true,
            }
        })
    }
}

// ── SenderFactory ─────────────────────────────────────────────────────────────

/// Selects the appropriate [`RequestSender`] strategy from [`Config`].
///
/// Returns `DryRunSender` when `--dry-run` is set, otherwise builds a
/// `reqwest::Client` and wraps it in `HttpSender`.
///
/// **Extension point**: add new strategy variants here — no pipeline code
/// needs to change.
pub struct SenderFactory;

impl SenderFactory {
    pub fn from_config(
        config: &Config,
    ) -> Result<Box<dyn RequestSender>, TrafficReplayerError> {
        if config.dry_run {
            Ok(Box::new(DryRunSender))
        } else {
            let client = build_client(config)?;
            Ok(Box::new(HttpSender::new(client)))
        }
    }
}
