//! Per-request dispatch helper.
//!
//! `dispatch` is a thin façade that delegates to the injected [`RequestSender`]
//! strategy.  Keeping it separate from the pipeline loop makes the hot path
//! easy to test in isolation.

use crate::models::{ReplayOutcome, SanitizedReplayRecord};
use crate::replay::sender::RequestSender;

/// Dispatches one sanitized record through the given sender strategy and
/// returns the outcome (status, latency, error).
///
/// This function is intentionally trivial: all policy lives in the
/// [`RequestSender`] implementation, not here.
pub async fn dispatch(
    sender: &dyn RequestSender,
    record: SanitizedReplayRecord,
) -> ReplayOutcome {
    sender.send(record).await
}
