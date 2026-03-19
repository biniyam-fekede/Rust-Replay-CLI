use crate::cli::Cli;
use crate::error::TrafficReplayerError;
use std::collections::HashSet;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub enum InputSource {
    LocalFile(PathBuf),
    S3 { bucket: String, key: String },
}

impl InputSource {
    /// Returns `Some(&PathBuf)` when the source is a local file, otherwise `None`.
    pub fn as_local_path(&self) -> Option<&PathBuf> {
        match self {
            InputSource::LocalFile(p) => Some(p),
            InputSource::S3 { .. } => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub base_url: String,
    pub input_source: InputSource,
    pub concurrency: u32,
    pub timeout_ms: u64,
    pub connect_timeout_ms: u64,
    pub methods: Vec<String>,
    pub path_prefixes: Vec<String>,
    pub statuses: Vec<u16>,
    pub rate: Option<u32>,
    pub dry_run: bool,
    pub verbose: bool,
    pub output_stats: Option<PathBuf>,
    pub max_records: Option<u64>,
    pub insecure_skip_tls_verify: bool,
    pub follow_redirects: bool,
    pub aws_region: Option<String>,
    pub passthrough_keys: HashSet<String>,
    pub drain_timeout_ms: u64,
}

impl Config {
    pub fn from_cli(cli: Cli) -> Result<Self, TrafficReplayerError> {
        // Validate input source
        let input_source = match (cli.input_file, cli.s3_bucket, cli.s3_key) {
            (Some(path), None, None) => InputSource::LocalFile(path),
            (None, Some(bucket), Some(key)) => InputSource::S3 { bucket, key },
            (None, None, None) => {
                return Err(TrafficReplayerError::ConfigValidation(
                    "Must provide either --input-file or both --s3-bucket and --s3-key".to_string(),
                ))
            }
            _ => {
                return Err(TrafficReplayerError::ConfigValidation(
                    "--input-file and --s3-bucket/--s3-key are mutually exclusive".to_string(),
                ))
            }
        };

        // Validate base URL
        if cli.base_url.is_empty() {
            return Err(TrafficReplayerError::ConfigValidation(
                "--base-url cannot be empty".to_string(),
            ));
        }

        // Parse passthrough keys into lowercased HashSet
        let passthrough_keys: HashSet<String> = cli
            .sanitize_passthrough_keys
            .unwrap_or_default()
            .split(',')
            .filter(|s| !s.is_empty())
            .map(|s| s.trim().to_lowercase())
            .collect();

        Ok(Config {
            base_url: cli.base_url.trim_end_matches('/').to_string(),
            input_source,
            concurrency: cli.concurrency,
            timeout_ms: cli.timeout_ms,
            connect_timeout_ms: cli.connect_timeout_ms,
            methods: cli.methods.into_iter().map(|m| m.to_uppercase()).collect(),
            path_prefixes: cli.path_prefixes,
            statuses: cli.statuses,
            rate: cli.rate,
            dry_run: cli.dry_run,
            verbose: cli.verbose,
            output_stats: cli.output_stats,
            max_records: cli.max_records,
            insecure_skip_tls_verify: cli.insecure_skip_tls_verify,
            follow_redirects: cli.follow_redirects,
            aws_region: cli.aws_region,
            passthrough_keys,
            drain_timeout_ms: cli.drain_timeout_ms,
        })
    }
}
