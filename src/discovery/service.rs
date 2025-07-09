use crate::{cli::Cli, config::Config, error::AppError};
use futures::future::join_all;
use rand::seq::SliceRandom;
use reqwest::Client;
use sqlx::{Pool, Postgres};
use std::collections::HashMap;
use std::collections::HashSet;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    sync::{broadcast, Mutex, Semaphore},
    time,
};

use super::{
    cache, memory::run_memory_only_discovery, network_scanner::scan_network,
    utils::create_http_client,
};

// Constants for chain scanning
const MAX_PEERS_PER_CHAIN: usize = 4000;
const MAX_SCAN_TIME_PER_CHAIN: Duration = Duration::from_secs(3600); // 1 hour
const WORKERS_PER_CHAIN: usize = 25; // Number of concurrent workers per chain

// Add in the struct that manages scanning
#[allow(dead_code)]
struct NetworkScanState {
    // Maximum number of consecutive scans with minimal change
    max_stable_scans: usize,
    // Threshold for what counts as a significant change (in percent)
    min_change_threshold: f64,
    // Track peer counts by network
    peer_counts: HashMap<String, usize>,
    // Track consecutive scans with minimal change
    stable_scan_count: HashMap<String, usize>,
    // Last scan time per network
    last_scan_time: HashMap<String, Instant>,
    // Network scan count
    scan_count: HashMap<String, usize>,
}

impl NetworkScanState {
    #[allow(dead_code)]
    fn new() -> Self {
        Self {
            max_stable_scans: 3,
            min_change_threshold: 5.0, // 5% change
            peer_counts: HashMap::new(),
            stable_scan_count: HashMap::new(),
            last_scan_time: HashMap::new(),
            scan_count: HashMap::new(),
        }
    }

    #[allow(dead_code)]
    fn update_peer_count(&mut self, network: &str, new_count: usize) -> bool {
        // Record scan count
        *self.scan_count.entry(network.to_string()).or_insert(0) += 1;

        // Update last scan time
        self.last_scan_time
            .insert(network.to_string(), Instant::now());

        // Get previous count
        let prev_count = *self.peer_counts.get(network).unwrap_or(&0);

        // Calculate percent change
        let percent_change = if prev_count == 0 {
            100.0 // First scan always counts as significant
        } else {
            let diff = (new_count as isize - prev_count as isize).abs() as f64;
            (diff / prev_count as f64) * 100.0
        };

        // Update stored count
        self.peer_counts.insert(network.to_string(), new_count);

        // Check if this is a significant change
        let is_significant = percent_change >= self.min_change_threshold;

        // Update stability counter
        if is_significant {
            // Reset stable count if significant change
            self.stable_scan_count.insert(network.to_string(), 0);
        } else {
            // Increment stable count
            let count = self
                .stable_scan_count
                .entry(network.to_string())
                .or_insert(0);
            *count += 1;
        }

        // Return whether we should continue scanning this network
        let stable_count = *self.stable_scan_count.get(network).unwrap_or(&0);
        let should_continue = stable_count < self.max_stable_scans;

        tracing::info!(
            "Network {} peer count: {} -> {} ({}% change, stability: {}/{})",
            network,
            prev_count,
            new_count,
            percent_change as usize,
            stable_count,
            self.max_stable_scans
        );

        should_continue
    }

    #[allow(dead_code)]
    fn should_scan_network(&self, network: &str) -> bool {
        let stable_count = *self.stable_scan_count.get(network).unwrap_or(&0);
        stable_count < self.max_stable_scans
    }
}

