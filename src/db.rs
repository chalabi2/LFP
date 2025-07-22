use chrono::Utc;
use sqlx::migrate::MigrateDatabase;
use sqlx::{postgres::PgPoolOptions, Pool, Postgres};
use std::time::Duration;

use crate::{
    error::AppError,
    models::{CountryStats, NetworkStats, PeerGeoInfo, PeerNode, PeerStats},
};

/// Batch update peer information in the database
pub async fn upsert_peers_batch(
    pool: &Pool<Postgres>,
    peers: &[PeerGeoInfo],
    network: &str,
) -> Result<(), AppError> {
    if peers.is_empty() {
        tracing::info!("No peers to upsert");
        return Ok(());
    }

    // Generate a batch of statements to upsert peers
    tracing::info!("Upserting {} peers for network {}", peers.len(), network);

    // Process in batches to avoid overwhelming the database
    for chunk in peers.chunks(100) {
        let mut tx = pool
            .begin()
            .await
            .map_err(|e| AppError::DatabaseError(format!("Failed to start transaction: {}", e)))?;

        // Update in batches
        for peer in chunk {
            sqlx::query(
                r#"
                INSERT INTO "PeerNode" (
                    ip, network, rpc_address, country, region, province, state, city, isp, lat, lon, last_seen, active, is_live, node_id, p2p_port
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, NOW(), TRUE, $12, $13, $14)
                ON CONFLICT (ip, network) DO UPDATE SET
                    rpc_address = EXCLUDED.rpc_address,
                    country = EXCLUDED.country,
                    region = EXCLUDED.region,
                    province = EXCLUDED.province,
                    state = EXCLUDED.state,
                    city = EXCLUDED.city,
                    isp = EXCLUDED.isp,
                    lat = EXCLUDED.lat,
                    lon = EXCLUDED.lon,
                    last_seen = CASE 
                        WHEN EXCLUDED.is_live = TRUE THEN NOW() 
                        ELSE COALESCE("PeerNode".last_seen, NOW()) 
                    END,
                    active = TRUE,
                    is_live = EXCLUDED.is_live,
                    node_id = EXCLUDED.node_id,
                    p2p_port = EXCLUDED.p2p_port
                "#
            )
            .bind(&peer.ip)
            .bind(network)
            .bind(&peer.rpc_address)
            .bind(&peer.country)
            .bind(&peer.region)
            .bind(&peer.province)
            .bind(&peer.state)
            .bind(&peer.city)
            .bind(&peer.isp)
            .bind(peer.lat)
            .bind(peer.lon)
            .bind(peer.is_live)
            .bind(&peer.node_id)
            .bind(peer.p2p_port.map(|p| p as i32))
            .execute(&mut *tx)
            .await
            .map_err(|e| AppError::DatabaseError(format!("Failed to upsert peer: {}", e)))?;
        }

        // Commit the transaction
        tx.commit()
            .await
            .map_err(|e| AppError::DatabaseError(format!("Failed to commit transaction: {}", e)))?;
    }

    // Mark peers not in the current batch as inactive
    sqlx::query(
        r#"
        UPDATE "PeerNode"
        SET "active" = false
        WHERE "network" = $1
        AND "last_seen" < NOW() - INTERVAL '15 minutes'
        "#,
    )
    .bind(network)
    .execute(pool)
    .await
    .map_err(|e| AppError::DatabaseError(format!("Failed to mark inactive peers: {}", e)))?;

    Ok(())
}

