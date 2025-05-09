use futures::future;
use futures::stream::{self, StreamExt};
use parking_lot::Mutex;
use reqwest::Client;
use sqlx::{Pool, Postgres};
use std::{
    collections::{HashMap, HashSet},
    net::IpAddr,
    str::FromStr,
    sync::Arc,
    time::Duration,
};
use tokio::time;
use url::Url;

use crate::{
    cli::Cli,
    config::Config,
    db,
    error::AppError,
    models::{GeoApiResponse, PeerData, PeerGeoInfo, PeerInfo, SeiPeerData, TendermintRpcResponse},
};

/// Start the peer discovery service that continuously scans for peers
pub async fn start_discovery_service(db_pool: Pool<Postgres>, cli: Cli) -> Result<(), AppError> {
    // Load configuration
    let config = Config::from_env()?;

    // Create a shared HTTP client
    let client = create_http_client(cli.request_timeout())?;

    // Run initial scan if configured
    if cli.scan_on_startup {
        tracing::info!("Running initial scan on startup");

        if let Some(network) = &cli.network {
            // Scan only the specified network
            if let Some(rpc_urls) = config.get_rpc_urls(network) {
                scan_network_with_fallback(&db_pool, network, rpc_urls, &client, &cli).await?;
            } else {
                tracing::warn!("Network {} not found in configuration", network);
            }
        } else {
            // Scan all networks in parallel
            let networks = config.network_names();
            let scan_futures = networks
                .iter()
                .filter_map(|network| {
                    config.get_rpc_urls(network).map(|rpc_urls| {
                        let db_pool = db_pool.clone();
                        let client = client.clone();
                        let cli = cli.clone();
                        let network = network.clone();

                        async move {
                            match scan_network_with_fallback(
                                &db_pool, &network, rpc_urls, &client, &cli,
                            )
                            .await
                            {
                                Ok(_) => tracing::info!("Successfully scanned network {}", network),
                                Err(e) => {
                                    tracing::error!("Error scanning network {}: {}", network, e)
                                }
                            }
                        }
                    })
                })
                .collect::<Vec<_>>();

            // Execute all scans concurrently
            futures::future::join_all(scan_futures).await;
        }
    }

    // If continuous scanning is enabled, start the interval-based scanning
    if cli.continuous {
        tracing::info!(
            "Starting continuous scanning with interval of {} seconds",
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
            interval.tick().await;
            tracing::info!("Starting scheduled scan for all networks");

            // Create a future for each network scan
            let scan_futures = networks
                .iter()
                .filter_map(|network| {
                    config.get_rpc_urls(network).map(|rpc_urls| {
                        let db_pool = db_pool.clone();
                        let client = client.clone();
                        let cli = cli.clone();
                        let network = network.clone();

                        async move {
                            match scan_network_with_fallback(
                                &db_pool, &network, rpc_urls, &client, &cli,
                            )
                            .await
                            {
                                Ok(_) => tracing::info!("Successfully scanned network {}", network),
                                Err(e) => {
                                    tracing::error!("Error scanning network {}: {}", network, e)
                                }
                            }
                        }
                    })
                })
                .collect::<Vec<_>>();

            // Execute all network scans concurrently
            futures::future::join_all(scan_futures).await;

            // No need for sleep between networks as they're processed concurrently
        }
    }

    Ok(())
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
        max_peers: 1000,
        continuous: true,
        max_depth: 0,
        request_timeout: 5,
        scan_on_startup: true,
        max_concurrent_requests: 100,
    };

    let client = create_http_client(Duration::from_secs(cli.request_timeout))?;

    // Execute the scan with fallback
    scan_network_with_fallback(&db_pool, &network, rpc_urls, &client, &cli).await?;

    Ok(())
}

/// Create an HTTP client with appropriate configuration
fn create_http_client(timeout: Duration) -> Result<Client, AppError> {
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .user_agent("lfp/0.1.0")
        .build()
        .map_err(|e| AppError::RequestError(format!("Failed to create HTTP client: {}", e)))?;

    Ok(client)
}

