//! `traffic-replayer` library crate.
//!
//! Exposes all public modules so that:
//!   * integration tests in `tests/` can access the full pipeline and helpers
//!   * downstream crates (e.g. a GUI wrapper) can embed the replay engine
//!
//! # Module map
//! ```text
//! traffic_replayer
//! ├── cli          – clap CLI struct definitions
//! ├── config       – validated Config + InputSource
//! ├── error        – TrafficReplayerError (thiserror)
//! ├── filters      – Filter trait, FilterChain, FilterFactory, concrete filters
//! ├── input        – RecordSource trait, SourceFactory, LocalSource, S3Source
//! ├── models       – ReplayRecord, SanitizedReplayRecord, ReplayOutcome, AggregatedStats
//! ├── replay
//! │   ├── client   – reqwest::Client builder
//! │   ├── pipeline – async pipeline loop
//! │   ├── sender   – RequestSender trait, HttpSender, DryRunSender, SenderFactory
//! │   └── worker   – dispatch helper
//! ├── sanitize     – Sanitizer strategies, RecordSanitizer, SanitizerFactory
//! ├── stats        – StatsAccumulator, hdrhistogram aggregator
//! └── summary      – stdout summary printer + JSON stats writer
//! ```

pub mod cli;
pub mod config;
pub mod error;
pub mod filters;
pub mod input;
pub mod models;
pub mod replay;
pub mod sanitize;
pub mod stats;
pub mod summary;
