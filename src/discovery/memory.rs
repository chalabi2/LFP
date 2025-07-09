use crate::{
    cli::Cli,
    config::Config,
    error::AppError,
    models::{PeerGeoInfo, PeerInfo},
};
use parking_lot::Mutex;
use rand::seq::SliceRandom;
use rand::Rng;
use reqwest::Client;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::{broadcast, Semaphore},
    time,
};

use super::{
    cache,
    geo::fetch_geo_info_batch,
    peer_parser::fetch_peer_info,
    peer_traversal::{follow_rpc_addresses, resolve_rpc_url},
    utils::{create_http_client, has_problematic_pattern, is_valid_public_ip},
};

/// Run peer discovery in memory-only mode (no database storage)
pub async fn run_memory_only_discovery(
    cli: Cli,
    mut shutdown_rx: Option<broadcast::Receiver<()>>,
) -> Result<(), AppError> {
    // Load configuration
    let config = Config::from_env()?;

    // Limit default concurrency to prevent overloading
    let concurrent_chains = if cli.concurrent_chains > 0 {
        std::cmp::min(cli.concurrent_chains, 5) // Cap at 5 parallel chains max
    } else {
        3 // Default to 3 if not specified
    };

    // Create a shared HTTP client with appropriate settings for peer discovery
    let client = create_http_client(cli.request_timeout())?;

    // If a specific network is requested, only scan that one
    if let Some(network) = &cli.network {
        // Check for shutdown signal
        if let Some(ref mut rx) = shutdown_rx {
            match rx.try_recv() {
                Ok(_) => {
                    tracing::info!("Received shutdown signal, stopping memory-only discovery");
                    return Ok(());
                }
                Err(broadcast::error::TryRecvError::Empty) => {
                    // No shutdown signal yet, continue
                }
                Err(broadcast::error::TryRecvError::Closed) => {
                    tracing::info!("Shutdown channel closed, stopping memory-only discovery");
                    return Ok(());
                }
                Err(broadcast::error::TryRecvError::Lagged(_)) => {
                    tracing::info!("Shutdown signal lagged, stopping memory-only discovery");
                    return Ok(());
                }
            }
        }

        tracing::info!("Running memory-only scan for network: {}", network);

        // Try to use cached peers first
        let use_peer_cache = !cache::get_cached_peers(network).is_empty();

        if use_peer_cache {
            tracing::info!("Using cached peers for memory-only scan of {}", network);
            match memory_scan_from_cached_peers(network, &client, &cli).await {
                Ok(_) => tracing::info!("Successfully scanned network {}", network),
                Err(e) => {
                    tracing::error!(
                        "Error scanning network {} with cached peers: {}",
                        network,
                        e
                    );
                    // Fall back to configured URLs
                    if let Some(rpc_urls) = config.get_rpc_urls(network) {
                        tracing::info!("Falling back to configured endpoints for {}", network);
                        memory_scan_network(network, rpc_urls, &client, &cli, &config).await?;
                    } else {
                        tracing::warn!("Network {} not found in configuration", network);
                    }
                }
            }
        } else if let Some(rpc_urls) = config.get_rpc_urls(network) {
            memory_scan_network(network, rpc_urls, &client, &cli, &config).await?;
        } else {
            tracing::warn!("Network {} not found in configuration", network);
        }
    } else {
        // Scan all networks in parallel with concurrency limit
        let networks = config.network_names();
        tracing::info!(
            "Starting memory-only scan for {} networks with concurrency of {}",
            networks.len(),
            concurrent_chains
        );

        // Use a semaphore to limit concurrent network scans
        let semaphore = Arc::new(Semaphore::new(concurrent_chains));

        // Create futures for each network
        let scan_futures = networks
            .iter()
            .map(|network| {
                // Try to use cached peers first
                let use_peer_cache = !cache::get_cached_peers(network).is_empty();

                let client = client.clone();
                let cli = cli.clone();
                let network = network.clone();
                let config = config.clone();
                let semaphore = semaphore.clone();

                async move {
                    // Acquire permit from semaphore (this limits concurrency)
                    let _permit = semaphore.acquire().await.unwrap();

                    if use_peer_cache {
                        tracing::info!(
                            "Starting memory-only scan for network {} using cached peers",
                            network
                        );
                        match memory_scan_from_cached_peers(&network, &client, &cli).await {
                            Ok(_) => tracing::info!("Successfully scanned network {}", network),
                            Err(e) => {
                                tracing::error!(
                                    "Error scanning network {} with cached peers: {}",
                                    network,
                                    e
                                );
                                // Fall back to configured URLs
                                if let Some(rpc_urls) = config.get_rpc_urls(&network) {
                                    tracing::info!(
                                        "Falling back to configured endpoints for {}",
                                        network
                                    );
                                    if let Err(e) = memory_scan_network(
                                        &network, rpc_urls, &client, &cli, &config,
                                    )
                                    .await
                                    {
                                        tracing::error!(
                                            "Error in fallback scan for {}: {}",
                                            network,
                                            e
                                        );
                                    }
                                }
                            }
                        }
                    } else if let Some(rpc_urls) = config.get_rpc_urls(&network) {
                        tracing::info!("Starting memory-only scan for network {}", network);
                        match memory_scan_network(&network, rpc_urls, &client, &cli, &config).await
                        {
                            Ok(_) => tracing::info!("Successfully scanned network {}", network),
                            Err(e) => {
                                tracing::error!("Error scanning network {}: {}", network, e)
                            }
                        }
                    }
                }
            })
            .collect::<Vec<_>>();

        // Execute all network scans with controlled concurrency
        futures::future::join_all(scan_futures).await;
    }

    // If continuous scanning is enabled, start the interval-based scanning
    if cli.continuous {
        tracing::info!(
            "Starting continuous memory-only scanning with interval of {} seconds",
            cli.scan_interval
        );

        let networks = if let Some(network) = &cli.network {
            // Only scan the specified network
            vec![network.clone()]
        } else {
            // Scan all networks
            config.network_names()
        };

        // Start the continuous scanning loop
        let mut interval = time::interval(cli.scan_interval());
        loop {
            // Check for shutdown signal
            if let Some(ref mut rx) = shutdown_rx {
                match rx.try_recv() {
                    Ok(_) => {
                        tracing::info!(
                            "Received shutdown signal, stopping continuous memory-only discovery"
                        );
                        break;
                    }
                    Err(broadcast::error::TryRecvError::Empty) => {
                        // No shutdown signal yet, continue
                    }
                    Err(broadcast::error::TryRecvError::Closed) => {
                        tracing::info!(
                            "Shutdown channel closed, stopping continuous memory-only discovery"
                        );
                        break;
                    }
                    Err(broadcast::error::TryRecvError::Lagged(_)) => {
                        tracing::info!(
                            "Shutdown signal lagged, stopping continuous memory-only discovery"
                        );
                        break;
                    }
                }
            }

            interval.tick().await;
            tracing::info!("Starting scheduled memory-only scan for all networks");

            // Use a semaphore to limit concurrent network scans
            let semaphore = Arc::new(Semaphore::new(concurrent_chains));

            // Create a future for each network scan
            let scan_futures = networks
                .iter()
                .map(|network| {
                    // Try to use cached peers first
                    let use_peer_cache = !cache::get_cached_peers(network).is_empty();

                    let client = client.clone();
                    let cli = cli.clone();
                    let network = network.clone();
                    let config = config.clone();
                    let semaphore = semaphore.clone();

                    async move {
                        // Acquire permit from semaphore (this limits concurrency)
                        let _permit = semaphore.acquire().await.unwrap();

                        // Add small random delay to stagger requests
                        let delay = rand::thread_rng().gen_range(0..1000);
                        tokio::time::sleep(Duration::from_millis(delay)).await;

                        if use_peer_cache {
                            tracing::info!(
                                "Starting memory-only scan for network {} using cached peers",
                                network
                            );
                            match memory_scan_from_cached_peers(&network, &client, &cli).await {
                                Ok(_) => tracing::info!("Successfully scanned network {}", network),
                                Err(e) => {
                                    tracing::error!(
                                        "Error scanning network {} with cached peers: {}",
                                        network,
                                        e
                                    );
                                    // Fall back to configured URLs
                                    if let Some(rpc_urls) = config.get_rpc_urls(&network) {
                                        tracing::info!(
                                            "Falling back to configured endpoints for {}",
                                            network
                                        );
                                        if let Err(e) = memory_scan_network(
                                            &network, rpc_urls, &client, &cli, &config,
                                        )
                                        .await
                                        {
                                            tracing::error!(
                                                "Error in fallback scan for {}: {}",
                                                network,
                                                e
                                            );
                                        }
                                    }
                                }
                            }
                        } else if let Some(rpc_urls) = config.get_rpc_urls(&network) {
                            tracing::info!("Starting memory-only scan for network {}", network);
                            match memory_scan_network(&network, rpc_urls, &client, &cli, &config)
                                .await
                            {
                                Ok(_) => tracing::info!("Successfully scanned network {}", network),
                                Err(e) => {
                                    tracing::error!("Error scanning network {}: {}", network, e)
                                }
                            }
                        }
                    }
                })
                .collect::<Vec<_>>();

            // Execute all network scans with controlled concurrency
            futures::future::join_all(scan_futures).await;

            // Check for shutdown signal after completing a scan cycle
            if let Some(ref mut rx) = shutdown_rx {
                match rx.try_recv() {
                    Ok(_) => {
                        tracing::info!(
                            "Received shutdown signal, stopping continuous memory-only discovery"
                        );
                        break;
                    }
                    Err(broadcast::error::TryRecvError::Empty) => {
                        // No shutdown signal yet, continue
                    }
                    Err(broadcast::error::TryRecvError::Closed) => {
                        tracing::info!(
                            "Shutdown channel closed, stopping continuous memory-only discovery"
                        );
                        break;
                    }
                    Err(broadcast::error::TryRecvError::Lagged(_)) => {
                        tracing::info!(
                            "Shutdown signal lagged, stopping continuous memory-only discovery"
                        );
                        break;
                    }
                }
            }
        }
    }

    Ok(())
}

