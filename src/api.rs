use actix_web::{get, web, HttpResponse, Responder};
use serde_json::json;
use std::collections::HashMap;

use crate::discovery::{ALL_DISCOVERED_PEERS, KNOWN_GOOD_PEERS};
use crate::models::PeerGeoInfo;
use crate::{cli::Cli, config::Config, error::AppError, peer_discovery};

// Configure API routes
#[allow(dead_code)]
pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("")
            .service(health_check)
            .service(get_peers)
            .service(get_peers_by_network)
            .service(get_stats)
            .service(trigger_scan)
            .service(get_scan_status)
            .service(configure_cache)
            .service(refresh_cache)
            .service(get_live_peers),
    );
}

/// Health check endpoint
#[get("/health")]
async fn health_check() -> impl Responder {
    HttpResponse::Ok().json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION")
    }))
}

/// Get all peers
#[get("/peers")]
async fn get_peers(db_pool: web::Data<Option<sqlx::PgPool>>) -> impl Responder {
    // Check if we have a database
    if let Some(pool) = db_pool.as_ref() {
        // Query the database
        match crate::db::get_all_peers(pool).await {
            Ok(peers) => HttpResponse::Ok().json(peers),
            Err(e) => {
                tracing::error!("Failed to get peers: {}", e);
                HttpResponse::InternalServerError().json(json!({
                    "error": format!("Failed to get peers: {}", e)
                }))
            }
        }
    } else {
        // First try Redis cache
        match get_peers_from_redis().await {
            Ok(peers) => HttpResponse::Ok().json(peers),
            Err(_) => {
                // If Redis fails, fall back to in-memory cache
                let in_memory_peers = get_peers_from_memory_cache();
                if in_memory_peers.is_empty() {
                    HttpResponse::InternalServerError().json(json!({
                        "error": "No peers found. Memory cache is empty."
                    }))
                } else {
                    HttpResponse::Ok().json(in_memory_peers)
                }
            }
        }
    }
}

/// Get peers by network
#[get("/peers/network/{network}")]
async fn get_peers_by_network(
    network: web::Path<String>,
    query: web::Query<HashMap<String, String>>,
    db_pool: web::Data<Option<sqlx::PgPool>>,
) -> impl Responder {
    let network = network.into_inner();
    // Parse active filter from query parameters if provided (format: ?active=true or ?active=false)
    let active_filter = query.get("active").and_then(|v| v.parse::<bool>().ok());

    // Check cache first
    let cache = crate::cache::DB_CACHE.lock().await;
    if let Some(peers) = cache.get_peers_for_network(&network) {
        // We found cached data, use it
        tracing::debug!("Using cached peer data for network {}", network);

        // Apply active filter if needed
        let filtered_peers = if let Some(active) = active_filter {
            peers
                .into_iter()
                .filter(|p| p.active == active)
                .collect::<Vec<_>>()
        } else {
            peers
        };

        // Return the data directly without pagination wrapper
        return HttpResponse::Ok().json(filtered_peers);
    }
    drop(cache); // Release lock before async operations

    // Check if we have a database
    if let Some(pool) = db_pool.as_ref() {
        // Query the database for all peers for this network
        match crate::db::get_peers_by_network_with_filter(pool, &network, active_filter).await {
            Ok(peers) => {
                // Store in cache for future requests
                let mut cache = crate::cache::DB_CACHE.lock().await;
                // Only store unfiltered data in the cache
                if active_filter.is_none() {
                    cache.update_peers_for_network(&network, peers.clone());
                }
                drop(cache);

                // Return directly without pagination
                HttpResponse::Ok().json(peers)
            }
            Err(e) => {
                tracing::error!("Failed to get peers for network {}: {}", network, e);
                HttpResponse::InternalServerError().json(json!({
                    "error": format!("Failed to get peers for network {}: {}", network, e)
                }))
            }
        }
    } else {
        // First try Redis
        match get_peers_by_network_from_redis(&network).await {
            Ok(mut peers) => {
                // Apply filter if provided when using Redis
                if let Some(active) = active_filter {
                    peers = peers
                        .into_iter()
                        .filter(|p| p.active == active)
                        .collect::<Vec<_>>();
                }
                HttpResponse::Ok().json(peers)
            }
            Err(_) => {
                // If Redis fails, fall back to in-memory cache
                let mut in_memory_peers = get_peers_by_network_from_memory(&network);

                // Apply filter if provided when using in-memory cache
                if let Some(active) = active_filter {
                    in_memory_peers = in_memory_peers
                        .into_iter()
                        .filter(|p| p.active == active)
                        .collect::<Vec<_>>();
                }

                if in_memory_peers.is_empty() {
                    HttpResponse::InternalServerError().json(json!({
                        "error": format!("No peers found for network {}. Memory cache is empty.", network)
                    }))
                } else {
                    HttpResponse::Ok().json(in_memory_peers)
                }
            }
        }
    }
}

