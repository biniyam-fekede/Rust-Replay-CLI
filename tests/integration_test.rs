//! Integration tests for traffic-replayer.
//!
//! Each test spins up a local mock HTTP server (mockito), writes a temporary
//! NDJSON file, runs the full pipeline end-to-end, and asserts on stats and
//! server interactions.

use std::collections::HashSet;
use std::io::Write as IoWrite;
use std::path::PathBuf;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use traffic_replayer::{
    config::{Config, InputSource},
    filters::{FilterChain, FilterFactory, MethodFilter, PathPrefixFilter, StatusFilter},
    input::local::open_local_stream,
    replay::pipeline::run_pipeline,
    summary::write_stats_json,
};

// ─── helpers ─────────────────────────────────────────────────────────────────

fn write_ndjson(lines: &[&str]) -> PathBuf {
    let mut tmp = tempfile::NamedTempFile::new().expect("tempfile");
    for line in lines {
        writeln!(tmp, "{}", line).unwrap();
    }
    let path = tmp.path().to_path_buf();
    tmp.keep().unwrap();
    path
}

fn make_config(base_url: &str, input_file: PathBuf) -> Config {
    Config {
        base_url: base_url.trim_end_matches('/').to_string(),
        input_source: InputSource::LocalFile(input_file),
        concurrency: 4,
        timeout_ms: 5000,
        connect_timeout_ms: 2000,
        methods: vec![],
        path_prefixes: vec![],
        statuses: vec![],
        rate: None,
        dry_run: false,
        verbose: false,
        output_stats: None,
        max_records: None,
        insecure_skip_tls_verify: false,
        follow_redirects: false,
        aws_region: None,
        passthrough_keys: HashSet::new(),
        drain_timeout_ms: 2000,
    }
}

fn no_filters() -> FilterChain {
    FilterChain::new(vec![])
}

// ─── sample NDJSON lines ─────────────────────────────────────────────────────

const GET_USERS: &str = r#"{"method":"GET","path":"/api/users","status":200}"#;
const POST_USERS: &str = r#"{"method":"POST","path":"/api/users","body":"{\"name\":\"alice\"}","body_encoding":"utf8","content_type":"application/json","status":201}"#;
const GET_ADMIN: &str = r#"{"method":"GET","path":"/admin/secret","status":200}"#;
const BAD_JSON: &str = r#"not-valid-json at all"#;

// ─── 1. Local NDJSON end-to-end ───────────────────────────────────────────────

#[tokio::test]
async fn test_local_ndjson_end_to_end() {
    let mut server = mockito::Server::new_async().await;
    let _m1 = server.mock("GET", "/api/users").with_status(200).create_async().await;
    let _m2 = server.mock("POST", "/api/users").with_status(201).create_async().await;

    let path = write_ndjson(&[GET_USERS, POST_USERS]);
    let config = Arc::new(make_config(&server.url(), path.clone()));
    let stream = open_local_stream(&path).await.unwrap();

    let mut interrupted = false;
    let acc = run_pipeline(config, stream, no_filters(), CancellationToken::new(), &mut interrupted)
        .await
        .unwrap();

    let stats = acc.finalize();
    assert_eq!(stats.total_lines, 2);
    assert_eq!(stats.parse_errors, 0);
    assert_eq!(stats.filtered_count, 0);
    assert_eq!(stats.attempted, 2);
    assert_eq!(stats.succeeded, 2);
    assert_eq!(stats.failed, 0);
    assert!(!interrupted);
}

// ─── 2. Parse errors counted; valid records still replay ─────────────────────

#[tokio::test]
async fn test_parse_errors_counted_replay_continues() {
    let mut server = mockito::Server::new_async().await;
    let _m = server.mock("GET", "/api/users").with_status(200).create_async().await;

    let path = write_ndjson(&[BAD_JSON, GET_USERS, BAD_JSON]);
    let config = Arc::new(make_config(&server.url(), path.clone()));
    let stream = open_local_stream(&path).await.unwrap();

    let mut interrupted = false;
    let acc = run_pipeline(config, stream, no_filters(), CancellationToken::new(), &mut interrupted)
        .await
        .unwrap();

    let stats = acc.finalize();
    assert_eq!(stats.total_lines, 3);
    assert_eq!(stats.parse_errors, 2);
    assert_eq!(stats.attempted, 1);
    assert_eq!(stats.succeeded, 1);
}

