use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug, Clone)]
#[command(
    name = "traffic-replayer",
    about = "Replay HTTP traffic from NDJSON logs to a target service",
    long_about = None,
)]
pub struct Cli {
    /// Target base URL (required)
    #[arg(long, required = true)]
    pub base_url: String,

    /// Local NDJSON input file (mutually exclusive with --s3-bucket/--s3-key)
    #[arg(long, conflicts_with_all = ["s3_bucket", "s3_key"])]
    pub input_file: Option<PathBuf>,

    /// S3 bucket name (requires --s3-key)
    #[arg(long, requires = "s3_key", conflicts_with = "input_file")]
    pub s3_bucket: Option<String>,

    /// S3 object key (requires --s3-bucket)
    #[arg(long, requires = "s3_bucket", conflicts_with = "input_file")]
    pub s3_key: Option<String>,

    /// Max in-flight requests
    #[arg(long, default_value = "50")]
    pub concurrency: u32,

    /// Total request timeout in milliseconds
    #[arg(long, default_value = "30000")]
    pub timeout_ms: u64,

    /// TCP connect timeout in milliseconds
    #[arg(long, default_value = "5000")]
    pub connect_timeout_ms: u64,

    /// Filter by HTTP method (repeatable, OR logic)
    #[arg(long = "method", action = clap::ArgAction::Append)]
    pub methods: Vec<String>,

    /// Filter by path prefix (repeatable, OR logic)
    #[arg(long = "path-prefix", action = clap::ArgAction::Append)]
    pub path_prefixes: Vec<String>,

    /// Filter by original response status code (repeatable, OR logic)
    #[arg(long = "status", action = clap::ArgAction::Append)]
    pub statuses: Vec<u16>,

    /// Max requests per second (uses governor rate limiter)
    #[arg(long)]
    pub rate: Option<u32>,

    /// Parse, filter, sanitize but do NOT send requests
    #[arg(long, default_value = "false")]
    pub dry_run: bool,

    /// Enable debug-level tracing
    #[arg(long, default_value = "false")]
    pub verbose: bool,

    /// Write JSON stats to this file path (never stdout)
    #[arg(long)]
    pub output_stats: Option<PathBuf>,

    /// Stop after processing N records
    #[arg(long)]
    pub max_records: Option<u64>,

    /// Disable TLS certificate verification
    #[arg(long, default_value = "false")]
    pub insecure_skip_tls_verify: bool,

    /// Follow HTTP redirects (default: off — redirects are NOT followed)
    #[arg(long, default_value = "false")]
    pub follow_redirects: bool,

    /// AWS region (falls back to AWS_DEFAULT_REGION env var)
    #[arg(long)]
    pub aws_region: Option<String>,

    /// Comma-separated keys exempt from sanitization (headers, query, body)
    #[arg(long)]
    pub sanitize_passthrough_keys: Option<String>,

