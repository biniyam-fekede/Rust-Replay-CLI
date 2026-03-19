//! Entry point — intentionally thin.
//!
//! Responsibilities:
//!   1. Parse CLI args
//!   2. Initialise tracing → stderr
//!   3. Build `Config`
//!   4. Wire source, filter chain, and pipeline via factories
//!   5. Print summary + write stats JSON
//!   6. Exit with the appropriate exit code

use clap::Parser;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

use traffic_replayer::cli::Cli;
use traffic_replayer::config::Config;
use traffic_replayer::error::TrafficReplayerError;
use traffic_replayer::filters::FilterFactory;
use traffic_replayer::input::SourceFactory;
use traffic_replayer::replay::pipeline::run_pipeline;
use traffic_replayer::summary;

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    let filter = if cli.verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::new("info")
    };
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .init();

    std::process::exit(match run(cli).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("Fatal error: {e}");
            1
        }
    });
}

async fn run(cli: Cli) -> Result<i32, TrafficReplayerError> {
    let config = Arc::new(Config::from_cli(cli)?);

    tracing::info!(
        base_url  = %config.base_url,
        concurrency = config.concurrency,
        dry_run   = config.dry_run,
        "Starting traffic-replayer"
    );

    // ── Open input source via factory ─────────────────────────────────────────
    let source = SourceFactory::create(&config).await.map_err(|e| {
        tracing::error!("Cannot open input source: {}", e);
        e
    })?;
    let stream = source.into_stream();

    // ── Build filter chain via factory ────────────────────────────────────────
    let filter_chain = FilterFactory::from_config(&config);
    if !filter_chain.is_empty() {
        tracing::info!("Filters active");
    }

    // ── SIGINT → CancellationToken ────────────────────────────────────────────
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            tracing::info!("SIGINT received — draining in-flight requests");
            cancel_clone.cancel();
        }
    });

    // ── Run pipeline ──────────────────────────────────────────────────────────
    let mut interrupted = false;
    let acc =
        run_pipeline(config.clone(), stream, filter_chain, cancel, &mut interrupted).await?;

    let stats = acc.finalize();

    // ── Output ────────────────────────────────────────────────────────────────
    summary::print_summary(&stats, interrupted);

    if let Some(ref path) = config.output_stats {
        match summary::write_stats_json(&stats, path) {
            Ok(()) => tracing::info!("Stats written to {}", path.display()),
            Err(e) => tracing::error!("Failed to write stats to {}: {}", path.display(), e),
        }
    }

    tracing::info!("Run complete");
    Ok(if interrupted { 2 } else { 0 })
}