/// Scan a network with fallback to Polkachu endpoint if primary fails
async fn scan_network_with_fallback(
    db_pool: &Pool<Postgres>,
    network: &str,
    rpc_urls: (String, String),
    client: &Client,
    cli: &Cli,
) -> Result<(), AppError> {
    let (primary_url, fallback_url) = rpc_urls;

    // Try primary URL with exponential backoff (3 attempts)
    let mut attempt = 1;
    let max_attempts = 3;
    let mut backoff_duration = Duration::from_secs(1);

    while attempt <= max_attempts {
        tracing::info!(
            "Attempting to connect to primary endpoint for {} (attempt {}/{})",
            network,
            attempt,
            max_attempts
        );

        // Try primary URL with a timeout
        let primary_result = tokio::time::timeout(
            Duration::from_secs(10),
            scan_network(db_pool, network, &primary_url, client, cli),
        )
        .await;

        match primary_result {
            // Primary succeeded within timeout
            Ok(Ok(_)) => {
                tracing::info!(
                    "Successfully scanned network {} using primary endpoint (attempt {})",
                    network,
                    attempt
                );
                return Ok(());
            }
            // Primary timed out or failed
            _ => {
                if attempt < max_attempts {
                    tracing::warn!(
                        "Primary endpoint for {} failed or timed out on attempt {}/{}. Retrying in {:?}...",
                        network,
                        attempt,
                        max_attempts,
                        backoff_duration
                    );

                    // Wait with exponential backoff before retrying
                    tokio::time::sleep(backoff_duration).await;

                    // Increase backoff for next attempt (exponential)
                    backoff_duration *= 2;
                    attempt += 1;
                } else {
                    // All primary attempts failed, try fallback
                    break;
                }
            }
        }
    }

    // All primary attempts failed, try fallback URL
    tracing::warn!(
        "Primary endpoint for {} failed after {} attempts, trying fallback endpoint: {}",
        network,
        max_attempts,
        fallback_url
    );

    // Try fallback URL
    match scan_network(db_pool, network, &fallback_url, client, cli).await {
        Ok(_) => {
            tracing::info!(
                "Successfully scanned network {} using fallback endpoint",
                network
            );
            Ok(())
        }
        Err(e) => {
            tracing::error!(
                "Both primary and fallback endpoints failed for network {}: {}",
                network,
                e
            );
            Err(AppError::RequestError(format!(
                "Both primary and fallback endpoints failed for network {}",
                network
            )))
        }
    }
}

/// Scan a network for peers
async fn scan_network(
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
        return Ok(());
    }

    tracing::info!(
        "Found {} initial peers for {}",
        initial_peers.len(),
        network
    );

    // Follow RPC addresses to discover more peers
    let visited = Arc::new(Mutex::new(HashSet::new()));
    let all_peers = follow_rpc_addresses(
        client,
        &initial_peers,
        network,
        visited,
        cli.max_depth,
        cli.max_peers,
    )
    .await?;

    tracing::info!(
        "Discovered a total of {} peers for {}",
        all_peers.len(),
        network
    );

    // Get geographical information for all discovered peers
    let peers_with_geo = fetch_geo_info_batch(client, &all_peers, network).await?;

    tracing::info!(
        "Added geographical information to {} peers",
        peers_with_geo.len()
    );

    // Update the database with the discovered peers
    db::upsert_peers_batch(db_pool, &peers_with_geo, network).await?;

    tracing::info!("Updated database with peers for network {}", network);

    Ok(())
}