/// Start the peer discovery service that continuously scans for peers
#[allow(dead_code)]
pub async fn start_discovery_service(
    db_pool: Pool<Postgres>,
    cli: Cli,
    mut shutdown_rx: Option<broadcast::Receiver<()>>,
) -> Result<(), AppError> {
    // Get configuration for networks and endpoints
    let config = Config::from_env()?;

    // Create a shared HTTP client
    let client = create_http_client(cli.request_timeout())?;

    // Get all networks to scan
    let all_networks = config.network_names();
    if all_networks.is_empty() {
        return Err(AppError::ConfigError(
            "No networks found in configuration".to_string(),
        ));
    }

    tracing::info!(
        "Starting sequential chain scanning with {} workers per chain",
        WORKERS_PER_CHAIN
    );
    tracing::info!(
        "Will scan each chain until finding {} peers or scanning for 1 hour",
        MAX_PEERS_PER_CHAIN
    );

    // Create network scan tracker
    let mut scan_state = NetworkScanState::new();

    // Create a channel specifically for signaling all discovery workers to stop
    let (worker_shutdown_tx, _) = broadcast::channel::<()>(50);

    // Infinite loop to continuously scan networks sequentially
    loop {
        // Check for shutdown signal before starting a new scanning cycle
        if let Some(ref mut rx) = shutdown_rx {
            match rx.try_recv() {
                Ok(_) => {
                    tracing::info!("Received shutdown signal, stopping discovery service");
                    // Forward shutdown signal to all active workers
                    let _ = worker_shutdown_tx.send(());
                    break;
                }
                Err(broadcast::error::TryRecvError::Empty) => {
                    // No shutdown signal yet, continue
                }
                Err(broadcast::error::TryRecvError::Closed) => {
                    tracing::info!("Shutdown channel closed, stopping discovery service");
                    let _ = worker_shutdown_tx.send(());
                    break;
                }
                Err(broadcast::error::TryRecvError::Lagged(_)) => {
                    tracing::info!("Shutdown signal lagged, stopping discovery service");
                    let _ = worker_shutdown_tx.send(());
                    break;
                }
            }
        }

        for network in &all_networks {
            // Check for shutdown signal before starting each network
            if let Some(ref mut rx) = shutdown_rx {
                match rx.try_recv() {
                    Ok(_) => {
                        tracing::info!("Received shutdown signal, stopping discovery service");
                        // Forward shutdown signal to all active workers
                        let _ = worker_shutdown_tx.send(());
                        break;
                    }
                    Err(broadcast::error::TryRecvError::Empty) => {
                        // No shutdown signal yet, continue
                    }
                    Err(broadcast::error::TryRecvError::Closed) => {
                        tracing::info!("Shutdown channel closed, stopping discovery service");
                        let _ = worker_shutdown_tx.send(());
                        break;
                    }
                    Err(broadcast::error::TryRecvError::Lagged(_)) => {
                        tracing::info!("Shutdown signal lagged, stopping discovery service");
                        let _ = worker_shutdown_tx.send(());
                        break;
                    }
                }
            }

            // Skip networks that haven't shown significant changes in peer count
            if !scan_state.should_scan_network(network) {
                tracing::info!(
                    "Skipping network {} as it has stabilized (not yielding new peers)",
                    network
                );
                continue;
            }

            // Skip invalid networks
            if let Some(rpc_urls) = config.get_rpc_urls(network) {
                let scan_start = Instant::now();
                tracing::info!("Starting scan for network: {}", network);

                // Shared state for tracking peers
                let discovered_peers = Arc::new(Mutex::new(HashSet::new()));

                // Track if we should stop scanning this chain
                let should_stop = Arc::new(Mutex::new(false));

                // Try to use cached peers first
                let use_cached_peers = !cache::get_cached_peers(network).is_empty();

                if use_cached_peers {
                    tracing::info!("Using cached peers for network {}", network);
                    match scan_chain_with_cached_peers(
                        &db_pool,
                        network,
                        &client,
                        &cli,
                        discovered_peers.clone(),
                        should_stop.clone(),
                        worker_shutdown_tx.subscribe(),
                    )
                    .await
                    {
                        Ok(total_peers) => {
                            tracing::info!("Successfully scanned network {} using cached peers, found {} peers", network, total_peers);

                            // Update peer count in our tracking
                            let should_continue =
                                scan_state.update_peer_count(network, total_peers);
                            if !should_continue {
                                tracing::info!(
                                    "Network {} has stabilized, moving to next network",
                                    network
                                );
                                continue;
                            }

                            // If we already have enough peers, continue to next network
                            if total_peers >= MAX_PEERS_PER_CHAIN {
                                continue;
                            }
                        }
                        Err(e) => {
                            tracing::error!(
                                "Error scanning network {} with cached peers: {}",
                                network,
                                e
                            );
                            // Fall through to RPC endpoint scan
                        }
                    }
                }

                // If we haven't found enough peers yet, use regular RPC endpoints
                tracing::info!("Scanning network {} using configured endpoints", network);

                // Reset should_stop flag
                *should_stop.lock().await = false;

                match scan_chain_with_multiple_workers(
                    &db_pool,
                    network,
                    rpc_urls,
                    &client,
                    &cli,
                    discovered_peers.clone(),
                    should_stop.clone(),
                    scan_start,
                    worker_shutdown_tx.subscribe(),
                )
                .await
                {
                    Ok(total_peers) => {
                        tracing::info!(
                            "Completed scan for network {}, found {} peers in {:?}",
                            network,
                            total_peers,
                            scan_start.elapsed()
                        );

                        // Update peer count in our tracking
                        scan_state.update_peer_count(network, total_peers);
                    }
                    Err(e) => {
                        tracing::error!("Error scanning network {}: {}", network, e);
                    }
                }

                // Sleep briefly before moving to next chain
                tokio::time::sleep(Duration::from_secs(1)).await;
            } else {
                tracing::warn!("Network {} not found in configuration, skipping", network);
            }
        }

        // Check for shutdown signal after completing a full cycle
        if let Some(ref mut rx) = shutdown_rx {
            match rx.try_recv() {
                Ok(_) => {
                    tracing::info!("Received shutdown signal, stopping discovery service");
                    // Forward shutdown signal to all active workers
                    let _ = worker_shutdown_tx.send(());
                    break;
                }
                Err(broadcast::error::TryRecvError::Empty) => {
                    // No shutdown signal yet, continue
                }
                Err(broadcast::error::TryRecvError::Closed) => {
                    tracing::info!("Shutdown channel closed, stopping discovery service");
                    let _ = worker_shutdown_tx.send(());
                    break;
                }
                Err(broadcast::error::TryRecvError::Lagged(_)) => {
                    tracing::info!("Shutdown signal lagged, stopping discovery service");
                    let _ = worker_shutdown_tx.send(());
                    break;
                }
            }
        }

        // Optional small delay between full cycles
        time::sleep(Duration::from_secs(10)).await;
        tracing::info!("Completed full cycle of all networks, starting again");
    }

    Ok(())
}

