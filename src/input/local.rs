//! Local filesystem input source.
//!
//! Streams an NDJSON file line-by-line using `tokio::io::AsyncBufReadExt`.
//! The file is never fully buffered into memory.

use crate::error::TrafficReplayerError;
use crate::input::{LineStream, RecordSource};
use futures::StreamExt;
use std::path::Path;
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_stream::wrappers::LinesStream;

/// A local NDJSON file opened for streaming.
pub struct LocalSource {
    reader: BufReader<File>,
}

impl LocalSource {
    /// Opens `path` for async line-by-line reading.
    pub async fn open(path: &Path) -> Result<Self, TrafficReplayerError> {
        let file = File::open(path).await?;
        Ok(LocalSource {
            reader: BufReader::new(file),
        })
    }
}

impl RecordSource for LocalSource {
    fn into_stream(self: Box<Self>) -> LineStream {
        let lines = self.reader.lines();
        Box::pin(
            LinesStream::new(lines)
                .map(|r: Result<String, std::io::Error>| r.map_err(TrafficReplayerError::Io)),
        )
    }
}

// ── Convenience wrapper (keeps integration tests and main.rs concise) ─────────

/// Opens a local NDJSON file and immediately returns a [`LineStream`].
///
/// This is a thin convenience wrapper around `LocalSource::open` +
/// `RecordSource::into_stream`.  Prefer using [`LocalSource`] directly
/// when you need access to the typed source object.
pub async fn open_local_stream(path: &Path) -> Result<LineStream, TrafficReplayerError> {
    Ok(Box::new(LocalSource::open(path).await?).into_stream())
}
