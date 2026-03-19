use crate::config::Config;
use crate::error::TrafficReplayerError;
use reqwest::redirect;
use reqwest::Client;
use std::time::Duration;

pub fn build_client(config: &Config) -> Result<Client, TrafficReplayerError> {
    let redirect_policy = if config.follow_redirects {
        redirect::Policy::default()
    } else {
        redirect::Policy::none()
    };

    let client = Client::builder()
        .timeout(Duration::from_millis(config.timeout_ms))
        .connect_timeout(Duration::from_millis(config.connect_timeout_ms))
        .redirect(redirect_policy)
        .gzip(true)
        .pool_max_idle_per_host(config.concurrency as usize)
        .danger_accept_invalid_certs(config.insecure_skip_tls_verify)
        .build()?;

    Ok(client)
}