/// Memory-only scan from a single cached peer
async fn memory_scan_from_cached_peers(
    network: &str,
    client: &Client,
    cli: &Cli,
) -> Result<(), AppError> {
    // Get cached peers for this network
    let cached_peers = cache::get_cached_peers(network);

    if cached_peers.is_empty() {
        return Err(AppError::NotFoundError(format!(
            "No cached peers available for network {}",
            network
        )));
    }

    // Shuffle the peers to avoid hitting the same one repeatedly in multiple runs
    let shuffled_peers = {
        let mut rng = rand::thread_rng();
        let mut shuffled_peers = cached_peers.clone();
        shuffled_peers.shuffle(&mut rng);
        shuffled_peers
    };

    // Try just the first peer
    if let Some(peer) = shuffled_peers.first() {
        if let Some(rpc_url) = resolve_rpc_url(peer) {
            tracing::info!("Trying single cached peer for {}: {}", network, rpc_url);

            // Try this peer with a reasonable timeout
            let timeout = Duration::from_secs(10);
            match tokio::time::timeout(
                timeout,
                memory_scan_endpoint(network, &rpc_url, client, cli),
            )
            .await
            {
                Ok(Ok(_)) => {
                    tracing::info!("Successfully scanned network {} using cached peer", network);
                    return Ok(());
                }
                Ok(Err(e)) => {
                    let error_msg = format!("Cached peer {} failed: {}", rpc_url, e);
                    tracing::debug!("{}", error_msg);
                    return Err(AppError::RequestError(error_msg));
                }
                Err(_) => {
                    let error_msg = format!("Cached peer {} timed out", rpc_url);
                    tracing::debug!("{}", error_msg);
                    return Err(AppError::RequestError(error_msg));
                }
            }
        }
    }

    // No usable peer was found
    Err(AppError::RequestError(format!(
        "No usable cached peer for network {}",
        network
    )))
}

