use crate::{
    error::AppError,
    models::{PeerInfo, StatusResponse},
};
use futures::stream::{self, StreamExt};
use parking_lot::Mutex;
use rand::rngs::SmallRng;
use rand::seq::SliceRandom;
use rand::thread_rng;
use rand::SeedableRng;
use reqwest::{Client, Error as ReqwestError, Response};
use std::collections::HashMap;
use std::{collections::HashSet, sync::Arc};
use tokio::sync::Semaphore;
use tokio::time::error::Elapsed;

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
    let all_peers = Arc::new(Mutex::new(
        initial_peers
            .iter()
            .cloned()
            .map(|p| (p.ip.clone(), p.clone()))
            .collect::<HashMap<_, _>>(),
    ));

    let mut seen_ips: HashSet<String> = all_peers.lock().keys().cloned().collect();

    let mut queue: Vec<PeerInfo> = initial_peers
        .iter()
        .filter(|peer| is_valid_public_ip(&peer.ip))
        .cloned()
        .collect();

    let max_query_attempts = if max_peers > 0 { max_peers } else { 500 }; // Increased from 50 to 500
    let mut attempted_queries = 0;

    let mut current_depth = 0;

    let max_peers_per_depth = 50; // Increased from 10 to 50

    while !queue.is_empty() && (max_depth == 0 || current_depth < max_depth) {
        // Check if we've reached the maximum number of peers
        if max_peers > 0 && all_peers.lock().len() >= max_peers {
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

        let semaphore = Arc::new(Semaphore::new(max_concurrent));

        let results = stream::iter(peers_at_current_depth)
            .map(|peer| {
                let peer_clone = peer.clone();
                let client = client.clone();
                let visited_clone = visited.clone();
                let all_peers_clone = all_peers.clone();
                let network = network.to_string();
                let semaphore = semaphore.clone();

                async move {
                    let _permit = semaphore.acquire().await.unwrap();

                    if let Some(url_to_fetch) = resolve_rpc_url(&peer_clone) {
                        {
                            let mut visited_guard = visited_clone.lock();
                            if visited_guard.contains(&url_to_fetch) {
                                return (Vec::new(), Some("already_visited"));
                            }
                            visited_guard.insert(url_to_fetch.clone());
                        }

                        tracing::debug!("Fetching from URL: {}", url_to_fetch);

                        let base_url = if url_to_fetch.ends_with("/net_info") {
                            url_to_fetch[0..url_to_fetch.len() - 9].to_string()
                        } else {
                            url_to_fetch.clone()
                        };

                        let status_url = format!("{}/status", base_url);

                        let timeout_duration = tokio::time::Duration::from_secs(10);

                        let status_result: Result<Result<Response, ReqwestError>, Elapsed> =
                            tokio::time::timeout(timeout_duration, client.get(&status_url).send())
                                .await;

                        let (status_succeeded, is_live) = match status_result {
                            Ok(Ok(resp)) if resp.status().is_success() => {
                                let json_result: Result<
                                    Result<StatusResponse, ReqwestError>,
                                    Elapsed,
                                > = tokio::time::timeout(
                                    timeout_duration,
                                    resp.json::<StatusResponse>(),
                                )
                                .await;
                                match json_result {
                                    Ok(Ok(_json)) => {
                                        // Consider a peer live if we can successfully get status.
                                        // Be more permissive - even catching_up nodes are useful
                                        let is_responsive = true;
                                        (true, is_responsive)
                                    },
                                    Ok(Err(_)) => {
                                        // HTTP success but invalid JSON - still consider it live
                                        tracing::debug!("Got HTTP 200 but invalid JSON from {}, considering live", status_url);
                                        (true, true)
                                    },
                                    Err(_) => {
                                        // Timeout on JSON parsing - still consider it live if we got HTTP 200
                                        tracing::debug!("Timeout parsing JSON from {}, but HTTP 200, considering live", status_url);
                                        (true, true)
                                    },
                                }
                            }
                            Ok(Ok(resp)) => {
                                // HTTP response but not 2xx - peer is reachable but maybe unhealthy
                                tracing::debug!("Got HTTP {} from {}, considering not live", resp.status(), status_url);
                                (true, false)
                            }
                            Ok(Err(_)) => {
                                // Network error - peer not reachable
                                (false, false)
                            }
                            Err(_) => {
                                // Timeout - peer not reachable
                                (false, false)
                            }
                        };

                        {
                            let mut all_peers_guard = all_peers_clone.lock();
                            if let Some(p) = all_peers_guard.get_mut(&peer_clone.ip) {
                                p.is_live = Some(is_live);
                                tracing::debug!(
                                    "Liveness test for {} ({}): status_succeeded={}, is_live={}", 
                                    peer_clone.ip, status_url, status_succeeded, is_live
                                );
                            }
                        }

                        if !status_succeeded {
                            return (Vec::new(), Some("status_failed"));
                        }

                        let timeout = tokio::time::Duration::from_secs(10);
                        match tokio::time::timeout(
                            timeout,
                            fetch_peer_info(&client, &url_to_fetch, Some(&network)),
                        )
                        .await
                        {
                            Ok(Ok(new_peers)) => {
                                tracing::debug!(
                                    "Successfully parsed {} peers from {}",
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
                            Err(_) => (Vec::new(), Some("timeout")),
                        }
                    } else {
                        (Vec::new(), Some("local_address"))
                    }
                }
            })
            .buffer_unordered(max_concurrent)
            .collect::<Vec<_>>()
            .await;

        for (new_peers, _error_type) in results {
            for peer in new_peers {
                let ip = peer.ip.clone();

                if !seen_ips.contains(&ip) {
                    seen_ips.insert(ip.clone());
                    all_peers.lock().insert(ip, peer.clone());

                    if is_valid_public_ip(&peer.ip) && peer.rpc_address.is_some() {
                        queue.push(peer);
                    }
                }
            }
        }

        let mut rng = {
            let mut local_rng = thread_rng();
            SmallRng::from_rng(&mut local_rng).unwrap()
        };
        queue.shuffle(&mut rng);
    }

    let final_peers: Vec<PeerInfo> = all_peers.lock().values().cloned().collect();

    tracing::info!(
        "Discovery complete: found {} unique peers, made {} queries",
        final_peers.len(),
        attempted_queries
    );

    Ok(final_peers)
}
