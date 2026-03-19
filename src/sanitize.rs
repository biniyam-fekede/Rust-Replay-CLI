//! PII sanitization for replay records.
//!
//! # Design patterns
//!   * **Strategy**              — `HeaderSanitizer`, `QuerySanitizer`, and
//!     `BodySanitizer` each encapsulate one sanitization strategy.
//!   * **Facade**                — `RecordSanitizer` composes the three strategies
//!     into a single, convenient `sanitize()` call.
//!   * **Factory**               — `SanitizerFactory::from_config` builds the
//!     facade without exposing construction details to callers.
//!   * **Open/Closed Principle** — extend sanitization by implementing new strategy
//!     structs; existing ones are never modified.
//!
//! # Denylist semantics
//! NOTE: Denylist matching is exact and case-insensitive.
//! Fields like `key` (music API) or `session` (game session) will be
//! redacted. Use --sanitize-passthrough-keys to exempt specific fields.
//! Substring matching was rejected due to excessive false positives.

use crate::config::Config;
use crate::error::TrafficReplayerError;
use crate::models::{ReplayRecord, SanitizedReplayRecord};
use base64::Engine;
use bytes::Bytes;
use indexmap::IndexMap;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;
use std::collections::HashSet;

// ── Denylist constants ────────────────────────────────────────────────────────

const HEADER_REMOVE_LIST: &[&str] = &[
    "authorization",
    "cookie",
    "set-cookie",
    "x-api-key",
    "proxy-authorization",
];

const QUERY_DENYLIST: &[&str] = &[
    "email", "phone", "token", "session", "ssn", "password", "auth", "key",
];

const BODY_DENYLIST: &[&str] = &[
    "password",
    "token",
    "authorization",
    "email",
    "phone",
    "ssn",
    "secret",
    "session",
    "cookie",
    "api_key",
];

/// Headers that reqwest manages itself; forwarding original values breaks requests.
const STRIP_BEFORE_SEND: &[&str] = &[
    "host",
    "content-length",
    "connection",
    "transfer-encoding",
    "keep-alive",
];

// ── Sanitizer marker trait ────────────────────────────────────────────────────

/// Marker trait for all sanitization strategies.
///
/// Implement this (plus a domain-specific `sanitize` method) to add new
/// sanitization strategies without touching existing ones — OCP compliant.
pub trait Sanitizer: Send + Sync {}

// ── HeaderSanitizer (Strategy) ────────────────────────────────────────────────

/// Removes sensitive headers from the header map before forwarding.
pub struct HeaderSanitizer {
    remove_list: &'static [&'static str],
    passthrough_keys: HashSet<String>,
}

impl HeaderSanitizer {
    pub fn new(passthrough_keys: HashSet<String>) -> Self {
        HeaderSanitizer {
            remove_list: HEADER_REMOVE_LIST,
            passthrough_keys,
        }
    }

    pub fn sanitize(
        &self,
        headers: &IndexMap<String, Vec<String>>,
    ) -> IndexMap<String, Vec<String>> {
        headers
            .iter()
            .filter(|(k, _)| {
                let lower = k.to_lowercase();
                // Passthrough keys are never removed
                if self.passthrough_keys.contains(&lower) {
                    return true;
                }
                !self.remove_list.contains(&lower.as_str())
            })
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

impl Sanitizer for HeaderSanitizer {}

// ── QuerySanitizer (Strategy) ─────────────────────────────────────────────────

/// Replaces sensitive query parameter values with `[REDACTED]`.
pub struct QuerySanitizer {
    denylist: &'static [&'static str],
    passthrough_keys: HashSet<String>,
}

impl QuerySanitizer {
    pub fn new(passthrough_keys: HashSet<String>) -> Self {
        QuerySanitizer {
            denylist: QUERY_DENYLIST,
            passthrough_keys,
        }
    }

