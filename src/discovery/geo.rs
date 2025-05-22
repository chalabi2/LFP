use crate::{
    error::AppError,
    models::{GeoApiResponse, PeerGeoInfo, PeerInfo},
};
use reqwest::Client;
use std::{collections::HashMap, time::Duration};
use tokio::time;

/// Fetch geographical information for a batch of peers
pub async fn fetch_geo_info_batch(
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
