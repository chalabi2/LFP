use super::{
    cache, geo::fetch_geo_info_batch, peer_parser::fetch_peer_info,
    peer_traversal::follow_rpc_addresses, utils::is_valid_public_ip,
};
use crate::{
    cli::Cli,
    db,
    error::AppError,
    models::{PeerGeoInfo, PeerInfo},
};
use parking_lot::Mutex;

use reqwest::Client;
use sqlx::{Pool, Postgres};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

/// Scan a network for peers from a given endpoint
pub async fn scan_network(
    db_pool: &Pool<Postgres>,
    network: &str,
    url: &str,
    client: &Client,
    cli: &Cli,
) -> Result<(), AppError> {
    tracing::info!(
        "Starting peer discovery for network {} from {}",
        network,
        url
    );

    // Fetch initial peers from the provided RPC URL
    let initial_peers = fetch_peer_info(client, url, Some(network)).await?;

    if initial_peers.is_empty() {
        tracing::warn!("No initial peers found for {}", network);
        // Signal other components that this endpoint is not working
        cache::mark_endpoint_failed(network, url);
        return Ok(());
    }

    tracing::info!(
        "Found {} initial peers for {}",
        initial_peers.len(),
        network
    );

    // Store all initial peers in the all discovered peers cache
    cache::add_all_discovered_peers(network, initial_peers.clone());

    // Update the in-memory cache immediately with initial peers
    // This ensures the API has something to show right away
    let good_initial_peers: Vec<_> = initial_peers
        .iter()
        .filter(|p| p.rpc_address.is_some() && is_valid_public_ip(&p.ip))
        .cloned()
        .collect();

    if !good_initial_peers.is_empty() {
        tracing::info!(
            "Updating cache with {} initial peers for {}",
            good_initial_peers.len(),
            network
        );
        cache::update_cached_peers(network, good_initial_peers, 100);
    }

    // Follow RPC addresses to discover more peers
    let visited = Arc::new(Mutex::new(HashSet::new()));
    visited.lock().insert(url.to_string()); // Mark initial URL as visited

    tracing::info!(
        "Starting recursive peer discovery for {} with max_depth={}, max_peers={}, concurrency={}",
        network,
        cli.max_depth,
        cli.max_peers,
        cli.max_concurrent_requests
    );

    let all_peers = follow_rpc_addresses(
        client,
        &initial_peers,
        network,
        visited,
        cli.max_depth,
        cli.max_peers,
        cli.max_concurrent_requests,
    )
    .await?;

    // Log detailed peer information
    for (i, peer) in all_peers.iter().enumerate().take(10) {
        tracing::info!(
            "[{}] Peer {}/{}: IP={}, RPC={}",
            network,
            i + 1,
            all_peers.len(),
            peer.ip,
            peer.rpc_address.as_deref().unwrap_or("none")
        );
    }

    if all_peers.len() > 10 {
        tracing::info!("... and {} more peers", all_peers.len() - 10);
    }

    tracing::info!(
        "Discovered a total of {} peers for {}",
        all_peers.len(),
        network
    );

    // Store all discovered peers in the all discovered peers cache
    cache::add_all_discovered_peers(network, all_peers.clone());

    // Store good peers in the cache for future use
    let good_peers: Vec<PeerInfo> = all_peers
        .iter()
        .filter(|p| p.rpc_address.is_some() && is_valid_public_ip(&p.ip))
        .cloned()
        .collect();

    // Log how many good peers we found
    tracing::info!(
        "Storing {} good peers with RPC addresses in cache for {}",
        good_peers.len(),
        network
    );

    // Only store up to 50 peers to keep the cache manageable
    let max_peers_to_cache = 50;
    cache::update_cached_peers(network, good_peers, max_peers_to_cache);

    // Get geographical information for all discovered peers
    let peers_with_geo = fetch_geo_info_batch(client, &all_peers, network).await?;

    tracing::info!(
        "Added geographical information to {} peers",
        peers_with_geo.len()
    );

    // Get Redis cache client
    let cache_lock = crate::redis_cache::REDIS_CACHE.lock().await;
    if let Some(redis_cache) = cache_lock.as_ref() {
        // First, delete existing peers for this network to avoid stale data
        // This ensures we only store current, active peers
        if let Err(e) = redis_cache.delete_peers_by_network(network).await {
            tracing::warn!("Failed to clear Redis cache for network {}: {}", network, e);
        }

        // Store peers in Redis for permanent storage with deduplication
        let mut unique_peers = HashMap::new();
        for peer in peers_with_geo.iter() {
            unique_peers.insert(peer.ip.clone(), peer.clone());
        }

        let unique_peer_vec: Vec<PeerGeoInfo> = unique_peers.into_values().collect();

        // Store in Redis with appropriate TTL for "permanent" storage
        if let Err(e) = redis_cache
            .store_peers_batch(network, &unique_peer_vec)
            .await
        {
            tracing::warn!(
                "Failed to store peers in Redis cache for network {}: {}",
                network,
                e
            );
        } else {
            tracing::info!(
                "Stored {} unique peers in Redis cache for network {}",
                unique_peer_vec.len(),
                network
            );
        }

        // Also update the database with the discovered peers
        db::upsert_peers_batch(db_pool, &unique_peer_vec, network).await?;

        tracing::info!("Updated database with peers for network {}", network);
    } else {
        tracing::warn!("Redis cache not initialized, skipping Redis updates");
        // Still update the database
        db::upsert_peers_batch(db_pool, &peers_with_geo, network).await?;
        tracing::info!("Updated database with peers for network {}", network);
    }

    Ok(())
}