/// Get peer statistics
#[get("/stats")]
async fn get_stats(db_pool: web::Data<Option<sqlx::PgPool>>) -> impl Responder {
    // Check cache first
    let cache = crate::cache::DB_CACHE.lock().await;
    if let Some(stats) = cache.get_stats() {
        tracing::debug!("Using cached stats data");
        return HttpResponse::Ok().json(stats);
    }
    drop(cache); // Release lock before async operations

    // Check if we have a database
    if let Some(pool) = db_pool.as_ref() {
        // Query the database for statistics
        match crate::db::get_peer_stats(pool).await {
            Ok(stats) => {
                // Store in cache for future requests
                let mut cache = crate::cache::DB_CACHE.lock().await;
                cache.update_stats(stats.clone());
                drop(cache);

                HttpResponse::Ok().json(stats)
            }
            Err(e) => {
                tracing::error!("Failed to get peer statistics: {}", e);
                HttpResponse::InternalServerError().json(json!({
                    "error": format!("Failed to get peer statistics: {}", e)
                }))
            }
        }
    } else {
        // First try Redis
        match get_stats_from_redis().await {
            Ok(stats) => HttpResponse::Ok().json(stats),
            Err(_) => {
                // If Redis fails, fall back to in-memory cache stats
                let memory_stats = get_stats_from_memory();
                HttpResponse::Ok().json(memory_stats)
            }
        }
    }
}

/// Trigger a scan for a specific network
#[get("/peers/scan/{network}")]
async fn trigger_scan(
    network: web::Path<String>,
    db_pool: web::Data<Option<sqlx::PgPool>>,
    cli: web::Data<Cli>,
) -> impl Responder {
    let network = network.into_inner();

    // Load configuration
    let config = match Config::from_env() {
        Ok(config) => config,
        Err(e) => {
            tracing::error!("Failed to load configuration: {}", e);
            return HttpResponse::InternalServerError().json(json!({
                "error": format!("Failed to load configuration: {}", e)
            }));
        }
    };

    // Check if this network exists in the configuration
    if !config.network_names().contains(&network) {
        return HttpResponse::BadRequest().json(json!({
            "error": format!("Network '{}' not found in configuration", network)
        }));
    }

    // Start the scan in a separate task
    if let Some(pool) = db_pool.as_ref() {
        let pool_clone = pool.clone();
        let network_clone = network.clone();
        let config_clone = config.clone();

        // Run with database support
        tokio::spawn(async move {
            if let Err(e) = peer_discovery::trigger_network_scan(
                pool_clone,
                network_clone.clone(),
                config_clone,
            )
            .await
            {
                tracing::error!("Failed to scan network {}: {}", network_clone, e);
            }
        });
    } else {
        // Run in memory-only mode
        let cli_clone = cli.get_ref().clone();
        let network_clone = network.clone();

        tokio::task::spawn_blocking(move || {
            let rt = tokio::runtime::Runtime::new().expect("Failed to create runtime");
            rt.block_on(async {
                // Create a CLI with just this network
                let mut network_cli = cli_clone;
                network_cli.network = Some(network_clone.clone());

                if let Err(e) = peer_discovery::run_memory_only_discovery(network_cli, None).await {
                    tracing::error!(
                        "Failed to scan network {} in memory-only mode: {}",
                        network_clone,
                        e
                    );
                }
            });
        });
    }

    HttpResponse::Ok().json(json!({
        "status": "ok",
        "message": format!("Scan for network '{}' started", network)
    }))
}

