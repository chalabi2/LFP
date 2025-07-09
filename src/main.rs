use actix_web::{web, App, HttpServer};
use clap::Parser;
use dotenv::dotenv;
use std::env;
use tokio::signal;
use tokio::sync::broadcast;

mod api;
mod cache;
mod cli;
mod config;
mod db;
mod discovery;
mod error;
mod models;
mod peer_discovery;
mod redis_cache;
mod utils;

use cli::Cli;

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // Initialize environment
    dotenv().ok();

    // Initialize logging
    env_logger::init_from_env(env_logger::Env::new().default_filter_or("info"));

    // Parse command-line arguments
    let cli = Cli::parse();

    // Create shutdown signal channel
    let (shutdown_tx, _) = broadcast::channel(1);

    // Configure database cache with environment variables
    let cache_ttl = env::var("CACHE_TTL")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(3600); // Default 1 hour

    let cache_refresh_interval = env::var("CACHE_REFRESH_INTERVAL")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(3600); // Default 1 hour

    if let Err(e) = cache::configure_cache(Some(cache_ttl), Some(cache_refresh_interval)).await {
        tracing::warn!("Failed to configure cache: {}", e);
    } else {
        tracing::info!(
            "Cache configured with TTL: {}s, Refresh: {}s",
            cache_ttl,
            cache_refresh_interval
        );
    }

    // Get database URL from CLI or environment
    let database_url = cli
        .database_url
        .clone()
        .or_else(|| env::var("DATABASE_URL").ok());

    // Setup Redis if available
    let redis_url = env::var("REDIS_URL").ok();
    if let Some(url) = redis_url.clone() {
        if let Err(e) = redis_cache::init_redis_cache(&url).await {
            tracing::warn!("Failed to initialize Redis cache: {}", e);
            tracing::info!("Continuing without Redis cache");
        } else {
            tracing::info!("Redis cache initialized successfully");
        }
    } else {
        tracing::info!("No REDIS_URL provided, running without Redis cache");
    }

    // Setup database pool if URL is available
    let db_pool = if let Some(db_url) = database_url {
        match db::create_pool(&db_url).await {
            Ok(pool) => {
                tracing::info!("Database connection established");

                // Run migrations
                if let Err(e) = db::run_migrations(&pool).await {
                    tracing::error!("Failed to run database migrations: {}", e);
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!("Database migration failed: {}", e),
                    ));
                }

                Some(pool)
            }
            Err(e) => {
                tracing::warn!("Failed to connect to database: {}", e);
                tracing::info!("Running in memory-only mode (no database persistence)");
                None
            }
        }
    } else {
        tracing::info!("No DATABASE_URL provided, running in memory-only mode");
        None
    };

    // Collect background task handles to wait for them during shutdown
    let mut task_handles = Vec::new();

    // Start peer discovery service if database is available
    if let Some(pool) = db_pool.clone() {
        let peer_discovery_cli = cli.clone();
        // Create a new receiver for this task
        let shutdown_rx = shutdown_tx.subscribe();

        // Start in a separate task
        let handle = tokio::spawn(async move {
            if let Err(e) =
                peer_discovery::spawn_discovery_service(pool, peer_discovery_cli, Some(shutdown_rx))
                    .await
            {
                tracing::error!("Peer discovery service failed: {}", e);
            }
        });
        task_handles.push(handle);
    } else if cli.scan_on_startup || cli.continuous {
        // If we want to run without DB, use a special in-memory discovery mode
        tracing::info!("Starting peer discovery in memory-only mode");
        let peer_discovery_cli = cli.clone();
        // Create a new receiver for this task
        let shutdown_rx = shutdown_tx.subscribe();

        let handle = tokio::spawn(async move {
            if let Err(e) =
                peer_discovery::spawn_memory_only_discovery(peer_discovery_cli, Some(shutdown_rx))
                    .await
            {
                tracing::error!("In-memory peer discovery failed: {}", e);
            }
        });
        task_handles.push(handle);
    }

    // Start the web server
    let cli_for_server = cli.clone(); // Clone for port binding
    let cli_for_closure = cli.clone(); // Clone for use inside closure
    let server = HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(cli_for_closure.clone()))
            // Add database pool if available
            .app_data(web::Data::new(db_pool.clone()))
            // Add routes
            .configure(api::configure)
    })
    .bind(("0.0.0.0", cli_for_server.port))?
    .run();

    tracing::info!("Server started at http://0.0.0.0:{}", cli.port);

    // Handle graceful shutdown
    let server_handle = server.handle();
    let shutdown_tx_for_signal = shutdown_tx.clone();

    // Spawn a background task to handle shutdown signal
    tokio::spawn(async move {
        // Wait for CTRL+C signal
        if let Err(err) = signal::ctrl_c().await {
            tracing::error!("Error handling shutdown signal: {}", err);
            return;
        }

        tracing::info!("Received shutdown signal, beginning graceful shutdown...");

        // Broadcast shutdown signal to all background tasks
        if let Err(e) = shutdown_tx_for_signal.send(()) {
            tracing::warn!("Failed to send shutdown signal: {}", e);
        }

        // Stop accepting new connections and wait for existing connections to finish
        server_handle.stop(true).await;
    });

    // Run the server until it completes (which happens after stop is called)
    let server_result = server.await;

    // Wait for all background tasks to complete with a timeout
    tracing::info!("Waiting for background tasks to complete...");
    for (i, handle) in task_handles.into_iter().enumerate() {
        match tokio::time::timeout(tokio::time::Duration::from_secs(5), handle).await {
            Ok(result) => {
                if let Err(e) = result {
                    tracing::warn!("Background task {} ended with error: {}", i, e);
                } else {
                    tracing::debug!("Background task {} completed successfully", i);
                }
            }
            Err(_) => {
                tracing::warn!("Background task {} did not complete within timeout", i);
            }
        }
    }

    tracing::info!("Shutdown complete");
    server_result
}