/// Fetch peer information from a blockchain node's RPC endpoint
async fn fetch_peer_info(
    client: &Client,
    url: &str,
    network: Option<&str>,
) -> Result<Vec<PeerInfo>, AppError> {
    // Make sure we're fetching from the /net_info endpoint
    let net_info_url = if url.ends_with("/net_info") {
        url.to_string()
    } else if url.ends_with("/") {
        format!("{}net_info", url)
    } else {
        format!("{}/net_info", url)
    };

    tracing::debug!("Fetching from URL: {}", net_info_url);

    // Make the request
    let response = client
        .get(&net_info_url)
        .send()
        .await
        .map_err(|e| AppError::RequestError(format!("Failed to fetch peer info: {}", e)))?;

    if !response.status().is_success() {
        return Err(AppError::RequestError(format!(
            "Failed to fetch peer info, status: {}",
            response.status()
        )));
    }

    let response_text = response
        .text()
        .await
        .map_err(|e| AppError::RequestError(format!("Failed to read response body: {}", e)))?;

    // Try to parse as JSON
    if let Ok(data) = serde_json::from_str::<TendermintRpcResponse>(&response_text) {
        // Special case for Sei network
        if let Some(network_name) = network {
            if network_name.to_uppercase() == "SEI" {
                if let Ok(sei_data) = serde_json::from_str::<Vec<SeiPeerData>>(&response_text) {
                    return parse_sei_peer_info(&sei_data);
                }
            }
        }

        // Handle standard Tendermint RPC response format
        if let Some(result) = data.result {
            if let Some(peers) = result.peers {
                return parse_standard_peer_info(&peers);
            }
        } else if let Some(peers) = data.peers {
            return parse_standard_peer_info(&peers);
        }
    }

    // If all parsing attempts failed
    tracing::error!("Failed to parse response as peer data");
    Ok(vec![])
}

/// Parse peer information from standard Tendermint format
fn parse_standard_peer_info(peers: &[PeerData]) -> Result<Vec<PeerInfo>, AppError> {
    let result = peers
        .iter()
        .filter_map(|peer| {
            // Extract IP from remote_ip or addr field
            let ip = if let Some(remote_ip) = &peer.remote_ip {
                remote_ip.clone()
            } else if let Some(addr) = &peer.addr {
                // Extract IP from addr field (format: "id@ip:port")
                addr.split('@')
                    .nth(1)
                    .and_then(|s| s.split(':').next())
                    .unwrap_or("")
                    .to_string()
            } else {
                return None;
            };

            // Skip if IP is invalid
            if !is_valid_public_ip(&ip) {
                return None;
            }

            // Extract RPC address
            let rpc_address = if let Some(node_info) = &peer.node_info {
                if let Some(other) = &node_info.other {
                    other.rpc_address.clone()
                } else {
                    None
                }
            } else {
                None
            }
            .or_else(|| peer.rpc_address.clone());

            Some(PeerInfo { ip, rpc_address })
        })
        .collect();

    Ok(result)
}

/// Parse peer information from Sei network format
fn parse_sei_peer_info(peers: &[SeiPeerData]) -> Result<Vec<PeerInfo>, AppError> {
    let result = peers
        .iter()
        .filter_map(|peer| {
            // Try to extract the peer's IP address from the URL
            let mut ip = String::new();

            // Format is often like "id@ip:port"
            if let Some(url_part) = peer.url.split('@').nth(1) {
                if let Some(ip_part) = url_part.split(':').next() {
                    ip = ip_part.to_string();
                }
            }

            // If we have a remote_ip field, prefer that
            if let Some(remote_ip) = &peer.remote_ip {
                if is_valid_public_ip(remote_ip) {
                    ip = remote_ip.clone();
                }
            }

            if ip.is_empty() || !is_valid_public_ip(&ip) {
                return None;
            }

            // Try to extract the RPC address
            let rpc_address = if let Some(node_info) = &peer.node_info {
                if let Some(other) = &node_info.other {
                    other.rpc_address.clone()
                } else {
                    None
                }
            } else {
                None
            }
            .or_else(|| peer.rpc_address.clone());

            Some(PeerInfo { ip, rpc_address })
        })
        .collect();

    Ok(result)
}

