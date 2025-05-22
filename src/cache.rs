use chrono::Utc;
use lazy_static::lazy_static;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::error::AppError;
use crate::models::{AppCache, PeerNode, PeerStats};

/// Global cache storage for database results to reduce database load
#[derive(Debug, Default)]
pub struct DatabaseCache {
    /// Cached stats for overall peer stats
    pub stats: Option<PeerStats>,
    /// Cached peers by network
    pub peers_by_network: HashMap<String, Vec<PeerNode>>,
    /// Cache configuration
    pub config: AppCache,
}

// Create a global cache instance
lazy_static! {
    pub static ref DB_CACHE: Arc<Mutex<DatabaseCache>> =
        Arc::new(Mutex::new(DatabaseCache::default()));
}

impl DatabaseCache {
    /// Check if cache is stale and needs refresh
    pub fn is_stale(&self) -> bool {
        match self.config.last_refresh {
            Some(last_refresh) => {
                let now = Utc::now();
                let elapsed = now.signed_duration_since(last_refresh);
                elapsed.num_seconds() > self.config.cache_refresh_interval as i64
            }
            None => true, // No previous refresh, so it's stale
        }
    }

    /// Update cache configuration
    pub fn update_config(&mut self, ttl: Option<u64>, refresh_interval: Option<u64>) {
        if let Some(ttl) = ttl {
            self.config.cache_ttl = ttl;
        }

        if let Some(refresh_interval) = refresh_interval {
            self.config.cache_refresh_interval = refresh_interval;
        }
    }

    /// Update cache with new stats data
    pub fn update_stats(&mut self, stats: PeerStats) {
        self.stats = Some(stats);
        self.config.last_refresh = Some(Utc::now());
    }

    /// Update cache with peers for a specific network
    pub fn update_peers_for_network(&mut self, network: &str, peers: Vec<PeerNode>) {
        self.peers_by_network.insert(network.to_string(), peers);
        self.config.last_refresh = Some(Utc::now());
    }

    /// Get peers for a network from cache if fresh, or return None if stale
    pub fn get_peers_for_network(&self, network: &str) -> Option<Vec<PeerNode>> {
        if self.is_stale() {
            return None;
        }

        self.peers_by_network.get(network).cloned()
    }

    /// Get stats from cache if fresh, or return None if stale
    pub fn get_stats(&self) -> Option<PeerStats> {
        if self.is_stale() {
            return None;
        }

        self.stats.clone()
    }
}

/// Configure the cache with custom settings
pub async fn configure_cache(
    ttl: Option<u64>,
    refresh_interval: Option<u64>,
) -> Result<(), AppError> {
    let mut cache = DB_CACHE.lock().await;
    cache.update_config(ttl, refresh_interval);
    Ok(())
}

/// Clear the entire cache
pub async fn clear_cache() -> Result<(), AppError> {
    let mut cache = DB_CACHE.lock().await;
    *cache = DatabaseCache::default();
    Ok(())
}
