// Modules
mod cache;
mod geo;
mod memory;
mod network_scanner;
mod peer_parser;
mod peer_traversal;
mod service;
mod utils;

// Public exports
pub use memory::run_memory_only_discovery;
pub use service::{spawn_discovery_service, spawn_memory_only_discovery, trigger_network_scan};

// Internal exports for use within the module
pub(crate) use cache::ALL_DISCOVERED_PEERS;
pub(crate) use cache::KNOWN_GOOD_PEERS;
