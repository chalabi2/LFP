use crate::error::AppError;
use once_cell::sync::Lazy;
use reqwest::Client;
use std::time::Duration;

// Define a static set of problematic endpoint patterns to avoid
pub static PROBLEMATIC_ENDPOINT_PATTERNS: Lazy<Vec<&'static str>> = Lazy::new(|| {
    vec![
        // Loopback addresses (but not 0.0.0.0 which we can replace with the peer's IP)
        "tcp://127.0.0.1:",
        "http://127.0.0.1:",
        "tcp://localhost:",
        "http://localhost:",
        // Problematic IPv6 addresses that often fail
        "[2001:bc8:",
        "[2a01:4f",
        // Internal networks
        "tcp://10.",
        "http://10.",
        "tcp://192.168.",
        "http://192.168.",
        "tcp://172.16.",
        "http://172.16.",
        "tcp://172.17.",
        "http://172.17.",
        "tcp://172.18.",
        "http://172.18.",
        "tcp://172.19.",
        "http://172.19.",
        "tcp://172.2",
        "http://172.2",
        "tcp://172.3",
        "http://172.3",
    ]
});

/// Create an HTTP client with appropriate configuration for peer discovery
pub fn create_http_client(timeout: Duration) -> Result<Client, AppError> {
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .user_agent("Mozilla/5.0 PeerFinder/0.1.0 (https://github.com/user/peerfinder)")
        // Add connection timeout separate from request timeout
        .connect_timeout(Duration::from_secs(10))
        // Increase default timeout for slow nodes
        .pool_idle_timeout(Duration::from_secs(90))
        // Increase max connections per host to help with high latency networks
        .pool_max_idle_per_host(5) // Reduce from 10 to avoid rate limiting
        // Add redirects
        .redirect(reqwest::redirect::Policy::limited(5))
        // Disable certificate verification for some networks with self-signed certs
        .danger_accept_invalid_certs(true)
        .build()
        .map_err(|e| AppError::RequestError(format!("Failed to create HTTP client: {}", e)))?;

    Ok(client)
}

/// Check if an IP address is a valid public IP
pub fn is_valid_public_ip(ip: &str) -> bool {
    crate::utils::is_valid_public_ip(ip)
}

/// Check if an endpoint contains problematic patterns
pub fn has_problematic_pattern(url: &str) -> bool {
    PROBLEMATIC_ENDPOINT_PATTERNS
        .iter()
        .any(|pattern| url.contains(pattern))
}