    pub fn sanitize(&self, query: &str) -> String {
        let raw = query.strip_prefix('?').unwrap_or(query);
        if raw.is_empty() {
            return query.to_string();
        }

        let sanitized: Vec<String> = raw
            .split('&')
            .map(|pair| {
                let (key, _) = pair.split_once('=').unwrap_or((pair, ""));
                let lower = key.to_lowercase();
                if self.passthrough_keys.contains(&lower) {
                    return pair.to_string();
                }
                if self.denylist.contains(&lower.as_str()) {
                    format!("{}=[REDACTED]", key)
                } else {
                    pair.to_string()
                }
            })
            .collect();

        if query.starts_with('?') {
            format!("?{}", sanitized.join("&"))
        } else {
            sanitized.join("&")
        }
    }
}

impl Sanitizer for QuerySanitizer {}

// ── BodySanitizer (Strategy) ──────────────────────────────────────────────────

/// Recursively redacts sensitive keys inside a JSON body.
///
/// Gated on `content_type`: only attempted when the type contains
/// `"application/json"`.  All other content types pass through unchanged.
pub struct BodySanitizer {
    denylist: &'static [&'static str],
    passthrough_keys: HashSet<String>,
}

impl BodySanitizer {
    pub fn new(passthrough_keys: HashSet<String>) -> Self {
        BodySanitizer {
            denylist: BODY_DENYLIST,
            passthrough_keys,
        }
    }

    pub fn sanitize(&self, body: &str, content_type: Option<&str>) -> String {
        let is_json = content_type
            .map(|ct| ct.contains("application/json"))
            .unwrap_or(false);

        if !is_json {
            return body.to_string();
        }

        match serde_json::from_str::<Value>(body) {
            Ok(val) => serde_json::to_string(&self.redact(val))
                .unwrap_or_else(|_| body.to_string()),
            Err(e) => {
                tracing::warn!("JSON body parse failed during sanitization: {}", e);
                body.to_string()
            }
        }
    }

    /// Recursively walks maps and arrays, replacing denylist key values.
    fn redact(&self, value: Value) -> Value {
        match value {
            Value::Object(map) => {
                let redacted = map
                    .into_iter()
                    .map(|(k, v)| {
                        let lower = k.to_lowercase();
                        if self.passthrough_keys.contains(&lower) {
                            (k, v)
                        } else if self.denylist.contains(&lower.as_str()) {
                            (k, Value::String("[REDACTED]".to_string()))
                        } else {
                            (k, self.redact(v))
                        }
                    })
                    .collect();
                Value::Object(redacted)
            }
            Value::Array(arr) => {
                Value::Array(arr.into_iter().map(|v| self.redact(v)).collect())
            }
            other => other,
        }
    }
}

impl Sanitizer for BodySanitizer {}

// ── RecordSanitizer (Facade) ──────────────────────────────────────────────────

/// Composes the three sanitization strategies into one record-level operation.
///
/// Also handles URL construction, base64 body decoding, and stripping
/// hop-by-hop headers before building the final `SanitizedReplayRecord`.
pub struct RecordSanitizer {
    header_sanitizer: HeaderSanitizer,
    query_sanitizer: QuerySanitizer,
    body_sanitizer: BodySanitizer,
}

impl RecordSanitizer {
    pub fn new(passthrough_keys: HashSet<String>) -> Self {
        RecordSanitizer {
            header_sanitizer: HeaderSanitizer::new(passthrough_keys.clone()),
            query_sanitizer: QuerySanitizer::new(passthrough_keys.clone()),
            body_sanitizer: BodySanitizer::new(passthrough_keys),
        }
    }

