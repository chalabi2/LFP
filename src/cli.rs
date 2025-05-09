use clap::Parser;
use std::time::Duration;

/// Command-line interface for the PeerFinder application
#[derive(Parser, Debug, Clone)]
#[clap(
    name = "peerFinder",
    about = "A Rust web server that rapidly discovers and tracks blockchain network peers",
    version
)]
pub struct Cli {
    /// Port to run the web server on
    #[clap(short, long, default_value = "8080")]
    pub port: u16,

    /// Database URL for storing peer information
    #[clap(long, env("DATABASE_URL"))]
    pub database_url: Option<String>,

    /// Scan interval in seconds
    #[clap(long, default_value = "43200")] // Default: 12 hours
    pub scan_interval: u64,

    /// Specific blockchain network to scan
    #[clap(long)]
    pub network: Option<String>,

    /// Maximum number of peers to discover per network (0 = unlimited)
    #[clap(long, default_value = "0")]
    pub max_peers: usize,

    /// Enable continuous background scanning
    #[clap(long, default_value = "true")]
    pub continuous: bool,

    /// Maximum depth for recursive peer discovery (0 = unlimited)
    #[clap(long, default_value = "0")]
    pub max_depth: usize,

    /// Timeout for HTTP requests in seconds
    #[clap(long, default_value = "5")]
    pub request_timeout: u64,

    /// Run an immediate scan on startup
    #[clap(long, default_value = "true")]
    pub scan_on_startup: bool,

    /// The maximum number of concurrent requests
    #[clap(long, default_value = "100")]
    pub max_concurrent_requests: usize,
}

impl Cli {
    /// Get the request timeout as a Duration
    pub fn request_timeout(&self) -> Duration {
        Duration::from_secs(self.request_timeout)
    }

    /// Get the scan interval as a Duration
    pub fn scan_interval(&self) -> Duration {
        Duration::from_secs(self.scan_interval)
    }

    /// Check if there's no limit on recursive depth
    pub fn is_unlimited_depth(&self) -> bool {
        self.max_depth == 0
    }

    /// Check if there's no limit on number of peers
    pub fn is_unlimited_peers(&self) -> bool {
        self.max_peers == 0
    }
}
