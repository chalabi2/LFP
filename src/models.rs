use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::hash::{Hash, Hasher};
use uuid::Uuid;

/// Basic peer information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInfo {
    pub ip: String,
    pub rpc_address: Option<String>,
}

// Implement Hash for PeerInfo
impl Hash for PeerInfo {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.ip.hash(state);
        // Skip hashing rpc_address since we want to consider peers with the same IP
        // but different RPC addresses as the same peer
    }
}

// Implement PartialEq for PeerInfo
impl PartialEq for PeerInfo {
    fn eq(&self, other: &Self) -> bool {
        self.ip == other.ip
    }
}

// Implement Eq for PeerInfo
impl Eq for PeerInfo {}

/// Extended peer information with geo data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerGeoInfo {
    pub ip: String,
    #[serde(rename = "rpcAddress")]
    pub rpc_address: Option<String>,
    pub network: String,
    pub country: Option<String>,
    pub region: Option<String>,   // Full region name
    pub province: Option<String>, // Some countries use province
    pub state: Option<String>,    // Some countries use state (region code)
    pub city: Option<String>,     // City name
    pub isp: Option<String>,      // Internet Service Provider
    pub lat: Option<f64>,         // Latitude
    pub lon: Option<f64>,         // Longitude
    #[serde(rename = "lastSeen")]
    pub last_seen: Option<DateTime<Utc>>,
    pub active: bool,
}

/// Represents a peer node stored in the database
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct PeerNode {
    pub id: Uuid,
    pub ip: String,
    pub network: String,
    #[serde(rename = "rpcAddress")]
    pub rpc_address: Option<String>,
    pub country: Option<String>,
    pub region: Option<String>,
    pub province: Option<String>,
    pub state: Option<String>,
    pub city: Option<String>,
    pub isp: Option<String>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    #[serde(rename = "lastSeen")]
    pub last_seen: DateTime<Utc>,
    pub active: bool,
}

/// Represents the structure returned by Tendermint RPC net_info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TendermintRpcResponse {
    pub result: Option<TendermintRpcResult>,
    pub peers: Option<Vec<PeerData>>,
    // Some networks might wrap peers in an extra layer
    pub data: Option<TendermintRpcData>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TendermintRpcData {
    pub peers: Option<Vec<PeerData>>,
    pub result: Option<TendermintRpcResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TendermintRpcResult {
    pub peers: Option<Vec<PeerData>>,
    // Added to support different response formats
    pub n_peers: Option<String>,
}

/// Represents a peer as returned from Tendermint RPC
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerData {
    pub remote_ip: Option<String>,
    pub addr: Option<String>,
    pub node_info: Option<NodeInfo>,
    pub is_outbound: Option<bool>,
    pub connection_status: Option<ConnectionStatus>,
    pub rpc_address: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInfo {
    pub id: Option<String>,
    pub listen_addr: Option<String>,
    pub network: Option<String>,
    pub version: Option<String>,
    pub channels: Option<String>,
    pub moniker: Option<String>,
    pub other: Option<NodeInfoOther>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInfoOther {
    pub tx_index: Option<String>,
    pub rpc_address: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionStatus {
    pub duration: Option<String>,
    pub send_monitor: Option<serde_json::Value>,
    pub recv_monitor: Option<serde_json::Value>,
}

/// Interface for Sei network peer data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeiPeerData {
    pub url: String,
    pub node_info: Option<NodeInfo>,
    pub remote_ip: Option<String>,
    pub rpc_address: Option<String>,
}

/// Response from geo IP API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeoApiResponse {
    pub status: String,
    pub country: Option<String>,
    #[serde(rename = "regionName")]
    pub region_name: Option<String>,
    pub region: Option<String>,
    pub city: Option<String>,
    pub isp: Option<String>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
}

/// Statistics about peer nodes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerStats {
    pub total_peers: i64,
    pub active_peers: i64,
    pub networks: Vec<NetworkStats>,
    pub countries: Vec<CountryStats>,
    pub last_update: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct NetworkStats {
    pub network: String,
    pub peer_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct CountryStats {
    pub country: String,
    pub peer_count: i64,
}

/// Interface for Akash network peer data - might have a different format
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AkashPeerData {
    pub address: Option<String>,
    pub id: Option<String>,
    pub remote_ip: Option<String>,
    pub rpc_address: Option<String>,
}

/// Application configuration for caching
#[derive(Debug, Clone)]
pub struct AppCache {
    /// Cache TTL in seconds - how long before cached data expires
    pub cache_ttl: u64,
    /// Refresh interval in seconds - how often to refresh the cache
    pub cache_refresh_interval: u64,
    /// Last database fetch time
    pub last_refresh: Option<DateTime<Utc>>,
}

impl Default for AppCache {
    fn default() -> Self {
        Self {
            cache_ttl: 3600,              // 1 hour default
            cache_refresh_interval: 3600, // 1 hour default
            last_refresh: None,
        }
    }
}