/// Get scan status and memory cache info
#[get("/peers/scan/status")]
async fn get_scan_status(cli: web::Data<Cli>) -> impl Responder {
    // Get cache status
    let all_peers_guard = ALL_DISCOVERED_PEERS.lock();
    let good_peers_guard = KNOWN_GOOD_PEERS.lock();

    // Build a response with information about what's in the cache
    let mut networks = Vec::new();
    let mut total_peers = 0;
    let mut total_good_peers = 0;

    for (network, peers_set) in all_peers_guard.iter() {
        let peer_count = peers_set.len();
        total_peers += peer_count;

        // Count good peers (with RPC address) for reference
        let good_peer_count = good_peers_guard
            .get(network)
            .map(|peers| peers.len())
            .unwrap_or(0);
        total_good_peers += good_peer_count;

        let network_info = json!({
            "network": network,
            "peer_count": peer_count,
            "good_peer_count": good_peer_count,
            "peer_sample": peers_set.iter().take(5).map(|p| json!({
                "ip": p.ip,
                "rpc_address": p.rpc_address
            })).collect::<Vec<_>>()
        });

        networks.push(network_info);
    }

    HttpResponse::Ok().json(json!({
        "cache_status": {
            "total_networks": networks.len(),
            "total_peers": total_peers,
            "total_good_peers": total_good_peers,
            "networks": networks
        },
        "cli_settings": {
            "network": cli.network,
            "max_peers": cli.max_peers,
            "max_depth": cli.max_depth,
            "scan_interval": cli.scan_interval,
            "continuous": cli.continuous
        }
    }))
}

/// Configure the database cache
#[get("/cache/configure")]
async fn configure_cache(query: web::Query<HashMap<String, String>>) -> impl Responder {
    // Parse cache TTL and refresh interval from query parameters
    let ttl = query.get("ttl").and_then(|v| v.parse::<u64>().ok());
    let refresh_interval = query
        .get("refresh_interval")
        .and_then(|v| v.parse::<u64>().ok());

    // Update cache configuration
    match crate::cache::configure_cache(ttl, refresh_interval).await {
        Ok(_) => {
            let cache = crate::cache::DB_CACHE.lock().await;
            HttpResponse::Ok().json(json!({
                "status": "ok",
                "message": "Cache configuration updated",
                "cache_ttl": cache.config.cache_ttl,
                "cache_refresh_interval": cache.config.cache_refresh_interval
            }))
        }
        Err(e) => HttpResponse::InternalServerError().json(json!({
            "status": "error",
            "message": format!("Failed to update cache configuration: {}", e)
        })),
    }
}

/// Force refresh the cache
#[get("/cache/refresh")]
async fn refresh_cache(
    db_pool: web::Data<Option<sqlx::PgPool>>,
    query: web::Query<HashMap<String, String>>,
) -> impl Responder {
    // Check if we have a database
    if let Some(pool) = db_pool.as_ref() {
        // Optionally refresh specific network
        let network = query.get("network").cloned();

        if let Some(network) = network {
            // Handle Omniflix case
            if network.to_lowercase() == "omniflix" {
                match crate::db::get_peers_by_network_with_filter(pool, "omniflix", None).await {
                    Ok(peers) => {
                        // Update cache for network
                        let mut cache = crate::cache::DB_CACHE.lock().await;
                        cache.update_peers_for_network("omniflix", peers.clone());
                        drop(cache);

                        HttpResponse::Ok().json(json!({
                            "status": "ok",
                            "message": format!("Cache refreshed for network omniflix with {} peers", peers.len())
                        }))
                    }
                    Err(e) => HttpResponse::InternalServerError().json(json!({
                        "status": "error",
                        "message": format!("Failed to refresh cache for network omniflix: {}", e)
                    })),
                }
            } else {
                // Normal case for other networks
                match crate::db::get_peers_by_network(pool, &network).await {
                    Ok(peers) => {
                        // Update cache
                        let mut cache = crate::cache::DB_CACHE.lock().await;
                        cache.update_peers_for_network(&network, peers.clone());
                        drop(cache);

                        HttpResponse::Ok().json(json!({
                            "status": "ok",
                            "message": format!("Cache refreshed for network {} with {} peers", network, peers.len())
                        }))
                    }
                    Err(e) => HttpResponse::InternalServerError().json(json!({
                        "status": "error",
                        "message": format!("Failed to refresh cache for network {}: {}", network, e)
                    })),
                }
            }
        } else {
            // Refresh all
            // 1. Refresh stats
            let stats_result = crate::db::get_peer_stats(pool).await;

            // 2. Get a list of all networks
            let config = match crate::config::Config::from_env() {
                Ok(config) => config,
                Err(e) => {
                    return HttpResponse::InternalServerError().json(json!({
                        "status": "error",
                        "message": format!("Failed to load configuration: {}", e)
                    }));
                }
            };

            let networks = config.network_names();
            let mut success_count = 0;
            let mut error_count = 0;

            // Update cache with stats if successful
            if let Ok(stats) = stats_result {
                let mut cache = crate::cache::DB_CACHE.lock().await;
                cache.update_stats(stats);
                drop(cache);
                success_count += 1;
            } else {
                error_count += 1;
            }

            // Refresh each network
            for network in networks {
                match crate::db::get_peers_by_network(pool, &network).await {
                    Ok(peers) => {
                        let mut cache = crate::cache::DB_CACHE.lock().await;
                        cache.update_peers_for_network(&network, peers);
                        drop(cache);
                        success_count += 1;
                    }
                    Err(_) => {
                        error_count += 1;
                    }
                }
            }

            HttpResponse::Ok().json(json!({
                "status": "ok",
                "message": format!("Cache refresh complete: {} successful, {} failed", success_count, error_count)
            }))
        }
    } else {
        HttpResponse::BadRequest().json(json!({
            "status": "error",
            "message": "Cannot refresh cache without database connection"
        }))
    }
}

