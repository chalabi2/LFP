use crate::error::AppError;
use lazy_static::lazy_static;
use std::collections::HashMap;
use std::env;

const DEFAULT_REQUEST_TIMEOUT: u64 = 5; // 5 seconds
const RPC_TIMEOUT: u64 = 10; // 10 seconds for RPC requests

/// Application configuration
#[derive(Debug, Clone)]
pub struct Config {
    /// Network RPC URLs mapped by network name
    pub networks: HashMap<String, String>,

    /// Request timeout in seconds
    pub request_timeout: u64,
}

impl Config {
    /// Load configuration from environment variables
    pub fn from_env() -> Result<Self, AppError> {
        // Initialize an empty network map
        let mut networks = HashMap::new();

        // RPC URL environment variables follow the pattern NEXT_PUBLIC_<NETWORK>_RPC_URL
        for (key, value) in env::vars() {
            if key.starts_with("NEXT_PUBLIC_") && key.ends_with("_RPC_URL") {
                // Extract network name from the environment variable key
                let network_part = key
                    .strip_prefix("NEXT_PUBLIC_")
                    .unwrap()
                    .strip_suffix("_RPC_URL")
                    .unwrap();

                // Convert to a standardized format (capitalize first letter, lowercase rest)
                let network_name = normalize_network_name(network_part);

                // Add to our networks map
                networks.insert(network_name, value);
            }
        }

        // Get request timeout or use default
        let request_timeout = env::var("REQUEST_TIMEOUT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_REQUEST_TIMEOUT);

        if networks.is_empty() {
            tracing::warn!("No network RPC URLs found in environment variables");
        } else {
            tracing::info!("Loaded {} networks from environment", networks.len());
        }

        Ok(Config {
            networks,
            request_timeout,
        })
    }

    /// Get the list of configured network names
    pub fn network_names(&self) -> Vec<String> {
        self.networks.keys().cloned().collect()
    }

    /// Get the RPC URL for a specific network
    pub fn get_rpc_url(&self, network: &str) -> Option<&String> {
        // Try with the original name first
        self.networks
            .get(network)
            // Try with the normalized name as fallback
            .or_else(|| self.networks.get(&normalize_network_name(network)))
    }

    /// Get both primary and fallback RPC URLs for a network
    pub fn get_rpc_urls(&self, network: &str) -> Option<(String, String)> {
        let normalized_name = normalize_network_name(network);

        // Get primary URL
        let primary_url = self.get_rpc_url(&normalized_name)?;

        // Generate Polkachu fallback URL
        // Convert to lowercase for the Polkachu URL format
        let network_lower = normalized_name.to_lowercase();

        // Special case for OmniFlixHub
        let polkachu_network = if network_lower == "omniflixhub" {
            "omniflix".to_string()
        } else {
            network_lower
        };

        let fallback_url = format!("https://{}-rpc.polkachu.com/", polkachu_network);

        Some((primary_url.clone(), fallback_url))
    }
}

/// Helper function to normalize network names for consistent lookup
fn normalize_network_name(name: &str) -> String {
    // Split by common delimiters and remove version numbers
    let parts: Vec<&str> = name
        .split(|c| c == '_' || c == '-')
        .filter(|&part| !part.chars().all(|c| c.is_ascii_digit()))
        .collect();

    // Capitalize the first character of each part and join
    parts
        .iter()
        .map(|&part| {
            let mut chars = part.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => {
                    let first_upper = first.to_uppercase().collect::<String>();
                    let rest: String = chars.collect();
                    format!("{}{}", first_upper, rest.to_lowercase())
                }
            }
        })
        .collect()
}