    pub fn sanitize(
        &self,
        record: ReplayRecord,
        base_url: &str,
    ) -> Result<SanitizedReplayRecord, TrafficReplayerError> {
        // 1. Sanitize headers
        let clean_headers = record
            .headers
            .as_ref()
            .map(|h| self.header_sanitizer.sanitize(h))
            .unwrap_or_default();

        // 2. Sanitize query and build URL
        let clean_query = record
            .query
            .as_deref()
            .map(|q| self.query_sanitizer.sanitize(q))
            .unwrap_or_default();

        let url = if clean_query.is_empty() {
            format!("{}{}", base_url, record.path)
        } else {
            let q = if clean_query.starts_with('?') {
                clean_query.clone()
            } else {
                format!("?{}", clean_query)
            };
            format!("{}{}{}", base_url, record.path, q)
        };

        // 3. Decode / sanitize body
        let body_bytes: Option<Bytes> = match record.body.as_deref() {
            None => None,
            Some(body_str) => {
                let encoding = record.body_encoding.as_deref().unwrap_or("utf8");
                if encoding == "base64" {
                    let decoded = base64::engine::general_purpose::STANDARD
                        .decode(body_str)
                        .map_err(|e| TrafficReplayerError::Base64Decode(e.to_string()))?;
                    Some(Bytes::from(decoded))
                } else {
                    let clean = self
                        .body_sanitizer
                        .sanitize(body_str, record.content_type.as_deref());
                    Some(Bytes::from(clean.into_bytes()))
                }
            }
        };

        // 4. Build HeaderMap; strip hop-by-hop headers reqwest manages
        let mut header_map = HeaderMap::new();
        for (k, vals) in &clean_headers {
            let lower = k.to_lowercase();
            if STRIP_BEFORE_SEND.contains(&lower.as_str()) {
                continue;
            }
            if let Ok(name) = HeaderName::from_bytes(lower.as_bytes()) {
                for v in vals {
                    if let Ok(val) = HeaderValue::from_str(v) {
                        header_map.append(name.clone(), val);
                    }
                }
            }
        }

        Ok(SanitizedReplayRecord {
            method: record.method,
            url,
            headers: header_map,
            body: body_bytes,
            content_type: record.content_type,
        })
    }
}

// ── SanitizerFactory ──────────────────────────────────────────────────────────

/// Builds a [`RecordSanitizer`] from [`Config`].
///
/// **Extension point**: to add a new sanitization strategy, create a new
/// struct implementing [`Sanitizer`] and compose it here.  Existing sanitizers
/// are untouched — OCP compliant.
pub struct SanitizerFactory;

impl SanitizerFactory {
    pub fn from_config(config: &Config) -> RecordSanitizer {
        RecordSanitizer::new(config.passthrough_keys.clone())
    }
}

// ── Backward-compatible free functions (used by unit tests) ──────────────────

pub fn sanitize_headers(
    headers: &IndexMap<String, Vec<String>>,
    passthrough_keys: &HashSet<String>,
) -> IndexMap<String, Vec<String>> {
    HeaderSanitizer::new(passthrough_keys.clone()).sanitize(headers)
}

pub fn sanitize_query(query: &str, passthrough_keys: &HashSet<String>) -> String {
    QuerySanitizer::new(passthrough_keys.clone()).sanitize(query)
}

pub fn sanitize_body(
    body: &str,
    content_type: Option<&str>,
    passthrough_keys: &HashSet<String>,
) -> String {
    BodySanitizer::new(passthrough_keys.clone()).sanitize(body, content_type)
}