#[get("/live_peers/{network}")]
async fn get_live_peers(
    network: web::Path<String>,
    db_pool: web::Data<Option<sqlx::PgPool>>,
) -> impl Responder {
    let network = network.into_inner();

    // Function to format connection string
    fn format_conn(peer: &PeerGeoInfo) -> Option<String> {
        if peer.is_live == Some(true) {
            if let (Some(id), Some(port)) = (&peer.node_id, peer.p2p_port) {
                Some(format!("{}@{}:{}", id, peer.ip, port))
            } else {
                None
            }
        } else {
            None
        }
    }

    if let Some(pool) = db_pool.as_ref() {
        match crate::db::get_peers_by_network(pool, &network).await {
            Ok(peers) => {
                let live_conns: Vec<String> = peers
                    .iter()
                    .filter_map(|p| {
                        // Convert PeerNode to PeerGeoInfo-like for formatting
                        format_conn(&PeerGeoInfo {
                            ip: p.ip.clone(),
                            node_id: p.node_id.clone(),
                            p2p_port: p.p2p_port.map(|p| p as u16),
                            is_live: p.is_live,
                            rpc_address: None,
                            network: String::new(),
                            country: None,
                            region: None,
                            province: None,
                            state: None,
                            city: None,
                            isp: None,
                            lat: None,
                            lon: None,
                            last_seen: None,
                            active: false,
                        })
                    })
                    .collect();
                HttpResponse::Ok().json(live_conns)
            }
            Err(e) => HttpResponse::InternalServerError()
                .json(json!({"error": format!("Failed to get live peers: {}", e)})),
        }
    } else {
        match get_peers_by_network_from_redis(&network).await {
            Ok(peers) => {
                let live_conns: Vec<String> = peers.iter().filter_map(format_conn).collect();
                HttpResponse::Ok().json(live_conns)
            }
            Err(_) => {
                let memory_peers = get_peers_by_network_from_memory(&network);
                let live_conns: Vec<String> = memory_peers.iter().filter_map(format_conn).collect();
                HttpResponse::Ok().json(live_conns)
            }
        }
    }
}

// Helper functions for Redis-based data retrieval
async fn get_peers_from_redis() -> Result<Vec<PeerGeoInfo>, AppError> {
    let cache_lock = crate::redis_cache::REDIS_CACHE.lock().await;
    let redis_cache = match cache_lock.as_ref() {
        Some(cache) => cache,
        None => {
            return Err(AppError::CacheError(
                "Redis cache not initialized".to_string(),
            ))
        }
    };

    // Get all networks
    let config = Config::from_env()?;
    let networks = config.network_names();

    // Collect peers from all networks
    let mut all_peers = Vec::new();
    for network in networks {
        let peers = redis_cache.get_peers_by_network(&network).await?;
        all_peers.extend(peers);
    }

    Ok(all_peers)
}