// ─── 3. Method filter — non-matching never reaches the server ─────────────────

#[tokio::test]
async fn test_method_filter_blocks_non_matching() {
    let mut server = mockito::Server::new_async().await;
    let _m = server.mock("GET", "/api/users").with_status(200).expect(1).create_async().await;

    let path = write_ndjson(&[GET_USERS, POST_USERS]);
    let config = Arc::new(make_config(&server.url(), path.clone()));
    let stream = open_local_stream(&path).await.unwrap();

    // Only GET passes
    let chain = FilterChain::new(vec![Box::new(MethodFilter::new(vec!["GET".into()]))]);
    let mut interrupted = false;
    let acc = run_pipeline(config, stream, chain, CancellationToken::new(), &mut interrupted)
        .await
        .unwrap();

    let stats = acc.finalize();
    assert_eq!(stats.filtered_count, 1);
    assert_eq!(stats.attempted, 1);
}

// ─── 4. Path-prefix filter OR semantics ──────────────────────────────────────

#[tokio::test]
async fn test_path_prefix_filter_or_semantics() {
    let mut server = mockito::Server::new_async().await;
    let _m = server.mock("GET", "/api/users").with_status(200).create_async().await;

    let path = write_ndjson(&[GET_USERS, GET_ADMIN]);
    let config = Arc::new(make_config(&server.url(), path.clone()));
    let stream = open_local_stream(&path).await.unwrap();

    let chain = FilterChain::new(vec![Box::new(PathPrefixFilter::new(vec!["/api".into()]))]);
    let mut interrupted = false;
    let acc = run_pipeline(config, stream, chain, CancellationToken::new(), &mut interrupted)
        .await
        .unwrap();

    let stats = acc.finalize();
    assert_eq!(stats.filtered_count, 1);
    assert_eq!(stats.attempted, 1);
}

// ─── 5. Dry-run sends zero requests ──────────────────────────────────────────

#[tokio::test]
async fn test_dry_run_sends_no_requests() {
    let mut server = mockito::Server::new_async().await;
    let _m = server
        .mock("GET", mockito::Matcher::Any)
        .with_status(200)
        .expect(0)
        .create_async()
        .await;

    let path = write_ndjson(&[GET_USERS, POST_USERS]);
    let mut config = make_config(&server.url(), path.clone());
    config.dry_run = true;
    let config = Arc::new(config);
    let stream = open_local_stream(&path).await.unwrap();

    let mut interrupted = false;
    let acc = run_pipeline(config, stream, no_filters(), CancellationToken::new(), &mut interrupted)
        .await
        .unwrap();

    let stats = acc.finalize();
    assert_eq!(stats.attempted, 2);
    assert_eq!(stats.succeeded, 2);
    assert_eq!(stats.failed, 0);
}

// ─── 6. Stats correctness — 2xx vs non-2xx ───────────────────────────────────

#[tokio::test]
async fn test_stats_2xx_vs_non_2xx() {
    let mut server = mockito::Server::new_async().await;
    let _m200 = server.mock("GET", "/api/users").with_status(200).create_async().await;
    let _m404 = server.mock("GET", "/not-found").with_status(404).create_async().await;

    let not_found = r#"{"method":"GET","path":"/not-found","status":200}"#;
    let path = write_ndjson(&[GET_USERS, GET_USERS, not_found]);
    let config = Arc::new(make_config(&server.url(), path.clone()));
    let stream = open_local_stream(&path).await.unwrap();

    let mut interrupted = false;
    let acc = run_pipeline(config, stream, no_filters(), CancellationToken::new(), &mut interrupted)
        .await
        .unwrap();

    let stats = acc.finalize();
    assert_eq!(stats.attempted, 3);
    assert_eq!(stats.succeeded, 2);
    assert_eq!(stats.failed, 1);
    assert_eq!(stats.status_distribution[&200], 2);
    assert_eq!(stats.status_distribution[&404], 1);
}

