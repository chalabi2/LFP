use axum::{
    extract::{Path, Query},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::{
    db,
    error::AppError,
    models::{PeerNode, PeerStats, SharedState},
    peer_discovery::trigger_network_scan,
};

/// Health check endpoint
pub async fn health_check() -> StatusCode {
    StatusCode::OK
}

/// Query parameters for filtering peers
#[derive(Debug, Deserialize)]
pub struct PeerQuery {
    pub limit: Option<usize>,
    pub active: Option<bool>,
}

/// Get all peers
pub async fn get_peers(
    state: SharedState,
    Query(query): Query<PeerQuery>,
) -> Result<Json<Vec<PeerNode>>, AppError> {
    let mut peers = db::get_all_peers(&state.db_pool).await?;

    // Apply filters if provided
    if let Some(active) = query.active {
        peers.retain(|p| p.active == active);
    }

    // Apply limit if provided
    if let Some(limit) = query.limit {
        peers.truncate(limit);
    }

    Ok(Json(peers))
}

/// Get peers by network
pub async fn get_peers_by_network(
    state: SharedState,
    Path(network): Path<String>,
    Query(query): Query<PeerQuery>,
) -> Result<Json<Vec<PeerNode>>, AppError> {
    let mut peers = db::get_peers_by_network(&state.db_pool, &network).await?;

    // Apply filters if provided
    if let Some(active) = query.active {
        peers.retain(|p| p.active == active);
    }

    // Apply limit if provided
    if let Some(limit) = query.limit {
        peers.truncate(limit);
    }

    Ok(Json(peers))
}

/// Get peers by country
pub async fn get_peers_by_country(
    state: SharedState,
    Path(country): Path<String>,
    Query(query): Query<PeerQuery>,
) -> Result<Json<Vec<PeerNode>>, AppError> {
    let mut peers = db::get_peers_by_country(&state.db_pool, &country).await?;

    // Apply filters if provided
    if let Some(active) = query.active {
        peers.retain(|p| p.active == active);
    }

    // Apply limit if provided
    if let Some(limit) = query.limit {
        peers.truncate(limit);
    }

    Ok(Json(peers))
}

/// Trigger an immediate scan
#[derive(Debug, Deserialize)]
pub struct ScanRequest {
    pub network: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ScanResponse {
    pub message: String,
    pub triggered: bool,
}

pub async fn trigger_scan(
    state: SharedState,
    Json(req): Json<ScanRequest>,
) -> Result<Json<ScanResponse>, AppError> {
    let network = req.network.as_deref();

    if let Some(network_name) = network {
        // Check if network exists in config
        if state.config.get_rpc_url(network_name).is_none() {
            return Err(AppError::NotFoundError(format!(
                "Network {} not found in configuration",
                network_name
            )));
        }

        // Trigger scan for specific network
        tokio::spawn(trigger_network_scan(
            state.db_pool.clone(),
            network_name.to_string(),
            state.config.clone(),
        ));

        Ok(Json(ScanResponse {
            message: format!("Scan triggered for network: {}", network_name),
            triggered: true,
        }))
    } else {
        // Trigger scan for all networks
        for network_name in state.config.network_names() {
            let db_pool = state.db_pool.clone();
            let config = state.config.clone();
            let network = network_name.clone();

            tokio::spawn(async move {
                if let Err(e) = trigger_network_scan(db_pool, network, config).await {
                    tracing::error!("Error during triggered scan: {}", e);
                }
            });
        }

        Ok(Json(ScanResponse {
            message: "Scan triggered for all networks".to_string(),
            triggered: true,
        }))
    }
}

/// Get peer statistics
pub async fn get_stats(state: SharedState) -> Result<Json<PeerStats>, AppError> {
    let stats = db::get_peer_stats(&state.db_pool).await?;
    Ok(Json(stats))
}