/// Scan a network by spawning multiple workers that crawl through peer connections
async fn scan_chain_with_multiple_workers(
    db_pool: &Pool<Postgres>,
    network: &str,
    rpc_urls: (String, String),
    client: &Client,
    cli: &Cli,
    discovered_peers: Arc<Mutex<HashSet<String>>>,
    should_stop: Arc<Mutex<bool>>,
    scan_start: Instant,
    mut worker_shutdown_rx: broadcast::Receiver<()>,
) -> Result<usize, AppError> {
    // First, try primary URL
    let (primary_url, fallback_url) = rpc_urls;

    // Shared semaphore to limit number of concurrent workers per chain
    let semaphore = Arc::new(Semaphore::new(WORKERS_PER_CHAIN));

    // Manage worker handles
    let mut workers = Vec::new();
    let mut failed_endpoints = HashSet::new();
    let max_workers = 10; // Lower this to reduce parallel scanning
    let max_consecutive_failures = 3;
    let mut consecutive_failures = 0;

    // Add primary endpoint as the first worker
    tracing::info!(
        "Adding primary worker for {} using endpoint: {}",
        network,
        primary_url
    );
    workers.push(
        spawn_scan_worker(
            db_pool,
            network,
            &primary_url,
            client,
            cli,
            discovered_peers.clone(),
            should_stop.clone(),
            semaphore.clone(),
            scan_start,
            worker_shutdown_rx.resubscribe(),
        )
        .await,
    );

    // Then add fallback to ensure we have at least 2 endpoints
    tracing::info!(
        "Adding fallback worker for {} using endpoint: {}",
        network,
        fallback_url
    );
    workers.push(
        spawn_scan_worker(
            db_pool,
            network,
            &fallback_url,
            client,
            cli,
            discovered_peers.clone(),
            should_stop.clone(),
            semaphore.clone(),
            scan_start,
            worker_shutdown_rx.resubscribe(),
        )
        .await,
    );

    let mut active_workers = 2;
    let mut worker_index = 0;

    // Main worker management loop
    while active_workers > 0 {
        // Check for stop conditions
        let should_stop_now = {
            let stop_guard = should_stop.lock().await;
            *stop_guard
        };

        // If we've been instructed to stop or already ran for max time, break the loop
        if should_stop_now || scan_start.elapsed() >= MAX_SCAN_TIME_PER_CHAIN {
            tracing::info!(
                "Stopping scan for network {} (should_stop={}, elapsed={:?})",
                network,
                should_stop_now,
                scan_start.elapsed()
            );
            break;
        }

        // Try to receive a shutdown signal with a very short timeout
        match worker_shutdown_rx.try_recv() {
            Ok(_) => {
                tracing::info!("Received worker shutdown signal for network {}", network);
                break;
            }
            Err(broadcast::error::TryRecvError::Empty) => {
                // No shutdown signal, continue
            }
            Err(e) => {
                tracing::warn!("Error receiving worker shutdown signal: {}", e);
            }
        }

        let peers_count = {
            let peers_guard = discovered_peers.lock().await;
            peers_guard.len()
        };

        // First check if any workers have completed
        let mut i = 0;
        while i < workers.len() {
            if workers[i].is_finished() {
                workers.remove(i);
                active_workers -= 1;
                // Don't increment i when removing
            } else {
                i += 1;
            }
        }

        // If we have enough peers or have reached max worker count, don't add more
        if peers_count >= MAX_PEERS_PER_CHAIN || active_workers >= max_workers {
            if peers_count >= MAX_PEERS_PER_CHAIN {
                tracing::info!(
                    "Reached target of {} peers for network {}, waiting for workers to finish",
                    peers_count,
                    network
                );
                // Signal workers to stop
                *should_stop.lock().await = true;
            }
            // Sleep briefly before checking again
            time::sleep(Duration::from_millis(500)).await;
            continue;
        }

        // Try to get more workers from the peer cache
        let all_peers = cache::get_cached_peers(network);
        if all_peers.len() > worker_index {
            let max_to_add = std::cmp::min(2, max_workers - active_workers);
            let mut added = 0;

            // Spawn workers using next few peers from the cache
            for i in 0..max_to_add {
                if worker_index + i >= all_peers.len() {
                    break;
                }

                let peer = &all_peers[worker_index + i];
                if let Some(rpc_url) = super::peer_traversal::resolve_rpc_url(peer) {
                    // Skip if we've already tried this endpoint
                    if failed_endpoints.contains(&rpc_url) {
                        continue;
                    }

                    // Additional check to avoid adding problematic endpoints
                    if super::utils::has_problematic_pattern(&rpc_url) {
                        failed_endpoints.insert(rpc_url.clone());
                        continue;
                    }

                    tracing::info!(
                        "Adding worker {} for {} using peer RPC: {}",
                        workers.len() + 1,
                        network,
                        rpc_url
                    );

                    workers.push(
                        spawn_scan_worker(
                            db_pool,
                            network,
                            &rpc_url,
                            client,
                            cli,
                            discovered_peers.clone(),
                            should_stop.clone(),
                            semaphore.clone(),
                            scan_start,
                            worker_shutdown_rx.resubscribe(),
                        )
                        .await,
                    );

                    active_workers += 1;
                    added += 1;
                }
            }

            worker_index += max_to_add;

            if added > 0 {
                consecutive_failures = 0;
            } else {
                consecutive_failures += 1;
            }

            // If we've repeatedly failed to add workers, we might be out of good endpoints
            if consecutive_failures >= max_consecutive_failures {
                tracing::warn!(
                    "Failed to add workers for network {} after {} attempts, stopping",
                    network,
                    consecutive_failures
                );
                break;
            }
        } else {
            // If we've gone through all cached peers and still need more workers,
            // we might be stuck or have exhausted all available endpoints
            consecutive_failures += 1;
            if consecutive_failures >= max_consecutive_failures {
                tracing::info!(
                    "No more endpoints available for network {}, waiting for active workers",
                    network
                );
                break;
            }
        }

        // Sleep before checking again to avoid CPU spinning
        time::sleep(Duration::from_secs(1)).await;
    }

    // Wait for all remaining workers to finish or kill them after timeout
    let timeout = Duration::from_secs(10); // Short timeout before we force stop workers
    if !workers.is_empty() {
        tracing::info!(
            "Waiting for {} remaining workers for network {} to finish",
            workers.len(),
            network
        );

        match time::timeout(timeout, futures::future::join_all(workers)).await {
            Ok(_) => {
                tracing::info!("All workers for network {} completed", network);
            }
            Err(_) => {
                tracing::warn!(
                    "Timed out waiting for workers for network {} to finish",
                    network
                );
                // Workers will be dropped when they go out of scope
            }
        }
    }

    // Get final peer count
    let total_peers = {
        let peers_guard = discovered_peers.lock().await;
        peers_guard.len()
    };

    Ok(total_peers)
}