/// Check if an IP address is a valid public IP
fn is_valid_public_ip(ip: &str) -> bool {
    // Check if it's a valid IP format
    let parsed_ip = match IpAddr::from_str(ip) {
        Ok(addr) => addr,
        Err(_) => return false,
    };

    // Filter out private, local, and special purpose IPs
    match parsed_ip {
        IpAddr::V4(addr) => {
            let octets = addr.octets();

            // RFC 1918 (Private Use)
            if octets[0] == 10
                || (octets[0] == 172 && (octets[1] >= 16 && octets[1] <= 31))
                || (octets[0] == 192 && octets[1] == 168)
            {
                return false;
            }

            // Loopback, link-local, and other special ranges
            if octets[0] == 0 ||     // This network
               octets[0] == 127 ||   // Loopback
               (octets[0] == 169 && octets[1] == 254) || // Link-local
               (octets[0] == 192 && octets[1] == 0 && octets[2] == 0) || // IETF Protocol
               (octets[0] == 192 && octets[1] == 0 && octets[2] == 2) || // TEST-NET-1
               (octets[0] == 198 && octets[1] == 51 && octets[2] == 100) || // TEST-NET-2
               (octets[0] == 203 && octets[1] == 0 && octets[2] == 113) || // TEST-NET-3
               octets[0] >= 224 ||   // Multicast and reserved
               (octets[0] == 255 && octets[1] == 255 && octets[2] == 255 && octets[3] == 255)
            {
                // Broadcast
                return false;
            }

            true
        }
        IpAddr::V6(_) => {
            // For simplicity, we're allowing all IPv6 addresses that aren't loopback or link-local
            !parsed_ip.is_loopback() && !parsed_ip.is_unspecified()
        }
    }
}

/// Resolve a potentially local RPC address to a usable URL
fn resolve_rpc_url(peer_info: &PeerInfo) -> Option<String> {
    let rpc_address = peer_info.rpc_address.as_ref()?;

    // Skip if IP is invalid
    if !is_valid_public_ip(&peer_info.ip) {
        return None;
    }

    // Check if it's a local address that needs to be rewritten
    let is_local_address = rpc_address.contains("0.0.0.0")
        || rpc_address.contains("127.0.0.1")
        || rpc_address.contains("localhost");

    let base_url = if is_local_address {
        // Extract the port from the RPC address
        let parts: Vec<&str> = rpc_address.split(':').collect();
        if parts.len() < 2 {
            return None;
        }

        // Use the peer's IP with the port from the RPC address
        let port = parts[parts.len() - 1];
        format!("http://{}:{}", peer_info.ip, port)
    } else {
        // Clean the RPC address format (remove tcp:// prefix if present)
        let cleaned_url = rpc_address.replace("tcp://", "");

        // If no protocol is specified, add http://
        if !cleaned_url.starts_with("http://") && !cleaned_url.starts_with("https://") {
            format!("http://{}", cleaned_url)
        } else {
            cleaned_url
        }
    };

    // Ensure the URL ends with /net_info
    if base_url.ends_with("/net_info") {
        Some(base_url)
    } else if base_url.ends_with("/") {
        Some(format!("{}net_info", base_url))
    } else {
        Some(format!("{}/net_info", base_url))
    }
}

/// Recursively follow RPC addresses to discover additional peers
async fn follow_rpc_addresses(
    client: &Client,
    initial_peers: &[PeerInfo],
    network: &str,
    visited: Arc<Mutex<HashSet<String>>>,
    max_depth: usize,
    max_peers: usize,
) -> Result<Vec<PeerInfo>, AppError> {
    // Initialize result with initial peers
    let mut all_peers: Vec<PeerInfo> = initial_peers.to_vec();

    // Create a set to efficiently track IPs we've already seen
    let mut seen_ips = HashSet::new();
    for peer in &all_peers {
        seen_ips.insert(peer.ip.clone());
    }

    // Use a queue for breadth-first search
    let mut queue = initial_peers.to_vec();
    let mut current_depth = 0;

    while !queue.is_empty() && (max_depth == 0 || current_depth < max_depth) {
        // Check if we've reached the maximum number of peers
        if max_peers > 0 && all_peers.len() >= max_peers {
            tracing::info!("Reached maximum peer limit of {}", max_peers);
            break;
        }

        // Process all peers at the current depth level
        let peers_at_current_depth = queue.clone();
        queue.clear();
        current_depth += 1;

        tracing::info!(
            "Processing {} peers at depth {}",
            peers_at_current_depth.len(),
            current_depth
        );

        // Use a stream to process peers concurrently with rate limiting
        let results = stream::iter(peers_at_current_depth)
            .map(|peer| {
                let peer_clone = peer.clone();
                let client = client.clone();
                let visited_clone = visited.clone();
                let network = network.to_string();

                async move {
                    if let Some(url_to_fetch) = resolve_rpc_url(&peer_clone) {
                        // Skip if we've already visited this URL
                        {
                            let mut visited_guard = visited_clone.lock();
                            if visited_guard.contains(&url_to_fetch) {
                                return Vec::new();
                            }
                            visited_guard.insert(url_to_fetch.clone());
                        }

                        // Fetch peers from this URL
                        match fetch_peer_info(&client, &url_to_fetch, Some(&network)).await {
                            Ok(new_peers) => new_peers,
                            Err(e) => {
                                tracing::debug!("Error fetching from {}: {}", url_to_fetch, e);
                                Vec::new()
                            }
                        }
                    } else {
                        Vec::new()
                    }
                }
            })
            .buffer_unordered(50) // Process up to 50 requests concurrently
            .collect::<Vec<_>>()
            .await;

        // Process results and add new peers
        for new_peers in results {
            for peer in new_peers {
                if is_valid_public_ip(&peer.ip) && !seen_ips.contains(&peer.ip) {
                    seen_ips.insert(peer.ip.clone());
                    all_peers.push(peer.clone());
                    queue.push(peer);

                    // Check if we've reached the maximum peers limit
                    if max_peers > 0 && all_peers.len() >= max_peers {
                        break;
                    }
                }
            }
        }

        tracing::info!(
            "Depth {} complete, discovered {} total peers so far",
            current_depth,
            all_peers.len()
        );
    }

    Ok(all_peers)
}

