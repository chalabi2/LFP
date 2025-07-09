use crate::models::PeerInfo;
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use std::time::{Duration, Instant};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

/// A static cache of known good peer endpoints by network
pub static KNOWN_GOOD_PEERS: Lazy<Arc<Mutex<HashMap<String, Vec<PeerInfo>>>>> =
    Lazy::new(|| Arc::new(Mutex::new(HashMap::new())));

/// A static cache of all discovered peer endpoints by network
pub static ALL_DISCOVERED_PEERS: Lazy<Arc<Mutex<HashMap<String, HashSet<PeerInfo>>>>> =
    Lazy::new(|| Arc::new(Mutex::new(HashMap::new())));

/// A static cache for tracking failed endpoints
pub static FAILED_ENDPOINTS: Lazy<Mutex<HashMap<String, (Instant, usize)>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Cooldown period for failed endpoints (5 minutes)
#[allow(dead_code)]
const ENDPOINT_COOLDOWN: Duration = Duration::from_secs(0);
/// Maximum failure count before longer cooldown
#[allow(dead_code)]
const MAX_FAILURES: usize = 1;
/// Extended cooldown for repeated failures (30 minutes)
#[allow(dead_code)]
const EXTENDED_COOLDOWN: Duration = Duration::from_secs(0);

/// Add or replace peers for a specific network in the cache
pub fn update_cached_peers(network: &str, peers: Vec<PeerInfo>, max_peers_to_cache: usize) {
    let mut peers_guard = KNOWN_GOOD_PEERS.lock();
    let network_peers = peers_guard
        .entry(network.to_string())
        .or_insert_with(Vec::new);

    // Clear existing peers and add new ones with RPC addresses
    network_peers.clear();

    // Only store up to max_peers_to_cache peers to keep the cache manageable
    if peers.len() > max_peers_to_cache {
        network_peers.extend(peers.into_iter().take(max_peers_to_cache));
    } else {
        network_peers.extend(peers);
    }
}

/// Add all discovered peers to the ALL_DISCOVERED_PEERS cache
pub fn add_all_discovered_peers(network: &str, peers: Vec<PeerInfo>) {
    let mut all_peers_guard = ALL_DISCOVERED_PEERS.lock();
    let network_peers = all_peers_guard
        .entry(network.to_string())
        .or_insert_with(HashSet::new);

    // Add all discovered peers to the set
    for peer in peers {
        network_peers.insert(peer);
    }
}

/// Get cached peers for a specific network
pub fn get_cached_peers(network: &str) -> Vec<PeerInfo> {
    let peers_guard = KNOWN_GOOD_PEERS.lock();
    peers_guard.get(network).cloned().unwrap_or_default()
}

/// Get all discovered peers for a specific network
pub fn _get_all_discovered_peers(network: &str) -> Vec<PeerInfo> {
    let all_peers_guard = ALL_DISCOVERED_PEERS.lock();
    match all_peers_guard.get(network) {
        Some(peers_set) => peers_set.iter().cloned().collect(),
        None => Vec::new(),
    }
}

/// Mark an endpoint as failed to avoid repeatedly trying non-working endpoints
pub fn mark_endpoint_failed(network: &str, url: &str) {
    let key = format!("{}:{}", network, url);
    let mut failed_guard = FAILED_ENDPOINTS.lock();

    let now = Instant::now();
    let entry = failed_guard.entry(key).or_insert((now, 0));

    // Increment failure count
    entry.1 += 1;
    // Update timestamp
    entry.0 = now;

    tracing::debug!(
        "Marked endpoint as failed: {} for network {}, failure count: {}",
        url,
        network,
        entry.1
    );
}

/// Check if an endpoint is currently in a failed cooldown period
pub fn _is_endpoint_failed(network: &str, url: &str) -> bool {
    let key = format!("{}:{}", network, url);
    let failed_guard = FAILED_ENDPOINTS.lock();

    if let Some((timestamp, count)) = failed_guard.get(&key) {
        let now = Instant::now();
        let cooldown = if *count >= MAX_FAILURES {
            EXTENDED_COOLDOWN
        } else {
            ENDPOINT_COOLDOWN
        };

        if now.duration_since(*timestamp) < cooldown {
            tracing::debug!(
                "Skipping failed endpoint {} for network {}, in cooldown for {:?} more",
                url,
                network,
                cooldown.checked_sub(now.duration_since(*timestamp))
            );
            return true;
        }
    }

    false
}