/// Spawn a worker to scan the network from a specific endpoint
async fn spawn_scan_worker(
    db_pool: &Pool<Postgres>,
    network: &str,
    url: &str,
    client: &Client,
    cli: &Cli,
    _discovered_peers: Arc<Mutex<HashSet<String>>>,
    should_stop: Arc<Mutex<bool>>,
    semaphore: Arc<Semaphore>,
    _scan_start: Instant,
    mut worker_shutdown_rx: broadcast::Receiver<()>,
) -> tokio::task::JoinHandle<()> {
    let db_pool = db_pool.clone();
    let network = network.to_string();
    let url = url.to_string();
    let client = client.clone();
    let cli = cli.clone();

    tokio::spawn(async move {
        // Acquire a permit from the semaphore
        let permit = match semaphore.acquire().await {
            Ok(permit) => permit,
            Err(_) => {
                tracing::error!("Failed to acquire semaphore permit, worker shutting down");
                return;
            }
        };

        // Check shutdown signal and should_stop flag
        if worker_shutdown_rx.try_recv().is_ok() || *should_stop.lock().await {
            drop(permit);
            return;
        }

        // Use select to either run the scan or handle a shutdown signal
        let worker_result = tokio::select! {
            result = scan_network(&db_pool, &network, &url, &client, &cli) => result,
            _ = worker_shutdown_rx.recv() => {
                tracing::debug!("Worker for {} received shutdown signal, exiting", network);
                return;
            }
        };

        if let Err(e) = worker_result {
            tracing::error!("Worker for {} failed: {}", network, e);
        }

        // Release the permit explicitly (though it would be released automatically too)
        drop(permit);
    })
}