// ─── 7. --output-stats JSON is written and parseable ─────────────────────────

#[tokio::test]
async fn test_output_stats_json_written_and_valid() {
    let mut server = mockito::Server::new_async().await;
    let _m = server.mock("GET", "/api/users").with_status(200).create_async().await;

    let stats_tmp = tempfile::NamedTempFile::new().unwrap();
    let stats_path = stats_tmp.path().to_path_buf();

    let path = write_ndjson(&[GET_USERS]);
    let config = Arc::new(make_config(&server.url(), path.clone()));
    let stream = open_local_stream(&path).await.unwrap();

    let mut interrupted = false;
    let acc = run_pipeline(config, stream, no_filters(), CancellationToken::new(), &mut interrupted)
        .await
        .unwrap();

    let stats = acc.finalize();
    write_stats_json(&stats, &stats_path).expect("write stats JSON");

    let raw = std::fs::read_to_string(&stats_path).unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(v["attempted"].as_u64(), Some(1));
    assert_eq!(v["succeeded"].as_u64(), Some(1));
    assert_eq!(v["failed"].as_u64(), Some(0));
    assert!(v.get("p50_us").is_some());
    assert!(v.get("p95_us").is_some());
    assert!(v.get("p99_us").is_some());
    assert!(v["elapsed_secs"].as_f64().unwrap() >= 0.0);
}

// ─── 8. max-records stops replay early ───────────────────────────────────────

#[tokio::test]
async fn test_max_records_limits_replay() {
    let mut server = mockito::Server::new_async().await;
    let _m = server.mock("GET", "/api/users").with_status(200).create_async().await;

    let path = write_ndjson(&vec![GET_USERS; 10]);
    let mut config = make_config(&server.url(), path.clone());
    config.max_records = Some(3);
    let config = Arc::new(config);
    let stream = open_local_stream(&path).await.unwrap();

    let mut interrupted = false;
    let acc = run_pipeline(config, stream, no_filters(), CancellationToken::new(), &mut interrupted)
        .await
        .unwrap();

    assert_eq!(acc.finalize().attempted, 3);
}

// ─── 9. CancellationToken sets interrupted flag ───────────────────────────────

#[tokio::test]
async fn test_cancellation_sets_interrupted_flag() {
    let cancel = CancellationToken::new();
    cancel.cancel(); // cancelled before pipeline starts

    let mut server = mockito::Server::new_async().await;
    let _m = server.mock("GET", "/api/users").with_status(200).create_async().await;

    let path = write_ndjson(&vec![GET_USERS; 500]);
    let config = Arc::new(make_config(&server.url(), path.clone()));
    let stream = open_local_stream(&path).await.unwrap();

    let mut interrupted = false;
    run_pipeline(config, stream, no_filters(), cancel, &mut interrupted)
        .await
        .unwrap();

    assert!(interrupted);
}

// ─── 10. URL construction — no double-slash ───────────────────────────────────

#[tokio::test]
async fn test_url_no_double_slash() {
    let mut server = mockito::Server::new_async().await;
    let _m = server
        .mock("GET", "/api/users")
        .with_status(200)
        .expect(1)
        .create_async()
        .await;

    let base = server.url().trim_end_matches('/').to_string();
    let path = write_ndjson(&[GET_USERS]);
    let config = Arc::new(make_config(&base, path.clone()));
    let stream = open_local_stream(&path).await.unwrap();

    let mut interrupted = false;
    let acc = run_pipeline(config, stream, no_filters(), CancellationToken::new(), &mut interrupted)
        .await
        .unwrap();

    assert_eq!(acc.finalize().attempted, 1);
}

// ─── 11. Empty input yields zero counts ──────────────────────────────────────