pub fn sanitize_record(
    record: ReplayRecord,
    config: &Config,
    base_url: &str,
) -> Result<SanitizedReplayRecord, TrafficReplayerError> {
    SanitizerFactory::from_config(config).sanitize(record, base_url)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;

    fn no_passthrough() -> HashSet<String> {
        HashSet::new()
    }

    // ── HeaderSanitizer ──────────────────────────────────────────────────────

    #[test]
    fn header_sanitizer_removes_authorization() {
        let mut h = IndexMap::new();
        h.insert("authorization".into(), vec!["Bearer tok".into()]);
        h.insert("accept".into(), vec!["application/json".into()]);
        let result = HeaderSanitizer::new(no_passthrough()).sanitize(&h);
        assert!(!result.contains_key("authorization"));
        assert!(result.contains_key("accept"));
    }

    #[test]
    fn header_sanitizer_removes_cookie_and_set_cookie() {
        let mut h = IndexMap::new();
        h.insert("cookie".into(), vec!["s=abc".into()]);
        h.insert("set-cookie".into(), vec!["id=xyz".into()]);
        let result = HeaderSanitizer::new(no_passthrough()).sanitize(&h);
        assert!(!result.contains_key("cookie"));
        assert!(!result.contains_key("set-cookie"));
    }

    #[test]
    fn header_sanitizer_passthrough_key_exempts_header() {
        let mut h = IndexMap::new();
        h.insert("authorization".into(), vec!["Bearer tok".into()]);
        let mut pt = HashSet::new();
        pt.insert("authorization".into());
        let result = HeaderSanitizer::new(pt).sanitize(&h);
        assert!(result.contains_key("authorization"));
    }

    // ── QuerySanitizer ───────────────────────────────────────────────────────

    #[test]
    fn query_sanitizer_redacts_email() {
        let result = QuerySanitizer::new(no_passthrough())
            .sanitize("?email=test@example.com&lang=en");
        assert!(result.contains("email=[REDACTED]"));
        assert!(result.contains("lang=en"));
    }

    #[test]
    fn query_sanitizer_preserves_leading_question_mark() {
        let result = QuerySanitizer::new(no_passthrough()).sanitize("?foo=bar");
        assert!(result.starts_with('?'));
    }

    #[test]
    fn query_sanitizer_passthrough_exempts_key() {
        let mut pt = HashSet::new();
        pt.insert("session".into());
        let result = QuerySanitizer::new(pt).sanitize("?session=abc123");
        assert!(result.contains("session=abc123"));
    }

    // ── BodySanitizer ────────────────────────────────────────────────────────

    #[test]
    fn body_sanitizer_redacts_json_password() {
        let body = r#"{"password":"secret","username":"alice"}"#;
        let result = BodySanitizer::new(no_passthrough())
            .sanitize(body, Some("application/json"));
        let v: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["password"], "[REDACTED]");
        assert_eq!(v["username"], "alice");
    }

    #[test]
    fn body_sanitizer_non_json_passes_through() {
        let body = "password=secret&user=admin";
        let result = BodySanitizer::new(no_passthrough())
            .sanitize(body, Some("application/x-www-form-urlencoded"));
        assert_eq!(result, body);
    }

    #[test]
    fn body_sanitizer_no_content_type_passes_through() {
        let body = r#"{"password":"secret"}"#;
        let result = BodySanitizer::new(no_passthrough()).sanitize(body, None);
        assert_eq!(result, body);
    }

    #[test]
    fn body_sanitizer_nested_json_fully_redacted() {
        let body = r#"{"user":{"password":"x","email":"a@b.com"},"data":"ok"}"#;
        let result = BodySanitizer::new(no_passthrough())
            .sanitize(body, Some("application/json"));
        let v: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["user"]["password"], "[REDACTED]");
        assert_eq!(v["user"]["email"], "[REDACTED]");
        assert_eq!(v["data"], "ok");
    }

    #[test]
    fn body_sanitizer_passthrough_key_exempts_body_field() {
        let mut pt = HashSet::new();
        pt.insert("token".into());
        let body = r#"{"token":"keep-me","password":"remove-me"}"#;
        let result = BodySanitizer::new(pt).sanitize(body, Some("application/json"));
        let v: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["token"], "keep-me");
        assert_eq!(v["password"], "[REDACTED]");
    }

    // ── RecordSanitizer (facade) ─────────────────────────────────────────────

    #[test]
    fn record_sanitizer_builds_url_with_query() {
        use crate::models::ReplayRecord;
        let record = ReplayRecord {
            method: "GET".into(),
            path: "/search".into(),
            query: Some("?q=hello&token=secret".into()),
            headers: None,
            body: None,
            body_encoding: None,
            content_type: None,
            status: None,
            timestamp: None,
        };
        let san = RecordSanitizer::new(no_passthrough());
        let result = san.sanitize(record, "http://localhost:8080").unwrap();
        assert!(result.url.contains("token=[REDACTED]"));
        assert!(result.url.contains("q=hello"));
        assert!(!result.url.contains("//search")); // no double slash
    }

    #[test]
    fn record_sanitizer_decodes_base64_body() {
        use crate::models::ReplayRecord;
        // base64("hello") = "aGVsbG8="
        let record = ReplayRecord {
            method: "POST".into(),
            path: "/upload".into(),
            query: None,
            headers: None,
            body: Some("aGVsbG8=".into()),
            body_encoding: Some("base64".into()),
            content_type: Some("application/octet-stream".into()),
            status: None,
            timestamp: None,
        };
        let san = RecordSanitizer::new(no_passthrough());
        let result = san.sanitize(record, "http://localhost").unwrap();
        assert_eq!(result.body.unwrap().as_ref(), b"hello");
    }
}
