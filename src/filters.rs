//! Filtering pipeline for replay records.
//!
//! # Design patterns
//!   * **Chain of Responsibility** — `FilterChain` composes independent `Filter`s
//!     using AND logic between filter types.
//!   * **Factory**                — `FilterFactory` builds the chain from `Config`
//!     without the caller knowing which filter types exist.
//!   * **Open/Closed Principle**  — add new filter types by implementing `Filter`
//!     and registering one arm in `FilterFactory`; existing code is untouched.

use crate::config::Config;
use crate::models::ReplayRecord;

// ── Filter trait ──────────────────────────────────────────────────────────────

/// A single stateless predicate over a [`ReplayRecord`].
///
/// Implement this to introduce new filter strategies (e.g. header-match,
/// body-size limit, path-regex) without modifying the chain or any existing
/// filter.
pub trait Filter: Send + Sync {
    /// Returns `true` when the record should proceed to replay.
    fn matches(&self, record: &ReplayRecord) -> bool;

    /// Short human-readable label used in `debug!` logs.
    fn description(&self) -> String;
}

// ── FilterChain (Chain of Responsibility) ─────────────────────────────────────

/// Ordered sequence of [`Filter`]s.
///
/// **AND** logic between filters — a record must pass every filter.
/// Each individual filter applies **OR** logic across its candidate values.
pub struct FilterChain {
    filters: Vec<Box<dyn Filter>>,
}

impl FilterChain {
    pub fn new(filters: Vec<Box<dyn Filter>>) -> Self {
        FilterChain { filters }
    }

    /// `true` when no filters are configured (all records pass).
    pub fn is_empty(&self) -> bool {
        self.filters.is_empty()
    }

    /// `true` when the record passes every filter in the chain.
    pub fn matches(&self, record: &ReplayRecord) -> bool {
        self.filters.iter().all(|f| f.matches(record))
    }
}

// ── FilterFactory ─────────────────────────────────────────────────────────────

/// Constructs a [`FilterChain`] from validated [`Config`].
///
/// **Extension point**: to add a new filter type, implement [`Filter`] and add
/// one conditional push here.  No existing filter or the chain changes — OCP.
pub struct FilterFactory;

impl FilterFactory {
    pub fn from_config(config: &Config) -> FilterChain {
        let mut filters: Vec<Box<dyn Filter>> = Vec::new();

        if !config.methods.is_empty() {
            filters.push(Box::new(MethodFilter::new(config.methods.clone())));
        }
        if !config.path_prefixes.is_empty() {
            filters.push(Box::new(PathPrefixFilter::new(
                config.path_prefixes.clone(),
            )));
        }
        if !config.statuses.is_empty() {
            filters.push(Box::new(StatusFilter::new(config.statuses.clone())));
        }

        FilterChain::new(filters)
    }
}

// ── MethodFilter ──────────────────────────────────────────────────────────────

/// Passes records whose HTTP method matches any value in the allow-list.
/// Stored uppercased; comparison is case-insensitive.
pub struct MethodFilter {
    methods: Vec<String>,
}

impl MethodFilter {
    pub fn new(methods: Vec<String>) -> Self {
        MethodFilter {
            methods: methods.into_iter().map(|m| m.to_uppercase()).collect(),
        }
    }
}

impl Filter for MethodFilter {
    fn matches(&self, record: &ReplayRecord) -> bool {
        let upper = record.method.to_uppercase();
        self.methods.iter().any(|m| m == &upper)
    }

    fn description(&self) -> String {
        format!("method ∈ {:?}", self.methods)
    }
}

// ── PathPrefixFilter ──────────────────────────────────────────────────────────

/// Passes records whose path starts with **any** of the configured prefixes.
/// Example: `["/api/v1", "/health"]` matches both `/api/v1/users` and `/health`.
pub struct PathPrefixFilter {
    prefixes: Vec<String>,
}

impl PathPrefixFilter {
    pub fn new(prefixes: Vec<String>) -> Self {
        PathPrefixFilter { prefixes }
    }
}

impl Filter for PathPrefixFilter {
    fn matches(&self, record: &ReplayRecord) -> bool {
        self.prefixes.iter().any(|p| record.path.starts_with(p.as_str()))
    }

    fn description(&self) -> String {
        format!("path starts_with any of {:?}", self.prefixes)
    }
}

// ── StatusFilter ──────────────────────────────────────────────────────────────

/// Passes records whose recorded original status code matches any allowed value.
/// Records missing a status field are treated as `0` and never match.
pub struct StatusFilter {
    statuses: Vec<u16>,
}

impl StatusFilter {
    pub fn new(statuses: Vec<u16>) -> Self {
        StatusFilter { statuses }
    }
}

