use std::sync::Arc;

use redis::aio::ConnectionManager;
use tokio::sync::Mutex;

use crate::{error::AppError, models::PeerGeoInfo};

// TTL in seconds for Redis keys (1 year - effectively permanent but allows cleanup if needed)
const PEER_KEY_TTL: usize = 31_536_000;

/// Wrapper around Redis connection manager for peer caching
pub struct RedisCache {
    client: ConnectionManager,
}

impl RedisCache {
    /// Create a new Redis cache instance
    pub async fn new(redis_url: &str) -> Result<Self, AppError> {
        // Create Redis client
        let client = redis::Client::open(redis_url)
            .map_err(|e| AppError::CacheError(format!("Failed to create Redis client: {}", e)))?;

        // Create connection manager for automatic reconnection
        let conn_manager = ConnectionManager::new(client)
            .await
            .map_err(|e| AppError::CacheError(format!("Failed to connect to Redis: {}", e)))?;

        // Log successful connection
        tracing::info!("Connected to Redis cache");

        Ok(Self {
            client: conn_manager,
        })
    }

    /// Store multiple peers in Redis
    pub async fn store_peers_batch(
        &self,
        network: &str,
        peers: &[PeerGeoInfo],
    ) -> Result<(), AppError> {
        // Get a connection from the pool
        let mut conn = self.client.clone();

        // Start pipeline for efficiency
        let mut pipe = redis::pipe();
        let network_set_key = format!("network:{}:peers", network);

        // Add each peer to the pipeline
        for peer in peers {
            let key = format!("peer:{}:{}", network, peer.ip);

            // Serialize peer to JSON
            let peer_json = serde_json::to_string(peer)
                .map_err(|e| AppError::CacheError(format!("Failed to serialize peer: {}", e)))?;

            // Add SET command to pipeline
            pipe.cmd("SET")
                .arg(&key)
                .arg(&peer_json)
                .arg("EX")
                .arg(PEER_KEY_TTL)
                .ignore();

            // Add to set of peers for this network
            pipe.cmd("SADD").arg(&network_set_key).arg(&key).ignore();
        }

        // Set TTL for the network set
        pipe.cmd("EXPIRE")
            .arg(&network_set_key)
            .arg(PEER_KEY_TTL)
            .ignore();

        // Execute the pipeline
        pipe.query_async::<_, ()>(&mut conn)
            .await
            .map_err(|e| AppError::CacheError(format!("Failed to store peers in Redis: {}", e)))?;

        Ok(())
    }

    /// Get all peers for a network
    pub async fn get_peers_by_network(&self, network: &str) -> Result<Vec<PeerGeoInfo>, AppError> {
        let mut conn = self.client.clone();
        let network_set_key = format!("network:{}:peers", network);

        // Get all peer keys for this network
        let peer_keys: Vec<String> = redis::cmd("SMEMBERS")
            .arg(&network_set_key)
            .query_async(&mut conn)
            .await
            .map_err(|e| {
                AppError::CacheError(format!("Failed to get peer keys from Redis: {}", e))
            })?;

        // If no peers found, return empty vector
        if peer_keys.is_empty() {
            return Ok(Vec::new());
        }

        // Get peer data for each key
        let mut peers = Vec::with_capacity(peer_keys.len());
        for key in peer_keys {
            let peer_json: Option<String> = redis::cmd("GET")
                .arg(&key)
                .query_async(&mut conn)
                .await
                .map_err(|e| {
                    AppError::CacheError(format!("Failed to get peer from Redis: {}", e))
                })?;

            if let Some(json) = peer_json {
                // Deserialize peer data
                match serde_json::from_str::<PeerGeoInfo>(&json) {
                    Ok(peer) => peers.push(peer),
                    Err(e) => {
                        tracing::warn!("Failed to deserialize peer data for key {}: {}", key, e);
                        // Continue with other peers
                    }
                }
            }
        }

        Ok(peers)
    }

    /// Delete peers for a network - used when refreshing the cache
    pub async fn delete_peers_by_network(&self, network: &str) -> Result<(), AppError> {
        let mut conn = self.client.clone();
        let network_set_key = format!("network:{}:peers", network);

        // Get all peer keys for this network
        let peer_keys: Vec<String> = redis::cmd("SMEMBERS")
            .arg(&network_set_key)
            .query_async(&mut conn)
            .await
            .map_err(|e| {
                AppError::CacheError(format!("Failed to get peer keys from Redis: {}", e))
            })?;

        // Delete each peer key
        let mut pipe = redis::pipe();
        for key in &peer_keys {
            pipe.cmd("DEL").arg(key).ignore();
        }

        // Delete the network set
        pipe.cmd("DEL").arg(&network_set_key).ignore();

        // Execute the pipeline
        pipe.query_async::<_, ()>(&mut conn).await.map_err(|e| {
            AppError::CacheError(format!("Failed to delete peers from Redis: {}", e))
        })?;

        tracing::info!("Deleted {} peers for network {}", peer_keys.len(), network);
        Ok(())
    }
}

// Create a global Redis cache instance
lazy_static::lazy_static! {
    pub static ref REDIS_CACHE: Arc<Mutex<Option<RedisCache>>> = Arc::new(Mutex::new(None));
}

/// Initialize the Redis cache
pub async fn init_redis_cache(redis_url: &str) -> Result<(), AppError> {
    let cache = RedisCache::new(redis_url).await?;

    // Store the cache in the global instance
    let mut cache_lock = REDIS_CACHE.lock().await;
    *cache_lock = Some(cache);

    Ok(())
}