async fn get_peers_by_network_from_redis(network: &str) -> Result<Vec<PeerGeoInfo>, AppError> {
    let cache_lock = crate::redis_cache::REDIS_CACHE.lock().await;
    let redis_cache = match cache_lock.as_ref() {
        Some(cache) => cache,
        None => {
            return Err(AppError::CacheError(
                "Redis cache not initialized".to_string(),
            ))
        }
    };

    redis_cache.get_peers_by_network(network).await
}

async fn get_stats_from_redis() -> Result<serde_json::Value, AppError> {
    let all_peers = get_peers_from_redis().await?;

    // Count peers by network
    let mut peers_by_network: HashMap<String, usize> = HashMap::new();
    for peer in &all_peers {
        *peers_by_network.entry(peer.network.clone()).or_insert(0) += 1;
    }

    // Count peers by country
    let mut peers_by_country: HashMap<String, usize> = HashMap::new();
    for peer in &all_peers {
        if let Some(country) = &peer.country {
            *peers_by_country.entry(country.clone()).or_insert(0) += 1;
        }
    }

    // Create stats response
    let stats = json!({
        "total_peers": all_peers.len(),
        "networks": peers_by_network.len(),
        "countries": peers_by_country.len(),
        "peers_by_network": peers_by_network,
        "peers_by_country": peers_by_country,
    });

    Ok(stats)
}

/// Get peers from in-memory cache
fn get_peers_from_memory_cache() -> Vec<PeerGeoInfo> {
    let mut result = Vec::new();

    // Get all network names from the in-memory cache
    let peers_guard = ALL_DISCOVERED_PEERS.lock();
    for (network, peers_set) in peers_guard.iter() {
        // Convert PeerInfo to PeerGeoInfo
        for peer in peers_set.iter() {
            result.push(PeerGeoInfo {
                ip: peer.ip.clone(),
                rpc_address: peer.rpc_address.clone(),
                network: network.clone(),
                country: None,
                region: None,
                province: None,
                state: None,
                city: None,
                isp: None,
                lat: None,
                lon: None,
                last_seen: None,
                active: true,
                is_live: peer.is_live,
                node_id: peer.node_id.clone(),
                p2p_port: peer.p2p_port,
            });
        }
    }

    result
}

/// Get peers by network from in-memory cache
fn get_peers_by_network_from_memory(network: &str) -> Vec<PeerGeoInfo> {
    let peers_guard = ALL_DISCOVERED_PEERS.lock();
    let peers_set = peers_guard.get(network);

    if let Some(peers_set) = peers_set {
        // Convert to PeerGeoInfo
        return peers_set
            .iter()
            .map(|p| PeerGeoInfo {
                ip: p.ip.clone(),
                rpc_address: p.rpc_address.clone(),
                network: network.to_string(),
                country: None,
                region: None,
                province: None,
                state: None,
                city: None,
                isp: None,
                lat: None,
                lon: None,
                last_seen: None,
                active: true,
                is_live: p.is_live,
                node_id: p.node_id.clone(),
                p2p_port: p.p2p_port,
            })
            .collect();
    }

    Vec::new()
}

/// Get peer statistics from in-memory cache
fn get_stats_from_memory() -> serde_json::Value {
    use chrono::Utc;

    // Get all networks from the in-memory cache
    let peers_guard = ALL_DISCOVERED_PEERS.lock();

    // Count peers by network
    let mut network_stats = Vec::new();
    let mut total_peers = 0;

    for (network, peers_set) in peers_guard.iter() {
        let peer_count = peers_set.len() as i64;
        total_peers += peer_count;

        network_stats.push(json!({
            "network": network,
            "peer_count": peer_count
        }));
    }

    // Return the stats in the same format as the other endpoints
    json!({
        "total_peers": total_peers,
        "active_peers": total_peers, // All memory peers are considered active
        "networks": network_stats,
        "countries": [], // No country data available in memory
        "last_update": Utc::now()
    })
}