/// Get all peers from the database
pub async fn get_all_peers(pool: &Pool<Postgres>) -> Result<Vec<PeerNode>, AppError> {
    let peers = sqlx::query_as::<_, PeerNode>(
        r#"
        SELECT * FROM "PeerNode" ORDER BY "network", "ip"
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(|e| AppError::DatabaseError(e.to_string()))?;

    Ok(peers)
}

/// Get peers by network with optional active filter
pub async fn get_peers_by_network_with_filter(
    pool: &Pool<Postgres>,
    network: &str,
    active_filter: Option<bool>,
) -> Result<Vec<PeerNode>, AppError> {
    // Use the exact network name - no special cases needed after migration
    let network_pattern = &network;

    // Use exact matching for all networks
    let comparison_operator = "=";

    // Build the query string dynamically based on the comparison operator
    let query_str = format!(
        r#"
        SELECT * FROM "PeerNode" 
        WHERE LOWER("network") {} LOWER($1) 
        {}
        ORDER BY "ip"
        "#,
        comparison_operator,
        active_filter.map_or("".to_string(), |_| "AND \"active\" = $2".to_string())
    );

    // Build the query depending on whether we have an active filter
    let query = match active_filter {
        Some(active) => {
            // Query with active filter
            sqlx::query_as::<_, PeerNode>(&query_str)
                .bind(network_pattern)
                .bind(active)
        }
        None => {
            // Query without active filter
            sqlx::query_as::<_, PeerNode>(&query_str).bind(network_pattern)
        }
    };

    // Execute the query
    let peers = query
        .fetch_all(pool)
        .await
        .map_err(|e| AppError::DatabaseError(e.to_string()))?;

    tracing::debug!("Found {} peers for network {}", peers.len(), network);
    Ok(peers)
}

/// Get peers by network (without filter)
pub async fn get_peers_by_network(
    pool: &Pool<Postgres>,
    network: &str,
) -> Result<Vec<PeerNode>, AppError> {
    // Simply call the filtered version with no filter
    get_peers_by_network_with_filter(pool, network, None).await
}

/// Get statistics about peers
pub async fn get_peer_stats(pool: &Pool<Postgres>) -> Result<PeerStats, AppError> {
    // Get total peers
    let total_peers = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT COUNT(*) FROM "PeerNode"
        "#,
    )
    .fetch_one(pool)
    .await
    .map_err(|e| AppError::DatabaseError(e.to_string()))?;

    // Get active peers
    let active_peers = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT COUNT(*) FROM "PeerNode" WHERE "active" = true
        "#,
    )
    .fetch_one(pool)
    .await
    .map_err(|e| AppError::DatabaseError(e.to_string()))?;

    // Get network statistics
    let networks = sqlx::query_as::<_, NetworkStats>(
        r#"
        SELECT "network", COUNT(*) as "peer_count"
        FROM "PeerNode"
        GROUP BY "network"
        ORDER BY "peer_count" DESC
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(|e| AppError::DatabaseError(e.to_string()))?;

    // Get country statistics
    let countries = sqlx::query_as::<_, CountryStats>(
        r#"
        SELECT "country" as "country", COUNT(*) as "peer_count"
        FROM "PeerNode"
        WHERE "country" IS NOT NULL
        GROUP BY "country"
        ORDER BY "peer_count" DESC
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(|e| AppError::DatabaseError(e.to_string()))?;

    // Get last update time as a string first and then parse it
    let last_update = sqlx::query_scalar::<_, chrono::DateTime<Utc>>(
        r#"
        SELECT MAX("last_seen") FROM "PeerNode"
        "#,
    )
    .fetch_one(pool)
    .await
    .map_err(|e| AppError::DatabaseError(e.to_string()))?;

    Ok(PeerStats {
        total_peers,
        active_peers,
        networks,
        countries,
        last_update,
    })
}

/// Create a Postgres connection pool
pub async fn create_pool(db_url: &str) -> Result<Pool<Postgres>, AppError> {
    // Check if database exists, if not create it
    if !Postgres::database_exists(db_url).await.unwrap_or(false) {
        tracing::info!("Database does not exist, creating...");
        Postgres::create_database(db_url)
            .await
            .map_err(|e| AppError::DatabaseError(format!("Failed to create database: {}", e)))?;
    }

    // Create connection pool with reasonable settings
    PgPoolOptions::new()
        .max_connections(20)
        .min_connections(1)
        .acquire_timeout(Duration::from_secs(5))
        .connect(db_url)
        .await
        .map_err(|e| AppError::DbConnectionError(e.to_string()))
}

/// Run database migrations
#[allow(dead_code)]
pub async fn run_migrations(pool: &Pool<Postgres>) -> Result<(), AppError> {
    tracing::info!("Running database migrations");

    // Create the PeerNode table if it doesn't exist
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS "PeerNode" (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            ip TEXT NOT NULL,
            network TEXT NOT NULL,
            rpc_address TEXT,
            country TEXT,
            region TEXT,
            province TEXT,
            state TEXT,
            city TEXT,
            isp TEXT,
            lat DOUBLE PRECISION,
            lon DOUBLE PRECISION,
            last_seen TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW(),
            active BOOLEAN NOT NULL DEFAULT TRUE,
            is_live BOOLEAN,
            node_id TEXT,
            p2p_port INTEGER,
            UNIQUE(ip, network)
        )
        "#,
    )
    .execute(pool)
    .await
    .map_err(|e| AppError::MigrationError(format!("Failed to create PeerNode table: {}", e)))?;

    // Add is_live column if not exists
    sqlx::query(
        r#"
        ALTER TABLE "PeerNode"
        ADD COLUMN IF NOT EXISTS is_live BOOLEAN;
        "#,
    )
    .execute(pool)
    .await
    .ok(); // Ignore errors

    // Create index on network for faster queries
    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS idx_peernode_network ON "PeerNode"(network);
        "#,
    )
    .execute(pool)
    .await
    .map_err(|e| AppError::MigrationError(format!("Failed to create network index: {}", e)))?;

    // Create index on country for faster queries
    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS idx_peernode_country ON "PeerNode"(country);
        "#,
    )
    .execute(pool)
    .await
    .map_err(|e| AppError::MigrationError(format!("Failed to create country index: {}", e)))?;

    tracing::info!("Database schema initialized successfully");
    Ok(())
}