    /// Max wait for in-flight requests on SIGINT (ms)
    #[arg(long, default_value = "5000")]
    pub drain_timeout_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        // Prepend a fake program name
        let mut full: Vec<&str> = vec!["traffic-replayer"];
        full.extend_from_slice(args);
        Cli::try_parse_from(full)
    }

    // ── Required args ─────────────────────────────────────────────────────────

    #[test]
    fn test_requires_base_url() {
        let result = parse(&["--input-file", "file.ndjson"]);
        assert!(result.is_err(), "Should fail when --base-url is missing");
    }

    #[test]
    fn test_requires_input_source() {
        // base-url alone with no input should succeed parsing (config validation catches it)
        let result = parse(&["--base-url", "http://localhost:8080"]);
        assert!(result.is_ok(), "Parser should not fail on missing input; Config validates it");
    }

    #[test]
    fn test_local_file_flag() {
        let cli = parse(&["--base-url", "http://localhost:8080", "--input-file", "/tmp/test.ndjson"]).unwrap();
        assert_eq!(cli.input_file.unwrap().to_str().unwrap(), "/tmp/test.ndjson");
        assert!(cli.s3_bucket.is_none());
        assert!(cli.s3_key.is_none());
    }

    #[test]
    fn test_s3_flags() {
        let cli = parse(&[
            "--base-url", "http://localhost:8080",
            "--s3-bucket", "my-bucket",
            "--s3-key", "logs/replay.ndjson",
        ]).unwrap();
        assert_eq!(cli.s3_bucket.as_deref(), Some("my-bucket"));
        assert_eq!(cli.s3_key.as_deref(), Some("logs/replay.ndjson"));
        assert!(cli.input_file.is_none());
    }

    // ── Mutual exclusion ──────────────────────────────────────────────────────

    #[test]
    fn test_input_file_and_s3_bucket_are_mutually_exclusive() {
        let result = parse(&[
            "--base-url", "http://localhost:8080",
            "--input-file", "file.ndjson",
            "--s3-bucket", "bucket",
            "--s3-key", "key",
        ]);
        assert!(result.is_err(), "input-file and s3 flags must be mutually exclusive");
    }

    #[test]
    fn test_s3_bucket_requires_s3_key() {
        let result = parse(&[
            "--base-url", "http://localhost:8080",
            "--s3-bucket", "bucket",
        ]);
        assert!(result.is_err(), "--s3-bucket requires --s3-key");
    }

    #[test]
    fn test_s3_key_requires_s3_bucket() {
        let result = parse(&[
            "--base-url", "http://localhost:8080",
            "--s3-key", "key",
        ]);
        assert!(result.is_err(), "--s3-key requires --s3-bucket");
    }

    // ── Defaults ──────────────────────────────────────────────────────────────

    #[test]
    fn test_defaults() {
        let cli = parse(&["--base-url", "http://localhost:8080", "--input-file", "f.ndjson"]).unwrap();
        assert_eq!(cli.concurrency, 50);
        assert_eq!(cli.timeout_ms, 30000);
        assert_eq!(cli.connect_timeout_ms, 5000);
        assert_eq!(cli.drain_timeout_ms, 5000);
        assert!(!cli.dry_run);
        assert!(!cli.verbose);
        assert!(!cli.insecure_skip_tls_verify);
        assert!(!cli.follow_redirects);
        assert!(cli.rate.is_none());
        assert!(cli.output_stats.is_none());
        assert!(cli.max_records.is_none());
        assert!(cli.aws_region.is_none());
        assert!(cli.sanitize_passthrough_keys.is_none());
    }

    // ── Repeatable flags ──────────────────────────────────────────────────────

    #[test]
    fn test_method_repeatable() {
        let cli = parse(&[
            "--base-url", "http://localhost:8080",
            "--input-file", "f.ndjson",
            "--method", "GET",
            "--method", "POST",
        ]).unwrap();
        assert_eq!(cli.methods, vec!["GET", "POST"]);
    }

    #[test]
    fn test_path_prefix_repeatable() {
        let cli = parse(&[
            "--base-url", "http://localhost:8080",
            "--input-file", "f.ndjson",
            "--path-prefix", "/api/v1",
            "--path-prefix", "/health",
        ]).unwrap();
        assert_eq!(cli.path_prefixes, vec!["/api/v1", "/health"]);
    }

    #[test]
    fn test_status_repeatable() {
        let cli = parse(&[
            "--base-url", "http://localhost:8080",
            "--input-file", "f.ndjson",
            "--status", "200",
            "--status", "201",
        ]).unwrap();
        assert_eq!(cli.statuses, vec![200u16, 201u16]);
    }

    // ── Boolean flags ─────────────────────────────────────────────────────────

    #[test]
    fn test_dry_run_flag() {
        let cli = parse(&[
            "--base-url", "http://localhost:8080",
            "--input-file", "f.ndjson",
            "--dry-run",
        ]).unwrap();
        assert!(cli.dry_run);
    }

    #[test]
    fn test_verbose_flag() {
        let cli = parse(&[
            "--base-url", "http://localhost:8080",
            "--input-file", "f.ndjson",
            "--verbose",
        ]).unwrap();
        assert!(cli.verbose);
    }

    #[test]
    fn test_follow_redirects_flag() {
        let cli = parse(&[
            "--base-url", "http://localhost:8080",
            "--input-file", "f.ndjson",
            "--follow-redirects",
        ]).unwrap();
        assert!(cli.follow_redirects);
    }

    #[test]
    fn test_insecure_skip_tls_verify_flag() {
        let cli = parse(&[
            "--base-url", "http://localhost:8080",
            "--input-file", "f.ndjson",
            "--insecure-skip-tls-verify",
        ]).unwrap();
        assert!(cli.insecure_skip_tls_verify);
    }

    // ── Numeric and optional flags ────────────────────────────────────────────

    #[test]
    fn test_concurrency_flag() {
        let cli = parse(&[
            "--base-url", "http://localhost:8080",
            "--input-file", "f.ndjson",
            "--concurrency", "200",
        ]).unwrap();
        assert_eq!(cli.concurrency, 200);
    }

    #[test]
    fn test_rate_flag() {
        let cli = parse(&[
            "--base-url", "http://localhost:8080",
            "--input-file", "f.ndjson",
            "--rate", "1000",
        ]).unwrap();
        assert_eq!(cli.rate, Some(1000));
    }

    #[test]
    fn test_max_records_flag() {
        let cli = parse(&[
            "--base-url", "http://localhost:8080",
            "--input-file", "f.ndjson",
            "--max-records", "50000",
        ]).unwrap();
        assert_eq!(cli.max_records, Some(50000));
    }

    #[test]
    fn test_sanitize_passthrough_keys_flag() {
        let cli = parse(&[
            "--base-url", "http://localhost:8080",
            "--input-file", "f.ndjson",
            "--sanitize-passthrough-keys", "key,session_id",
        ]).unwrap();
        assert_eq!(cli.sanitize_passthrough_keys.as_deref(), Some("key,session_id"));
    }

    #[test]
    fn test_aws_region_flag() {
        let cli = parse(&[
            "--base-url", "http://localhost:8080",
            "--s3-bucket", "bucket",
            "--s3-key", "key",
            "--aws-region", "us-east-1",
        ]).unwrap();
        assert_eq!(cli.aws_region.as_deref(), Some("us-east-1"));
    }

    #[test]
    fn test_output_stats_flag() {
        let cli = parse(&[
            "--base-url", "http://localhost:8080",
            "--input-file", "f.ndjson",
            "--output-stats", "/tmp/stats.json",
        ]).unwrap();
        assert_eq!(cli.output_stats.unwrap().to_str().unwrap(), "/tmp/stats.json");
    }
}
