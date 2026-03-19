use bytes::Bytes;
use indexmap::IndexMap;
use reqwest::header::HeaderMap;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

fn deserialize_headers<'de, D>(
    deserializer: D,
) -> Result<Option<IndexMap<String, Vec<String>>>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw: Option<IndexMap<String, Value>> = Option::deserialize(deserializer)?;
    match raw {
        None => Ok(None),
        Some(map) => {
            let mut result = IndexMap::new();
            for (key, val) in map {
                let values = match val {
                    Value::String(s) => vec![s],
                    Value::Array(arr) => arr
                        .into_iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect(),
                    _ => vec![val.to_string()],
                };
                result.insert(key.to_lowercase(), values);
            }
            Ok(Some(result))
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReplayRecord {
    pub timestamp: Option<String>,
    pub method: String,
    pub path: String,
    pub query: Option<String>,
    #[serde(default, deserialize_with = "deserialize_headers")]
    pub headers: Option<IndexMap<String, Vec<String>>>,
    pub body: Option<String>,
    pub body_encoding: Option<String>,
    pub content_type: Option<String>,
    pub status: Option<u16>,
}

#[derive(Debug, Clone)]
pub struct SanitizedReplayRecord {
    pub method: String,
    pub url: String,
    pub headers: HeaderMap,
    pub body: Option<Bytes>,
    pub content_type: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ReplayOutcome {
    pub status: Option<u16>,
    pub latency_us: u64,
    pub error: Option<String>,
    pub was_dry_run: bool,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize)]
pub struct AggregatedStats {
    pub total_lines: u64,
    pub parse_errors: u64,
    pub filtered_count: u64,
    pub attempted: u64,
    pub succeeded: u64,
    pub failed: u64,
    pub status_distribution: std::collections::HashMap<u16, u64>,
    pub error_categories: std::collections::HashMap<String, u64>,
    pub elapsed_secs: f64,
    pub throughput_rps: f64,
    pub p50_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── ReplayRecord deserialization ──────────────────────────────────────────

    #[test]
    fn test_deserialize_full_record() {
        let json = r#"{
            "timestamp": "2025-10-10T14:32:11Z",
            "method": "GET",
            "path": "/api/v1/hotels/123",
            "query": "?lang=en",
            "headers": {
                "accept": "application/json",
                "x-request-id": "abc-123"
            },
            "body": null,
            "body_encoding": "utf8",
            "content_type": "application/json",
            "status": 200
        }"#;
        let record: ReplayRecord = serde_json::from_str(json).unwrap();
        assert_eq!(record.method, "GET");
        assert_eq!(record.path, "/api/v1/hotels/123");
        assert_eq!(record.query.as_deref(), Some("?lang=en"));
        assert_eq!(record.status, Some(200));
        assert_eq!(record.content_type.as_deref(), Some("application/json"));
        assert_eq!(record.body_encoding.as_deref(), Some("utf8"));
        assert!(record.body.is_none());
        let hdrs = record.headers.unwrap();
        assert_eq!(hdrs["accept"], vec!["application/json"]);
        assert_eq!(hdrs["x-request-id"], vec!["abc-123"]);
    }

    #[test]
    fn test_deserialize_missing_optionals() {
        let json = r#"{"method": "DELETE", "path": "/resource/1"}"#;
        let record: ReplayRecord = serde_json::from_str(json).unwrap();
        assert_eq!(record.method, "DELETE");
        assert_eq!(record.path, "/resource/1");
        assert!(record.timestamp.is_none());
        assert!(record.query.is_none());
        assert!(record.headers.is_none());
        assert!(record.body.is_none());
        assert!(record.body_encoding.is_none());
        assert!(record.content_type.is_none());
        assert!(record.status.is_none());
    }

    #[test]
    fn test_deserialize_single_string_header() {
        let json = r#"{
            "method": "GET",
            "path": "/",
            "headers": {"accept": "text/html"}
        }"#;
        let record: ReplayRecord = serde_json::from_str(json).unwrap();
        let hdrs = record.headers.unwrap();
        assert_eq!(hdrs["accept"], vec!["text/html"]);
    }

    #[test]
    fn test_deserialize_array_header() {
        let json = r#"{
            "method": "GET",
            "path": "/",
            "headers": {"set-cookie": ["id=abc", "session=xyz"]}
        }"#;
        let record: ReplayRecord = serde_json::from_str(json).unwrap();
        let hdrs = record.headers.unwrap();
        assert_eq!(hdrs["set-cookie"], vec!["id=abc", "session=xyz"]);
    }

    #[test]
    fn test_deserialize_mixed_headers() {
        let json = r#"{
            "method": "POST",
            "path": "/submit",
            "headers": {
                "content-type": "application/json",
                "set-cookie": ["a=1", "b=2"],
                "x-trace-id": "xyz"
            }
        }"#;
        let record: ReplayRecord = serde_json::from_str(json).unwrap();
        let hdrs = record.headers.unwrap();
        assert_eq!(hdrs["content-type"], vec!["application/json"]);
        assert_eq!(hdrs["set-cookie"], vec!["a=1", "b=2"]);
        assert_eq!(hdrs["x-trace-id"], vec!["xyz"]);
    }

    #[test]
    fn test_deserialize_header_keys_lowercased() {
        let json = r#"{
            "method": "GET",
            "path": "/",
            "headers": {"Accept": "application/json", "X-Custom-Header": "value"}
        }"#;
        let record: ReplayRecord = serde_json::from_str(json).unwrap();
        let hdrs = record.headers.unwrap();
        // Keys must be stored lowercased
        assert!(hdrs.contains_key("accept"), "Key 'accept' not found (was it lowercased?)");
        assert!(hdrs.contains_key("x-custom-header"));
    }

    #[test]
    fn test_deserialize_base64_body() {
        // "aGVsbG8=" is base64 for "hello"
        let json = r#"{
            "method": "POST",
            "path": "/upload",
            "body": "aGVsbG8=",
            "body_encoding": "base64"
        }"#;
        let record: ReplayRecord = serde_json::from_str(json).unwrap();
        assert_eq!(record.body.as_deref(), Some("aGVsbG8="));
        assert_eq!(record.body_encoding.as_deref(), Some("base64"));
    }

    #[test]
    fn test_deserialize_null_body() {
        let json = r#"{"method": "GET", "path": "/", "body": null}"#;
        let record: ReplayRecord = serde_json::from_str(json).unwrap();
        assert!(record.body.is_none());
    }

    #[test]
    fn test_deserialize_utf8_body() {
        let json = r#"{
            "method": "POST",
            "path": "/api",
            "body": "{\"key\": \"value\"}",
            "body_encoding": "utf8",
            "content_type": "application/json"
        }"#;
        let record: ReplayRecord = serde_json::from_str(json).unwrap();
        assert_eq!(record.body.as_deref(), Some("{\"key\": \"value\"}"));
        assert_eq!(record.body_encoding.as_deref(), Some("utf8"));
    }

    #[test]
    fn test_deserialize_empty_headers_object() {
        let json = r#"{"method": "GET", "path": "/", "headers": {}}"#;
        let record: ReplayRecord = serde_json::from_str(json).unwrap();
        let hdrs = record.headers.unwrap();
        assert!(hdrs.is_empty());
    }

    #[test]
    fn test_deserialize_preserves_header_order() {
        // IndexMap preserves insertion order
        let json = r#"{
            "method": "GET",
            "path": "/",
            "headers": {
                "accept": "application/json",
                "authorization": "Bearer tok",
                "x-b": "2",
                "x-a": "1"
            }
        }"#;
        let record: ReplayRecord = serde_json::from_str(json).unwrap();
        let hdrs = record.headers.unwrap();
        let keys: Vec<&str> = hdrs.keys().map(|k| k.as_str()).collect();
        assert_eq!(keys, vec!["accept", "authorization", "x-b", "x-a"]);
    }

    #[test]
    fn test_deserialize_invalid_json_returns_error() {
        let bad = r#"{"method": "GET", "path": /no-quotes}"#;
        let result: Result<ReplayRecord, _> = serde_json::from_str(bad);
        assert!(result.is_err());
    }

    #[test]
    fn test_deserialize_missing_required_field_method() {
        // 'path' is also required, but serde gives an error if 'method' is absent
        let json = r#"{"path": "/api"}"#;
        let result: Result<ReplayRecord, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }
}