#[tokio::test]
async fn test_empty_input_yields_zero_counts() {
    let mut server = mockito::Server::new_async().await;
    let _m = server
        .mock("GET", mockito::Matcher::Any)
        .expect(0)
        .create_async()
        .await;

    let path = write_ndjson(&[]);
    let config = Arc::new(make_config(&server.url(), path.clone()));
    let stream = open_local_stream(&path).await.unwrap();

    let mut interrupted = false;
    let acc = run_pipeline(config, stream, no_filters(), CancellationToken::new(), &mut interrupted)
        .await
        .unwrap();

    let stats = acc.finalize();
    assert_eq!(stats.total_lines, 0);
    assert_eq!(stats.attempted, 0);
}

// ─── 12. PII — authorization header stripped ─────────────────────────────────

#[tokio::test]
async fn test_pii_auth_header_stripped() {
    let mut server = mockito::Server::new_async().await;
    // Mock matches only when Authorization is absent
    let _m = server
        .mock("GET", "/api/users")
        .match_header("authorization", mockito::Matcher::Missing)
        .with_status(200)
        .expect(1)
        .create_async()
        .await;

    let pii_line = r#"{"method":"GET","path":"/api/users","headers":{"authorization":"Bearer secret","accept":"application/json"},"status":200}"#;
    let path = write_ndjson(&[pii_line]);
    let config = Arc::new(make_config(&server.url(), path.clone()));
    let stream = open_local_stream(&path).await.unwrap();

    let mut interrupted = false;
    let acc = run_pipeline(config, stream, no_filters(), CancellationToken::new(), &mut interrupted)
        .await
        .unwrap();

    let stats = acc.finalize();
    assert_eq!(stats.attempted, 1);
    assert_eq!(stats.succeeded, 1, "auth must be stripped so mock returns 200");
}

// ─── 13. Status filter — FilterFactory builds from Config ────────────────────

#[tokio::test]
async fn test_status_filter_via_factory() {
    let mut server = mockito::Server::new_async().await;
    let _m = server.mock("GET", "/api/users").with_status(200).create_async().await;

    // Record with status=200 passes; record with status=404 is filtered
    let line_404 = r#"{"method":"GET","path":"/api/users","status":404}"#;
    let path = write_ndjson(&[GET_USERS, line_404]);

    use traffic_replayer::config::Config;
    let mut config = make_config(&server.url(), path.clone());
    config.statuses = vec![200]; // only replay status=200 records
    let chain = FilterFactory::from_config(&config);
    let config = Arc::new(config);
    let stream = open_local_stream(&path).await.unwrap();

    let mut interrupted = false;
    let acc = run_pipeline(config, stream, chain, CancellationToken::new(), &mut interrupted)
        .await
        .unwrap();

    let stats = acc.finalize();
    assert_eq!(stats.filtered_count, 1);
    assert_eq!(stats.attempted, 1);
}

// ─── 14. Combined filter+stats: filtered records not in attempted ─────────────

#[tokio::test]
async fn test_filtered_records_not_in_attempted() {
    let mut server = mockito::Server::new_async().await;
    let _m = server.mock("GET", "/api/users").with_status(200).create_async().await;

    let path = write_ndjson(&[GET_USERS, POST_USERS, GET_ADMIN]);
    let config = Arc::new(make_config(&server.url(), path.clone()));
    let stream = open_local_stream(&path).await.unwrap();

    // GET /api only
    let chain = FilterChain::new(vec![
        Box::new(MethodFilter::new(vec!["GET".into()])),
        Box::new(PathPrefixFilter::new(vec!["/api".into()])),
    ]);

    let mut interrupted = false;
    let acc = run_pipeline(config, stream, chain, CancellationToken::new(), &mut interrupted)
        .await
        .unwrap();

    let stats = acc.finalize();
    assert_eq!(stats.total_lines, 3);
    assert_eq!(stats.filtered_count, 2); // POST and /admin filtered
    assert_eq!(stats.attempted, 1);
    assert_eq!(stats.succeeded, 1);
}