/// Scan a network with one attempt per endpoint
async fn memory_scan_network(
    network: &str,
    rpc_urls: (String, String),
    client: &Client,
    cli: &Cli,
    config: &Config,
) -> Result<(), AppError> {
    let (primary_url, fallback_url) = rpc_urls;

    // Check for problematic patterns in primary URL before attempting
    let should_skip_primary = has_problematic_pattern(&primary_url);

    if should_skip_primary {
        tracing::info!(
            "Skipping known problematic primary endpoint: {}",
            primary_url
        );
        // Try fallback URL without retries
        return scan_single_endpoint(network, &fallback_url, client, cli, config).await;
    }

    // Try primary URL first
    let primary_result = scan_single_endpoint(network, &primary_url, client, cli, config).await;

    if primary_result.is_ok() {
        return primary_result;
    }

    // Only try fallback if primary completely failed
    tracing::info!(
        "Primary endpoint failed for {}, trying fallback: {}",
        network,
        fallback_url
    );

    scan_single_endpoint(network, &fallback_url, client, cli, config).await
}

/// Try a single endpoint with no retries
async fn scan_single_endpoint(
    network: &str,
    url: &str,
    client: &Client,
    cli: &Cli,
    config: &Config,
) -> Result<(), AppError> {
    // Get network-specific timeout
    let network_timeout = config.get_network_timeout(network);

    tracing::info!("Attempting to connect to endpoint for {}: {}", network, url);

    // Single attempt with timeout
    match tokio::time::timeout(
        network_timeout,
        memory_scan_endpoint(network, url, client, cli),
    )
    .await
    {
        // Success
        Ok(Ok(_)) => {
            tracing::info!("Successfully scanned network {} using endpoint", network);
            Ok(())
        }
        // Failed with error
        Ok(Err(e)) => {
            tracing::warn!("Endpoint for {} failed with error: {}", network, e);
            Err(e)
        }
        // Timed out
        Err(_) => {
            tracing::warn!(
                "Endpoint for {} timed out after {:?}",
                network,
                network_timeout
            );
            Err(AppError::RequestError(format!(
                "Endpoint {} timed out for network {}",
                url, network
            )))
        }
    }
}

