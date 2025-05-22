use crate::{error::AppError, models::PeerInfo};
use futures::stream::{self, StreamExt};
use parking_lot::Mutex;
use rand::seq::SliceRandom;
use reqwest::Client;
use std::{collections::HashSet, sync::Arc};
use tokio::sync::Semaphore;

use super::{
    peer_parser::fetch_peer_info,
    utils::{has_problematic_pattern, is_valid_public_ip},
};

/// Resolve a potentially local RPC address to a usable URL
pub fn resolve_rpc_url(peer_info: &PeerInfo) -> Option<String> {
    let rpc_address = peer_info.rpc_address.as_ref()?;

    // Skip if peer IP is invalid or private
    if !is_valid_public_ip(&peer_info.ip) {
        return None;
    }

    // Special handling for different RPC address patterns
    if rpc_address.contains("0.0.0.0") {
        // 0.0.0.0 can be accessed by replacing with the peer's IP
        tracing::debug!(
            "Processing 0.0.0.0 RPC address for peer {}: {}",
            peer_info.ip,
            rpc_address
        );
    } else if has_problematic_pattern(rpc_address) {
        // Skip truly problematic patterns that we can't transform
        tracing::debug!(
            "Skipping peer with problematic RPC address pattern: {}",
            rpc_address
        );
        return None;
    }

    // Use the utility function for transforming RPC address
    let base_url = match crate::utils::transform_rpc_address(rpc_address, &peer_info.ip) {
        Some(url) => url,
        None => {
            tracing::debug!(
                "Failed to transform RPC address for peer {}: {}",
                peer_info.ip,
                rpc_address
            );
            return None;
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

/// Recursively follow RPC addresses to discover additional peers with strict limits
pub async fn follow_rpc_addresses(
    client: &Client,
    initial_peers: &[PeerInfo],
    network: &str,
    visited: Arc<Mutex<HashSet<String>>>,
    max_depth: usize,
    max_peers: usize,
    max_concurrent: usize,
) -> Result<Vec<PeerInfo>, AppError> {
    // Initialize result with initial peers
    let mut all_peers: Vec<PeerInfo> = initial_peers.to_vec();

    // Create a set to efficiently track IPs we've already seen
    let mut seen_ips = HashSet::new();
    for peer in &all_peers {
        seen_ips.insert(peer.ip.clone());
    }

    // Use a queue for breadth-first search, but only add peers with valid public IPs to the queue
    // We still store all peers in all_peers regardless of IP type
    let mut queue: Vec<PeerInfo> = initial_peers
        .iter()
        .filter(|peer| is_valid_public_ip(&peer.ip))
        .cloned()
        .collect();

    // Limit the total number of peers we'll try to query to avoid excessive requests
    let max_query_attempts = if max_peers > 0 { max_peers } else { 50 };
    let mut attempted_queries = 0;

    let mut current_depth = 0;

    // Use a smaller queue size for each depth level to avoid querying too many peers
    let max_peers_per_depth = 10;

    while !queue.is_empty() && (max_depth == 0 || current_depth < max_depth) {
        // Check if we've reached the maximum number of peers
        if max_peers > 0 && all_peers.len() >= max_peers {
            tracing::info!("Reached maximum peer limit of {}", max_peers);
            break;
        }

        // Check if we've reached the maximum query attempts
        if attempted_queries >= max_query_attempts {
            tracing::info!(
                "Reached maximum query attempt limit of {}",
                max_query_attempts
            );
            break;
        }

        // Process limited peers at the current depth level
        let peers_limit = std::cmp::min(max_peers_per_depth, queue.len());
        let peers_at_current_depth = queue[0..peers_limit].to_vec();
        queue = queue[peers_limit..].to_vec();

        current_depth += 1;

        tracing::info!(
            "Processing {} peers at depth {} (attempted queries: {}/{})",
            peers_at_current_depth.len(),
            current_depth,
            attempted_queries,
            max_query_attempts
        );

        attempted_queries += peers_at_current_depth.len();

        // Use a semaphore to limit concurrent requests
        let semaphore = Arc::new(Semaphore::new(max_concurrent));

        // Use a stream to process peers concurrently with rate limiting
        let results = stream::iter(peers_at_current_depth)
            .map(|peer| {
                let peer_clone = peer.clone();
                let client = client.clone();
                let visited_clone = visited.clone();
                let network = network.to_string();
                let semaphore = semaphore.clone();

                async move {
                    // Acquire permit to limit concurrency
                    let _permit = semaphore.acquire().await.unwrap();

                    if let Some(url_to_fetch) = resolve_rpc_url(&peer_clone) {
                        // Skip if we've already visited this URL - do this first and return early
                        {
                            let mut visited_guard = visited_clone.lock();
                            if visited_guard.contains(&url_to_fetch) {
                                return (Vec::new(), Some("already_visited"));
                            }
                            // Mark as visited immediately to prevent duplicates
                            visited_guard.insert(url_to_fetch.clone());
                        }

                        tracing::debug!("Fetching from peer RPC at: {}", url_to_fetch);

                        // Fetch peers from this URL with a short timeout
                        let timeout = tokio::time::Duration::from_secs(10);
                        match tokio::time::timeout(
                            timeout,
                            fetch_peer_info(&client, &url_to_fetch, Some(&network)),
                        )
                        .await
                        {
                            Ok(Ok(new_peers)) => {
                                tracing::debug!(
                                    "Found {} peers from {}",
                                    new_peers.len(),
                                    url_to_fetch
                                );
                                (new_peers, None)
                            }
                            Ok(Err(e)) => {
                                let error_msg = e.to_string();
                                let error_type = if error_msg.contains("Connection refused") {
                                    "connection_refused"
                                } else if error_msg.contains("No route to host") {
                                    "no_route"
                                } else if error_msg.contains("400 Bad Request") {
                                    "bad_request"
                                } else {
                                    "other"
                                };
                                (Vec::new(), Some(error_type))
                            }
                            Err(_) => {
                                // Timeout
                                (Vec::new(), Some("timeout"))
                            }
                        }
                    } else {
                        // URL resolution failed (typically local IPs)
                        (Vec::new(), Some("local_address"))
                    }
                }
            })
            .buffer_unordered(max_concurrent) // Process up to max_concurrent requests concurrently
            .collect::<Vec<_>>()
            .await;

        // Process results and add new peers
        for (new_peers, _error_type) in results {
            for peer in new_peers {
                // Only add if IP is new to avoid duplicates
                if !seen_ips.contains(&peer.ip) {
                    seen_ips.insert(peer.ip.clone());
                    all_peers.push(peer.clone());

                    // Only add to the queue for further scanning if it has a valid public IP and an RPC address
                    if is_valid_public_ip(&peer.ip) && peer.rpc_address.is_some() {
                        queue.push(peer);
                    }
                }
            }
        }

        // Shuffle the queue to have diverse sources rather than all peers from one source
        let mut rng = rand::thread_rng();
        queue.shuffle(&mut rng);
    }

    tracing::info!(
        "Discovery complete: found {} unique peers, made {} queries",
        all_peers.len(),
        attempted_queries
    );

    Ok(all_peers)
}