/// Fetch geographical information for a batch of peers
async fn fetch_geo_info_batch(
    client: &Client,
    peers: &[PeerInfo],
    network: &str,
) -> Result<Vec<PeerGeoInfo>, AppError> {
    const BATCH_SIZE: usize = 100; // IP-API allows up to 100 IPs per batch
    let mut all_geo_info = Vec::new();

    // Get unique IPs only
    let mut unique_peers = HashMap::new();
    for peer in peers {
        unique_peers
            .entry(peer.ip.clone())
            .or_insert_with(|| peer.clone());
    }

    let unique_peer_vec: Vec<PeerInfo> = unique_peers.values().cloned().collect();

    // Process in batches
    for chunk in unique_peer_vec.chunks(BATCH_SIZE) {
        let ips: Vec<String> = chunk.iter().map(|p| p.ip.clone()).collect();

        // Prepare batch request
        let batch_request: Vec<serde_json::Value> = ips
            .iter()
            .map(|ip| serde_json::json!({ "query": ip }))
            .collect();

        // Make the request
        let response = client
            .post("http://ip-api.com/batch")
            .json(&batch_request)
            .send()
            .await
            .map_err(|e| AppError::RequestError(format!("Geo API request failed: {}", e)))?;

        if !response.status().is_success() {
            return Err(AppError::RequestError(format!(
                "Geo API returned error status: {}",
                response.status()
            )));
        }

        let geo_responses: Vec<GeoApiResponse> = response
            .json()
            .await
            .map_err(|e| AppError::JsonError(format!("Failed to parse geo API response: {}", e)))?;

        // Process the responses
        for (i, geo) in geo_responses.into_iter().enumerate() {
            if i >= chunk.len() {
                break;
            }

            let peer = &chunk[i];

            let peer_geo_info = PeerGeoInfo {
                ip: peer.ip.clone(),
                rpc_address: peer.rpc_address.clone(),
                network: network.to_string(),
                country: if geo.status == "success" {
                    geo.country
                } else {
                    None
                },
                region: if geo.status == "success" {
                    geo.region_name.clone()
                } else {
                    None
                },
                province: if geo.status == "success" {
                    geo.region_name
                } else {
                    None
                },
                state: if geo.status == "success" {
                    geo.region
                } else {
                    None
                },
                city: if geo.status == "success" {
                    geo.city
                } else {
                    None
                },
                isp: if geo.status == "success" {
                    geo.isp
                } else {
                    None
                },
                lat: if geo.status == "success" {
                    geo.lat
                } else {
                    None
                },
                lon: if geo.status == "success" {
                    geo.lon
                } else {
                    None
                },
                last_seen: None,
                active: true,
            };

            all_geo_info.push(peer_geo_info);
        }

        // Add a small delay between batches to respect rate limits
        time::sleep(Duration::from_millis(500)).await;
    }

    Ok(all_geo_info)
}