impl Filter for StatusFilter {
    fn matches(&self, record: &ReplayRecord) -> bool {
        let status = record.status.unwrap_or(0);
        self.statuses.iter().any(|s| *s == status)
    }

    fn description(&self) -> String {
        format!("status ∈ {:?}", self.statuses)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ReplayRecord;

    fn rec(method: &str, path: &str, status: u16) -> ReplayRecord {
        ReplayRecord {
            timestamp: None,
            method: method.to_string(),
            path: path.to_string(),
            query: None,
            headers: None,
            body: None,
            body_encoding: None,
            content_type: None,
            status: Some(status),
        }
    }

    // ── MethodFilter ─────────────────────────────────────────────────────────

    #[test]
    fn method_filter_passes_matching() {
        let f = MethodFilter::new(vec!["GET".into()]);
        assert!(f.matches(&rec("GET", "/", 200)));
    }

    #[test]
    fn method_filter_blocks_non_matching() {
        let f = MethodFilter::new(vec!["GET".into()]);
        assert!(!f.matches(&rec("POST", "/", 200)));
    }

    #[test]
    fn method_filter_case_insensitive() {
        let f = MethodFilter::new(vec!["GET".into()]);
        assert!(f.matches(&rec("get", "/", 200)));
        assert!(f.matches(&rec("Get", "/", 200)));
    }

    #[test]
    fn method_filter_or_across_values() {
        let f = MethodFilter::new(vec!["GET".into(), "POST".into()]);
        assert!(f.matches(&rec("GET", "/", 200)));
        assert!(f.matches(&rec("POST", "/", 200)));
        assert!(!f.matches(&rec("DELETE", "/", 200)));
    }

    // ── PathPrefixFilter ─────────────────────────────────────────────────────

    #[test]
    fn path_prefix_or_semantics() {
        let f = PathPrefixFilter::new(vec!["/api/v1".into(), "/health".into()]);
        assert!(f.matches(&rec("GET", "/api/v1/users", 200)));
        assert!(f.matches(&rec("GET", "/health", 200)));
        assert!(!f.matches(&rec("GET", "/other", 200)));
    }

    #[test]
    fn path_prefix_exact_match() {
        let f = PathPrefixFilter::new(vec!["/api".into()]);
        assert!(f.matches(&rec("GET", "/api", 200)));
        assert!(f.matches(&rec("GET", "/api/v1", 200)));
        assert!(!f.matches(&rec("GET", "/ap", 200)));
    }

    // ── StatusFilter ─────────────────────────────────────────────────────────

    #[test]
    fn status_filter_or_across_values() {
        let f = StatusFilter::new(vec![200, 201]);
        assert!(f.matches(&rec("GET", "/", 200)));
        assert!(f.matches(&rec("GET", "/", 201)));
        assert!(!f.matches(&rec("GET", "/", 404)));
    }

    #[test]
    fn status_filter_none_treated_as_zero() {
        let f = StatusFilter::new(vec![200]);
        let r = ReplayRecord {
            status: None,
            timestamp: None,
            method: "GET".into(),
            path: "/".into(),
            query: None,
            headers: None,
            body: None,
            body_encoding: None,
            content_type: None,
        };
        assert!(!f.matches(&r));
    }

    // ── FilterChain ──────────────────────────────────────────────────────────

    #[test]
    fn empty_chain_passes_all() {
        let chain = FilterChain::new(vec![]);
        assert!(chain.is_empty());
        assert!(chain.matches(&rec("DELETE", "/anything", 500)));
    }

    #[test]
    fn chain_and_logic() {
        let chain = FilterChain::new(vec![
            Box::new(MethodFilter::new(vec!["GET".into()])),
            Box::new(PathPrefixFilter::new(vec!["/api".into()])),
            Box::new(StatusFilter::new(vec![200])),
        ]);
        assert!(chain.matches(&rec("GET", "/api/v1", 200)));
        assert!(!chain.matches(&rec("POST", "/api/v1", 200)));
        assert!(!chain.matches(&rec("GET", "/other", 200)));
        assert!(!chain.matches(&rec("GET", "/api/v1", 404)));
    }

    // ── FilterFactory ────────────────────────────────────────────────────────

    #[test]
    fn factory_empty_config_empty_chain() {
        use crate::config::{Config, InputSource};
        use std::collections::HashSet;
        let config = Config {
            base_url: "http://localhost".into(),
            input_source: InputSource::LocalFile("/dev/null".into()),
            concurrency: 50,
            timeout_ms: 30000,
            connect_timeout_ms: 5000,
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
            drain_timeout_ms: 5000,
        };
        let chain = FilterFactory::from_config(&config);
        assert!(chain.is_empty());
    }
}
