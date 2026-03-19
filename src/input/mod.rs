//! Input source abstraction for NDJSON line streaming.
//!
//! # Design patterns
//!   * **Factory**               — `SourceFactory` selects the correct backend
//!     from `Config` without exposing construction to callers.
//!   * **Open/Closed Principle** — add new backends (HTTP, Kafka, stdin …) by
//!     implementing `RecordSource`; existing sources and the factory are untouched.

use crate::config::Config;
use crate::error::TrafficReplayerError;
use futures::Stream;
use std::pin::Pin;

pub mod local;
pub mod s3;

/// A lazily-evaluated, owned stream of raw NDJSON lines.
pub type LineStream =
    Pin<Box<dyn Stream<Item = Result<String, TrafficReplayerError>> + Send + 'static>>;

// ── RecordSource trait ────────────────────────────────────────────────────────

/// Any source that can produce a stream of raw NDJSON lines.
///
/// Implement this trait to add new input backends without modifying existing
/// source implementations or the factory — Open/Closed Principle.
///
/// Async initialization (opening files, establishing connections) belongs in
/// an associated `open` / `new` constructor on the concrete type.
/// `into_stream` is synchronous: it converts an already-opened resource into
/// a `LineStream`.
pub trait RecordSource: Send {
    /// Consumes the source and returns a streaming iterator of raw lines.
    fn into_stream(self: Box<Self>) -> LineStream;
}

// ── SourceFactory (Factory pattern) ───────────────────────────────────────────

/// Creates the appropriate [`RecordSource`] implementation from [`Config`].
///
/// **Extension point**: to support a new source type, add a new enum variant
/// to `InputSource`, implement `RecordSource` for the corresponding struct,
/// and add one arm here.  All existing source implementations remain unchanged.
pub struct SourceFactory;

impl SourceFactory {
    pub async fn create(
        config: &Config,
    ) -> Result<Box<dyn RecordSource>, TrafficReplayerError> {
        use crate::config::InputSource;

        match &config.input_source {
            InputSource::LocalFile(path) => {
                let source = local::LocalSource::open(path).await?;
                Ok(Box::new(source))
            }
            InputSource::S3 { bucket, key } => {
                let source =
                    s3::S3Source::open(bucket, key, config.aws_region.as_deref()).await?;
                Ok(Box::new(source))
            }
        }
    }
}
