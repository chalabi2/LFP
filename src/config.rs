use crate::error::AppError;
use lazy_static::lazy_static;
use std::collections::HashMap;
use std::env;
use std::time::Duration;

const DEFAULT_REQUEST_TIMEOUT: u64 = 2; // 5 seconds

// Map of networks that require special handling
lazy_static! {
    static ref NETWORK_TIMEOUTS: HashMap<&'static str, u64> = {
        let mut map = HashMap::new();
        // Networks that need longer timeouts
        map.insert("akash", 2);     // Akash needs 20 seconds
        map.insert("juno", 2);      // Juno needs 15 seconds
        map.insert("saga", 2);      // Saga needs 15 seconds
        map.insert("omniflix", 2);  // OmniFlix needs 15 seconds
        map.insert("althea", 2);    // Althea needs 15 seconds
        map
    };
}

/// Application configuration
#[derive(Debug, Clone)]
pub struct Config {
    /// Network RPC URLs mapped by network name
    pub networks: HashMap<String, String>,

    /// Request timeout in seconds
    pub request_timeout: u64,

    /// Network-specific timeouts
    pub network_timeouts: HashMap<String, Duration>,
}

impl Config {
    /// Load configuration from environment variables
    pub fn from_env() -> Result<Self, AppError> {
        // Initialize an empty network map
        let mut networks = HashMap::new();
        let mut network_timeouts = HashMap::new();

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

                // Check if this network needs a special timeout
                let network_lowercase = network_name.to_lowercase();
                if let Some(timeout) = NETWORK_TIMEOUTS.get(network_lowercase.as_str()) {
                    network_timeouts.insert(network_name.clone(), Duration::from_secs(*timeout));
                    tracing::info!(
                        "Using custom timeout of {}s for network {}",
                        timeout,
                        network_name
                    );
                }

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
            network_timeouts,
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

        // Generate fallback URL - try different well-known providers
        // Convert to lowercase for the URL format
        let network_lower = normalized_name.to_lowercase();

        // Generate a fallback URL based on the network
        let fallback_url = generate_fallback_url(&network_lower);

        Some((primary_url.clone(), fallback_url))
    }

    /// Get network-specific timeout if available, otherwise return default
    pub fn get_network_timeout(&self, network: &str) -> Duration {
        let normalized_name = normalize_network_name(network);

        // Check for custom timeout in config
        if let Some(timeout) = self.network_timeouts.get(&normalized_name) {
            return *timeout;
        }

        // Check predefined timeouts for known networks
        let network_lower = normalized_name.to_lowercase();
        if let Some(timeout) = NETWORK_TIMEOUTS.get(network_lower.as_str()) {
            return Duration::from_secs(*timeout);
        }

        // Return default timeout
        Duration::from_secs(self.request_timeout)
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

/// Generate the best fallback URL for a given network
fn generate_fallback_url(network: &str) -> String {
    // Use network name directly - no special case needed anymore
    let polkachu_network = network;

    // Try multiple fallback providers - return the first one that matches
    match network {
        // Networks with known custom providers
        "juno" => "https://juno-rpc.polkachu.com/".to_string(),
        "osmosis" => "https://osmosis-rpc.polkachu.com/".to_string(),
        "akash" => "https://akash-rpc.polkachu.com/".to_string(),
        "sentinel" => "https://sentinel-rpc.polkachu.com/".to_string(),
        "stargaze" => "https://stargaze-rpc.polkachu.com/".to_string(),
        "injective" => "https://injective-rpc.polkachu.com/".to_string(),
        "cosmoshub" => "https://cosmos-rpc.polkachu.com/".to_string(),
        "gravitybridge" => "https://gravity-rpc.polkachu.com/".to_string(),
        // Default to the generic Polkachu format
        _ => format!("https://{}-rpc.polkachu.com/", polkachu_network),
    }
}
