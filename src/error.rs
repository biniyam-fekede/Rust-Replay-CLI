use thiserror::Error;

#[derive(Debug, Error)]
pub enum TrafficReplayerError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("S3 error: {0}")]
    S3(String),

    #[error("JSON parse error: {0}")]
    JsonParse(#[from] serde_json::Error),

    #[error("URL construction error: {0}")]
    UrlConstruction(String),

    #[error("Request error: {0}")]
    Request(#[from] reqwest::Error),

    #[error("Config validation error: {0}")]
    ConfigValidation(String),

    #[error("Base64 decode error: {0}")]
    Base64Decode(String),
}