/// Memory-only scan endpoint without database storage
async fn memory_scan_endpoint(
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
        return Ok(());
    }

    tracing::info!(
        "Found {} initial peers for {}",
        initial_peers.len(),
        network
    );

    // Store all initial peers in the all discovered peers cache
    cache::add_all_discovered_peers(network, initial_peers.clone());

    // Follow RPC addresses to discover more peers
    let visited = Arc::new(Mutex::new(HashSet::new()));
    visited.lock().insert(url.to_string()); // Mark initial URL as visited

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

    // Store in Redis if available, regardless of database availability
    let cache_lock = crate::redis_cache::REDIS_CACHE.lock().await;
    if let Some(redis_cache) = cache_lock.as_ref() {
        // Clear existing peers for this network
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
    } else {
        // Log summary of collected peers since we can't store them
        let countries = peers_with_geo
            .iter()
            .filter_map(|p| p.country.clone())
            .collect::<HashSet<_>>();

        tracing::info!(
            "Memory-only mode: Found {} unique peers in {} countries for network {}",
            peers_with_geo.len(),
            countries.len(),
            network
        );

        // Log sample of peers
        let sample_size = std::cmp::min(5, peers_with_geo.len());
        for (i, peer) in peers_with_geo.iter().take(sample_size).enumerate() {
            tracing::info!(
                "Sample peer {}: IP: {}, Country: {}, ISP: {}",
                i + 1,
                peer.ip,
                peer.country
                    .clone()
                    .unwrap_or_else(|| "Unknown".to_string()),
                peer.isp.clone().unwrap_or_else(|| "Unknown".to_string())
            );
        }
    }

    Ok(())
}