/// Get peers by network with optional active filter and pagination
pub async fn _get_peers_by_network_with_filter_and_pagination(
    pool: &Pool<Postgres>,
    network: &str,
    active_filter: Option<bool>,
    page: i64,
    per_page: i64,
) -> Result<(Vec<PeerNode>, i64), AppError> {
    // Calculate offset
    let offset = (page - 1) * per_page;

    // First, get the total count
    let count_query = match active_filter {
        Some(active) => sqlx::query_scalar::<_, i64>(
            r#"
                SELECT COUNT(*) FROM "PeerNode" 
                WHERE LOWER("network") = LOWER($1) 
                AND "active" = $2
                "#,
        )
        .bind(network)
        .bind(active),
        None => sqlx::query_scalar::<_, i64>(
            r#"
                SELECT COUNT(*) FROM "PeerNode" 
                WHERE LOWER("network") = LOWER($1)
                "#,
        )
        .bind(network),
    };

    let total_count = count_query
        .fetch_one(pool)
        .await
        .map_err(|e| AppError::DatabaseError(e.to_string()))?;

    // Then get the paginated results
    let query = match active_filter {
        Some(active) => sqlx::query_as::<_, PeerNode>(
            r#"
                SELECT * FROM "PeerNode" 
                WHERE LOWER("network") = LOWER($1) 
                AND "active" = $2
                ORDER BY "ip"
                LIMIT $3 OFFSET $4
                "#,
        )
        .bind(network)
        .bind(active)
        .bind(per_page)
        .bind(offset),
        None => sqlx::query_as::<_, PeerNode>(
            r#"
                SELECT * FROM "PeerNode" 
                WHERE LOWER("network") = LOWER($1) 
                ORDER BY "ip"
                LIMIT $2 OFFSET $3
                "#,
        )
        .bind(network)
        .bind(per_page)
        .bind(offset),
    };

    let peers = query
        .fetch_all(pool)
        .await
        .map_err(|e| AppError::DatabaseError(e.to_string()))?;

    Ok((peers, total_count))
}

/// Migrate all Omniflixhub records to Omniflix
pub async fn migrate_omniflixhub_to_omniflix(pool: &Pool<Postgres>) -> Result<u64, AppError> {
    tracing::info!("Migrating Omniflixhub records to Omniflix");

    // First, identify conflicting IPs - these are IPs that exist in both networks
    let conflicts = sqlx::query_scalar::<_, String>(
        r#"
        SELECT o.ip 
        FROM "PeerNode" o
        WHERE o."network" = 'Omniflixhub'
        AND EXISTS (
            SELECT 1 
            FROM "PeerNode" f 
            WHERE f."network" = 'Omniflix'
            AND f.ip = o.ip
        )
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(|e| {
        AppError::DatabaseError(format!("Failed to identify conflicting records: {}", e))
    })?;

    tracing::info!(
        "Found {} conflicting IPs that exist in both networks",
        conflicts.len()
    );

    // Delete the conflicting Omniflixhub records
    let deleted = if !conflicts.is_empty() {
        let deleted_result = sqlx::query(
            r#"
            DELETE FROM "PeerNode"
            WHERE "network" = 'Omniflixhub'
            AND ip = ANY($1)
            "#,
        )
        .bind(&conflicts)
        .execute(pool)
        .await
        .map_err(|e| {
            AppError::DatabaseError(format!("Failed to delete conflicting records: {}", e))
        })?;

        let count = deleted_result.rows_affected();
        tracing::info!("Deleted {} conflicting Omniflixhub records", count);
        count
    } else {
        0
    };

    // Update remaining Omniflixhub records to Omniflix
    let updated = sqlx::query(
        r#"
        UPDATE "PeerNode"
        SET "network" = 'Omniflix'
        WHERE "network" = 'Omniflixhub'
        "#,
    )
    .execute(pool)
    .await
    .map_err(|e| {
        AppError::DatabaseError(format!(
            "Failed to migrate remaining Omniflixhub records: {}",
            e
        ))
    })?;

    let updated_count = updated.rows_affected();
    tracing::info!("Updated {} Omniflixhub records to Omniflix", updated_count);

    // Return total affected (deleted + updated)
    Ok(deleted + updated_count)
}
