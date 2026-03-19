//! Amazon S3 input source.
//!
//! Streams an S3 object line-by-line via `aws-sdk-s3` `GetObject`.
//! The object is never written to disk or fully buffered in memory.
//!
//! # AWS credential resolution
//! Uses `aws_config::defaults(BehaviorVersion::latest())` which follows the
//! standard chain: env vars → `~/.aws` files → IMDS.
//! Region: `--aws-region` flag → `AWS_DEFAULT_REGION` → `aws-config` defaults.

use crate::error::TrafficReplayerError;
use crate::input::{LineStream, RecordSource};
use aws_config::BehaviorVersion;
use aws_sdk_s3::Client as S3Client;
use futures::StreamExt;
use tokio::io::AsyncBufReadExt;
use tokio_stream::wrappers::LinesStream;

/// An S3 object opened for streaming as NDJSON lines.
///
/// Async initialisation (the `GetObject` API call) happens in `open`.
/// `into_stream` is then synchronous: it converts the already-established
/// byte stream into a typed `LineStream`.
pub struct S3Source {
    /// Pre-built during `open` to avoid naming the opaque `impl AsyncRead`
    /// type returned by `ByteStream::into_async_read`.
    stream: LineStream,
}

impl S3Source {
    /// Issues a `GetObject` request and wraps the response body as a
    /// line-by-line stream.
    pub async fn open(
        bucket: &str,
        key: &str,
        region: Option<&str>,
    ) -> Result<Self, TrafficReplayerError> {
        let mut loader = aws_config::defaults(BehaviorVersion::latest());
        if let Some(r) = region {
            loader = loader.region(aws_sdk_s3::config::Region::new(r.to_string()));
        }
        let sdk_config = loader.load().await;
        let client = S3Client::new(&sdk_config);

        let resp = client
            .get_object()
            .bucket(bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| TrafficReplayerError::S3(e.to_string()))?;

        let async_read = resp.body.into_async_read();
        let buf_reader = tokio::io::BufReader::new(async_read);
        let stream: LineStream = Box::pin(
            LinesStream::new(buf_reader.lines())
                .map(|r: Result<String, std::io::Error>| r.map_err(TrafficReplayerError::Io)),
        );

        Ok(S3Source { stream })
    }
}

impl RecordSource for S3Source {
    fn into_stream(self: Box<Self>) -> LineStream {
        self.stream
    }
}
