use axum::{
    routing::{get, post},
    Router,
};
use clap::Parser;
use dotenv::dotenv;
use std::net::SocketAddr;
use tower_http::cors::{Any, CorsLayer};
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

mod api;
mod cli;
mod config;
mod db;
mod error;
mod models;
mod peer_discovery;

use cli::Cli;
use error::AppError;

#[tokio::main]
async fn main() -> Result<(), AppError> {
    // Load environment variables from .env file
    dotenv().ok();

    // Setup logging
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .expect("Failed to set global default subscriber");

    // Parse command line arguments
    let cli = Cli::parse();

    // Initialize database connection
    let db_pool = db::init_db(&cli).await?;

    // Setup shared state
    let app_state = models::AppState {
        db_pool: db_pool.clone(),
        config: config::Config::from_env()?,
    };

    // Start peer discovery service in background
    let discovery_handle = tokio::spawn(peer_discovery::start_discovery_service(
        db_pool,
        cli.clone(),
    ));

    // Setup API routes
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/health", get(api::health_check))
        .route("/peers", get(api::get_peers))
        .route("/peers/network/:network", get(api::get_peers_by_network))
        .route("/peers/country/:country", get(api::get_peers_by_country))
        .route("/peers/scan/now", post(api::trigger_scan))
        .route("/peers/stats", get(api::get_stats))
        .layer(cors)
        .with_state(app_state);

    // Start the server
    let addr = SocketAddr::from(([0, 0, 0, 0], cli.port));
    info!("Starting server on {}", addr);

    // Use tokio's built-in TCP adapter
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();

    // The server will run until an error occurs or it's shut down
    // The discovery service will run in the background and we won't await it
    axum::serve(listener, app)
        .await
        .map_err(|e| AppError::ServerError(e.to_string()))?;

    // We don't actually expect to get here, but if we do:
    info!("Server shut down, cancelling discovery service");
    discovery_handle.abort();

    Ok(())
}