/// Scan a chain using cached peers
#[allow(dead_code)]
async fn scan_chain_with_cached_peers(
    db_pool: &Pool<Postgres>,
    network: &str,
    client: &Client,
    cli: &Cli,
    discovered_peers: Arc<Mutex<HashSet<String>>>,
    should_stop: Arc<Mutex<bool>>,
    worker_shutdown_rx: broadcast::Receiver<()>,
) -> Result<usize, AppError> {
    // Get cached peers for this network
    let cached_peers = cache::get_cached_peers(network);

    if cached_peers.is_empty() {
        return Err(AppError::NotFoundError(format!(
            "No cached peers available for network {}",
            network
        )));
    }

    // Shuffle the peers to avoid hitting the same ones repeatedly
    let shuffled_peers = {
        let mut rng = rand::thread_rng();
        let mut shuffled_peers = cached_peers.clone();
        shuffled_peers.shuffle(&mut rng);
        shuffled_peers
    };

    // Use a semaphore to limit concurrent requests
    let semaphore = Arc::new(Semaphore::new(WORKERS_PER_CHAIN));

    // Take only up to 10 peers to try
    let peers_to_try = &shuffled_peers[0..std::cmp::min(10, shuffled_peers.len())];
    tracing::info!(
        "Trying {} cached peers for network {}",
        peers_to_try.len(),
        network
    );

    // Create futures for each peer
    let scan_futures = peers_to_try
        .iter()
        .filter_map(|peer| {
            super::peer_traversal::resolve_rpc_url(peer).map(|rpc_url| {
                let db_pool = db_pool.clone();
                let client = client.clone();
                let cli = cli.clone();
                let network = network.to_string();
                let semaphore = semaphore.clone();
                let discovered_peers = discovered_peers.clone();
                let should_stop = should_stop.clone();
                let mut worker_shutdown_rx = worker_shutdown_rx.resubscribe();

                async move {
                    // Acquire permit
                    let _permit = semaphore.acquire().await.unwrap();

                    // Check if we should stop
                    if *should_stop.lock().await {
                        return;
                    }

                    // Check for shutdown signal
                    if worker_shutdown_rx.try_recv().is_ok() {
                        *should_stop.lock().await = true;
                        return;
                    }

                    tracing::info!("Trying cached peer for {}: {}", network, rpc_url);

                    // Use select to either run the scan or handle a shutdown signal
                    let scan_result = tokio::select! {
                        result = scan_network(&db_pool, &network, &rpc_url, &client, &cli) => result,
                        _ = worker_shutdown_rx.recv() => {
                            *should_stop.lock().await = true;
                            return;
                        }
                    };

                    match scan_result {
                        Ok(_) => {
                            // Update discovered peers
                            let peer_cache = cache::get_cached_peers(&network);
                            let mut peers_guard = discovered_peers.lock().await;
                            for peer in &peer_cache {
                                peers_guard.insert(peer.ip.clone());
                            }

                            // Check if we've reached the target
                            if peers_guard.len() >= MAX_PEERS_PER_CHAIN {
                                tracing::info!(
                                    "Found enough peers ({}/{}), signaling to stop",
                                    peers_guard.len(),
                                    MAX_PEERS_PER_CHAIN
                                );
                                *should_stop.lock().await = true;
                            }

                            tracing::info!("Successfully scanned {} using cached peer", network);
                        }
                        Err(e) => {
                            tracing::debug!("Cached peer scan failed: {}", e);
                        }
                    }
                }
            })
        })
        .collect::<Vec<_>>();

    // Execute all peer scans with controlled concurrency
    join_all(scan_futures).await;

    // Get the final count of discovered peers
    let total_peers = discovered_peers.lock().await.len();

    if total_peers > 0 {
        Ok(total_peers)
    } else {
        Err(AppError::RequestError(format!(
            "All cached peers failed for network {}",
            network
        )))
    }
}

