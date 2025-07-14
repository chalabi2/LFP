use super::utils::{is_valid_public_ip, PROBLEMATIC_ENDPOINT_PATTERNS};
use crate::{
    error::AppError,
    models::{AkashPeerData, PeerData, PeerInfo, SeiPeerData, TendermintRpcResponse},
};
use reqwest::Client;
use std::time::Duration;

/// Fetch peer information from a blockchain node's RPC endpoint
pub async fn fetch_peer_info(
    client: &Client,
    url: &str,
    _network: Option<&str>,
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

    let _base_url = if net_info_url.ends_with("/net_info") {
        net_info_url[0..net_info_url.len() - 9].to_string()
    } else {
        net_info_url.clone()
    };

    // let _status_url = format!("{}/status", base_url);  // Comment out or remove

    // Make the request - add retries for transient errors
    let mut attempts = 0;
    let max_attempts = 1;

    // Check for problematic URL patterns to avoid excessive retries
    for pattern in PROBLEMATIC_ENDPOINT_PATTERNS.iter() {
        if net_info_url.contains(pattern) {
            tracing::debug!("Skipping known problematic endpoint: {}", net_info_url);
            return Ok(vec![]);
        }
    }

    // Try the net_info endpoint first
    while attempts < max_attempts {
        match client.get(&net_info_url).send().await {
            Ok(response) => {
                if response.status().is_success() {
                    // Handle successful response
                    match response.text().await {
                        Ok(response_text) => {
                            tracing::debug!("Received response from {}", net_info_url);

                            // Check if response contains "peers" field but it's empty
                            if let Ok(json) =
                                serde_json::from_str::<serde_json::Value>(&response_text)
                            {
                                let has_peers_field =
                                    json.get("result").and_then(|r| r.get("peers")).is_some()
                                        || json.get("peers").is_some();

                                if has_peers_field {
                                    // Check if it has a valid peers structure but is just empty
                                    let empty_peers = json
                                        .get("result")
                                        .and_then(|r| r.get("peers"))
                                        .and_then(|p| p.as_array())
                                        .map(|a| a.is_empty())
                                        .unwrap_or(false)
                                        || json
                                            .get("peers")
                                            .and_then(|p| p.as_array())
                                            .map(|a| a.is_empty())
                                            .unwrap_or(false);

                                    if empty_peers {
                                        tracing::info!(
                                            "Endpoint returned empty peers array: {}",
                                            net_info_url
                                        );
                                        // If the endpoint explicitly says it has no peers, return empty list
                                        return Ok(vec![]);
                                    }
                                }
                            }

                            // Try to parse the response using our flexible parser
                            let parsed_peers =
                                parse_flexible_peer_response(&response_text, _network);
                            if !parsed_peers.is_empty() {
                                tracing::debug!(
                                    "Successfully parsed {} peers using flexible parser",
                                    parsed_peers.len()
                                );
                                return Ok(parsed_peers);
                            }

                            // If our flexible parser failed, try the network-specific ones
                            let network_specific_peers =
                                try_parse_network_specific(&response_text, _network)?;
                            if !network_specific_peers.is_empty() {
                                tracing::debug!(
                                    "Successfully parsed {} peers using network-specific parser",
                                    network_specific_peers.len()
                                );
                                return Ok(network_specific_peers);
                            }

                            // Return empty if no peers found
                            return Ok(vec![]);
                        }
                        Err(e) => {
                            tracing::debug!(
                                "Failed to read response body from {}: {}",
                                net_info_url,
                                e
                            );
                        }
                    }
                } else if response.status().as_u16() == 429 {
                    // Handle rate limiting with longer backoff
                    let retry_after = response
                        .headers()
                        .get("retry-after")
                        .and_then(|h| h.to_str().ok())
                        .and_then(|s| s.parse::<u64>().ok())
                        .unwrap_or(5); // Default to 5 seconds if no header

                    tracing::warn!(
                        "Rate limited (429) from {}, backing off for {} seconds",
                        net_info_url,
                        retry_after
                    );

                    // Wait for retry-after period
                    tokio::time::sleep(Duration::from_secs(retry_after)).await;

                    // Don't count this as a regular attempt
                    continue;
                } else {
                    tracing::debug!(
                        "Failed to fetch peer info from {}, status: {}",
                        net_info_url,
                        response.status()
                    );
                }
            }
            Err(e) => {
                // Differentiate between common error types
                if e.to_string().contains("Connection refused") {
                    tracing::debug!("Connection refused for {}", net_info_url);
                } else if e.to_string().contains("No route to host") {
                    tracing::debug!("No route to host for {}", net_info_url);
                } else if e.to_string().contains("429")
                    || e.to_string().contains("Too Many Requests")
                {
                    // Try to catch rate limiting in error messages
                    tracing::warn!("Possible rate limiting for {}: {}", net_info_url, e);
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                } else {
                    tracing::debug!("Request error for {}: {}", net_info_url, e);
                }
            }
        }

        // Increment attempts and retry if not at max
        attempts += 1;
        if attempts < max_attempts {
            tracing::debug!(
                "Retrying fetch_peer_info for {} ({}/{})",
                net_info_url,
                attempts,
                max_attempts
            );
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    // If all attempts failed, return empty list rather than error
    tracing::debug!(
        "Returning empty peer list after all attempts for {}",
        net_info_url
    );
    Ok(vec![])
}

/// Parse a response using a flexible approach that works for multiple networks
pub fn parse_flexible_peer_response(response_text: &str, _network: Option<&str>) -> Vec<PeerInfo> {
    // Try to parse the response as a generic JSON structure first
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(response_text) {
        // Try several common response formats

        // Format 1: Standard Tendermint format with result.peers
        if let Some(result) = value.get("result") {
            if let Some(peers) = result.get("peers") {
                if let Some(peers_array) = peers.as_array() {
                    let peers = extract_peers_from_array(peers_array);
                    if !peers.is_empty() {
                        return peers;
                    }
                }
            }
        }

        // Format 2: Direct peers array at the top level
        if let Some(peers) = value.get("peers") {
            if let Some(peers_array) = peers.as_array() {
                let peers = extract_peers_from_array(peers_array);
                if !peers.is_empty() {
                    return peers;
                }
            }
        }

        // Format 3: Nested in data structure
        if let Some(data) = value.get("data") {
            if let Some(peers) = data.get("peers") {
                if let Some(peers_array) = peers.as_array() {
                    let peers = extract_peers_from_array(peers_array);
                    if !peers.is_empty() {
                        return peers;
                    }
                }
            }

            if let Some(result) = data.get("result") {
                if let Some(peers) = result.get("peers") {
                    if let Some(peers_array) = peers.as_array() {
                        let peers = extract_peers_from_array(peers_array);
                        if !peers.is_empty() {
                            return peers;
                        }
                    }
                }
            }
        }

        // Try a deep search for any array that looks like peers
        return search_for_peers_recursively(&value);
    }

    vec![]
}

/// Extract peer information from an array of JSON objects
fn extract_peers_from_array(peers_array: &[serde_json::Value]) -> Vec<PeerInfo> {
    let mut peers = Vec::new();

    for peer in peers_array {
        // First, try to get remote_ip as it's the most reliable source of the peer's IP
        let remote_ip = peer
            .get("remote_ip")
            .and_then(|v| v.as_str())
            .map(String::from);

        // If remote_ip is not available, try other IP sources
        let fallback_ip = peer
            .get("ip")
            .or_else(|| peer.get("address"))
            .and_then(|v| v.as_str())
            .map(String::from);

        // Also try to extract IP from addr field (format often like "id@ip:port")
        let ip_from_addr = peer.get("addr").and_then(|v| v.as_str()).and_then(|addr| {
            addr.split('@')
                .nth(1)
                .and_then(|s| s.split(':').next())
                .map(String::from)
        });

        // Also check listen_addr in node_info
        let ip_from_listen_addr = peer
            .get("node_info")
            .and_then(|n| n.get("listen_addr"))
            .and_then(|v| v.as_str())
            .and_then(|addr| {
                // Format could be "IP:port" or similar
                addr.split(':').next().map(String::from)
            });

        // Prefer remote_ip, then fallback to other sources
        let ip = remote_ip.or_else(|| fallback_ip.or_else(|| ip_from_addr.or(ip_from_listen_addr)));

        if let Some(ip) = ip {
            // Only filter by valid public IP if we don't have a remote_ip
            if is_valid_public_ip(&ip) || peer.get("remote_ip").is_some() {
                // Extract RPC address from various possible fields
                let rpc_address = peer
                    .get("rpc_address")
                    .or_else(|| peer.get("rpcAddress"))
                    .or_else(|| {
                        peer.get("node_info")
                            .and_then(|n| n.get("other"))
                            .and_then(|o| o.get("rpc_address"))
                    })
                    .and_then(|v| v.as_str())
                    .map(String::from);

                // Debug log for better visibility
                if let Some(rpc) = &rpc_address {
                    tracing::debug!("Found peer with IP: {} and RPC: {}", ip, rpc);

                    // Add special handling for 0.0.0.0 RPC addresses
                    if rpc.contains("0.0.0.0") {
                        tracing::debug!(
                            "Processing peer with 0.0.0.0 RPC address - accessible via remote_ip"
                        );
                    }
                } else {
                    tracing::debug!("Found peer with IP: {} but no RPC address", ip);
                }

                let node_id = peer
                    .get("node_info")
                    .and_then(|n| n.get("id"))
                    .and_then(|v| v.as_str())
                    .map(String::from);

                let p2p_port = peer
                    .get("node_info")
                    .and_then(|n| n.get("listen_addr"))
                    .and_then(|v| v.as_str())
                    .and_then(|addr| addr.split(':').last())
                    .and_then(|p| p.parse::<u16>().ok());

                peers.push(PeerInfo {
                    ip,
                    rpc_address,
                    is_live: None, // Will be set later
                    node_id,
                    p2p_port,
                });
            }
        }
    }

    peers
}

/// Recursively search a JSON value for any array that looks like peers
fn search_for_peers_recursively(value: &serde_json::Value) -> Vec<PeerInfo> {
    match value {
        serde_json::Value::Array(arr) => {
            // Check if this array could be a list of peers
            let extracted = extract_peers_from_array(arr);
            if !extracted.is_empty() {
                return extracted;
            }

            // If not, search each element
            for item in arr {
                let result = search_for_peers_recursively(item);
                if !result.is_empty() {
                    return result;
                }
            }
        }
        serde_json::Value::Object(obj) => {
            // Search each field
            for (_, v) in obj {
                let result = search_for_peers_recursively(v);
                if !result.is_empty() {
                    return result;
                }
            }
        }
        _ => {}
    }

    vec![]
}

/// Try network-specific parsing approaches
fn try_parse_network_specific(
    response_text: &str,
    network: Option<&str>,
) -> Result<Vec<PeerInfo>, AppError> {
    // Special case for Akash network
    if let Some(network_name) = network {
        if network_name.to_uppercase() == "AKASH" {
            tracing::debug!("Processing Akash network response");

            // Try to parse as Akash-specific format
            if let Ok(akash_data) = serde_json::from_str::<Vec<AkashPeerData>>(response_text) {
                tracing::info!(
                    "Successfully parsed Akash-specific format with {} peers",
                    akash_data.len()
                );
                return parse_akash_peer_info(&akash_data);
            }
        }

        // Special case for Sei network
        if network_name.to_uppercase() == "SEI" {
            if let Ok(sei_data) = serde_json::from_str::<Vec<SeiPeerData>>(response_text) {
                return parse_sei_peer_info(&sei_data);
            }
        }
    }

    // Try standard Tendermint format
    if let Ok(data) = serde_json::from_str::<TendermintRpcResponse>(response_text) {
        if let Some(result) = data.result {
            if let Some(peers) = result.peers {
                return parse_standard_peer_info(&peers);
            }
        } else if let Some(peers) = data.peers {
            return parse_standard_peer_info(&peers);
        }
    }

    // Return empty list if no format matched
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

            let node_id = peer.node_info.as_ref().and_then(|n| n.id.clone());

            let p2p_port = peer
                .node_info
                .as_ref()
                .and_then(|n| n.listen_addr.as_ref())
                .and_then(|addr| addr.split(':').last())
                .and_then(|p| p.parse::<u16>().ok());

            Some(PeerInfo {
                ip,
                rpc_address,
                is_live: None,
                node_id,
                p2p_port,
            })
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

            let node_id = peer.node_info.as_ref().and_then(|n| n.id.clone());

            let p2p_port = peer
                .node_info
                .as_ref()
                .and_then(|n| n.listen_addr.as_ref())
                .and_then(|addr| addr.split(':').last())
                .and_then(|p| p.parse::<u16>().ok());

            Some(PeerInfo {
                ip,
                rpc_address,
                is_live: None,
                node_id,
                p2p_port,
            })
        })
        .collect();

    Ok(result)
}

/// Parse peer information from Akash network format
fn parse_akash_peer_info(peers: &[AkashPeerData]) -> Result<Vec<PeerInfo>, AppError> {
    let result = peers
        .iter()
        .filter_map(|peer| {
            // Try to get IP from remote_ip field
            let ip = if let Some(remote_ip) = &peer.remote_ip {
                remote_ip.clone()
            } else if let Some(address) = &peer.address {
                // Extract IP from address field (format: "id@ip:port")
                address
                    .split('@')
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

            // Use the RPC address if available
            let rpc_address = peer.rpc_address.clone();

            let node_id = None; // Adjust if Akash has node_info
            let p2p_port = None; // Adjust accordingly
            Some(PeerInfo {
                ip,
                rpc_address,
                is_live: None,
                node_id,
                p2p_port,
            })
        })
        .collect();

    Ok(result)
}