/// Trigger a scan for a specific network
pub async fn trigger_network_scan(
    db_pool: Pool<Postgres>,
    network: String,
    config: Config,
) -> Result<(), AppError> {
    // Get the RPC URLs for the network
    let rpc_urls = config.get_rpc_urls(&network).ok_or_else(|| {
        AppError::NotFoundError(format!("Network {} not found in configuration", network))
    })?;

    // Create default CLI settings for the scan
    let cli = Cli {
        port: 3000,
        database_url: None,
        scan_interval: 43200,
        network: None,
        max_peers: MAX_PEERS_PER_CHAIN, // Use our new constant
        continuous: false,
        max_depth: 0,        // No depth limit
        request_timeout: 30, // Increased timeout
        scan_on_startup: true,
        max_concurrent_requests: WORKERS_PER_CHAIN, // Use our worker count
        concurrent_chains: 1, // Always 1 since we're scanning one chain at a time
    };

    let client = create_http_client(cli.request_timeout())?;

    // Shared state for tracking peers
    let discovered_peers = Arc::new(Mutex::new(HashSet::new()));
    let should_stop = Arc::new(Mutex::new(false));
    let scan_start = Instant::now();

    // Create a worker shutdown channel but don't provide a receiver
    let (worker_shutdown_tx, _) = broadcast::channel::<()>(1);

    // Execute the scan with our new approach
    let result = scan_chain_with_multiple_workers(
        &db_pool,
        &network,
        rpc_urls,
        &client,
        &cli,
        discovered_peers.clone(),
        should_stop.clone(),
        scan_start,
        worker_shutdown_tx.subscribe(),
    )
    .await;

    match result {
        Ok(total_peers) => {
            tracing::info!(
                "Completed scan for network {}, found {} peers in {:?}",
                network,
                total_peers,
                scan_start.elapsed()
            );
            Ok(())
        }
        Err(e) => {
            tracing::error!("Error scanning network {}: {}", network, e);
            Err(e)
        }
    }
}

/// Spawn the discovery service in a background task
#[allow(dead_code)]
pub async fn spawn_discovery_service(
    pool: Pool<Postgres>,
    cli: Cli,
    shutdown_rx: Option<broadcast::Receiver<()>>,
) -> Result<(), AppError> {
    start_discovery_service(pool, cli, shutdown_rx).await
}

/// Spawn the memory-only discovery service in a background task
#[allow(dead_code)]
pub async fn spawn_memory_only_discovery(
    cli: Cli,
    shutdown_rx: Option<broadcast::Receiver<()>>,
) -> Result<(), AppError> {
    run_memory_only_discovery(cli, shutdown_rx).await
}
