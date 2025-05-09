use chrono::Utc;
use sqlx::{postgres::PgPoolOptions, Pool, Postgres};
use uuid::Uuid;

use crate::{
    cli::Cli,
    error::AppError,
    models::{CountryStats, NetworkStats, PeerGeoInfo, PeerNode, PeerStats},
};

/// Initialize the database connection pool
pub async fn init_db(cli: &Cli) -> Result<Pool<Postgres>, AppError> {
    // Get database URL from CLI or environment
    let database_url = match &cli.database_url {
        Some(url) => url.clone(),
        None => std::env::var("DATABASE_URL").map_err(|_| {
            AppError::ConfigError(
                "DATABASE_URL not set in environment or provided via CLI".to_string(),
            )
        })?,
    };

    // Create connection pool
    let pool = PgPoolOptions::new()
        .max_connections(20)
        .connect(&database_url)
        .await
        .map_err(|e| AppError::DbConnectionError(e.to_string()))?;

    // Ensure the database schema exists
    ensure_schema(&pool).await?;

    tracing::info!("Database connection established");
    Ok(pool)
}

/// Create database schema if it doesn't exist
async fn ensure_schema(pool: &Pool<Postgres>) -> Result<(), AppError> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS "PeerNode" (
            "id" UUID PRIMARY KEY,
            "ip" TEXT NOT NULL,
            "network" TEXT NOT NULL,
            "rpc_address" TEXT,
            "country" TEXT,
            "region" TEXT,
            "province" TEXT,
            "state" TEXT,
            "city" TEXT,
            "isp" TEXT,
            "lat" DOUBLE PRECISION,
            "lon" DOUBLE PRECISION,
            "last_seen" TIMESTAMP WITH TIME ZONE NOT NULL,
            "active" BOOLEAN NOT NULL DEFAULT true,
            UNIQUE("ip", "network")
        )
        "#,
    )
    .execute(pool)
    .await
    .map_err(|e| AppError::DbError(e.to_string()))?;

    // Create index on network for faster queries
    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS "PeerNode_network_idx" ON "PeerNode"("network");
        "#,
    )
    .execute(pool)
    .await
    .map_err(|e| AppError::DbError(e.to_string()))?;

    // Create index on country for faster queries
    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS "PeerNode_country_idx" ON "PeerNode"("country");
        "#,
    )
    .execute(pool)
    .await
    .map_err(|e| AppError::DbError(e.to_string()))?;

    tracing::info!("Database schema ensured");
    Ok(())
}

/// Update peer information in the database
pub async fn upsert_peer(pool: &Pool<Postgres>, peer: &PeerGeoInfo) -> Result<(), AppError> {
    // Generate a UUID if not already set
    let id = Uuid::new_v4();
    let now = Utc::now();

    sqlx::query(
        r#"
        INSERT INTO "PeerNode" (
            "id", "ip", "network", "rpc_address", "country", "region", 
            "province", "state", "city", "isp", "lat", "lon", "last_seen", "active"
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
        ON CONFLICT ("ip", "network")
        DO UPDATE SET
            "rpc_address" = $4,
            "country" = $5,
            "region" = $6,
            "province" = $7,
            "state" = $8,
            "city" = $9,
            "isp" = $10,
            "lat" = $11,
            "lon" = $12,
            "last_seen" = $13,
            "active" = $14
        "#,
    )
    .bind(id)
    .bind(&peer.ip)
    .bind(&peer.network)
    .bind(&peer.rpc_address)
    .bind(&peer.country)
    .bind(&peer.region)
    .bind(&peer.province)
    .bind(&peer.state)
    .bind(&peer.city)
    .bind(&peer.isp)
    .bind(peer.lat)
    .bind(peer.lon)
    .bind(&now)
    .bind(true) // active
    .execute(pool)
    .await
    .map_err(|e| AppError::DbError(e.to_string()))?;

    Ok(())
}

/// Batch update peer information in the database
pub async fn upsert_peers_batch(
    pool: &Pool<Postgres>,
    peers: &[PeerGeoInfo],
    network: &str,
) -> Result<(), AppError> {
    if peers.is_empty() {
        return Ok(());
    }

    // First, mark all existing peers for this network as inactive
    sqlx::query(
        r#"
        UPDATE "PeerNode" SET "active" = false WHERE "network" = $1
        "#,
    )
    .bind(network)
    .execute(pool)
    .await
    .map_err(|e| AppError::DbError(e.to_string()))?;

    // Then upsert each peer
    for peer in peers {
        upsert_peer(pool, peer).await?;
    }

    tracing::info!("Updated {} peers for network {}", peers.len(), network);
    Ok(())
}

/// Get all peers from the database
pub async fn get_all_peers(pool: &Pool<Postgres>) -> Result<Vec<PeerNode>, AppError> {
    let peers = sqlx::query_as::<_, PeerNode>(
        r#"
        SELECT * FROM "PeerNode" WHERE "active" = true ORDER BY "network", "ip"
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(|e| AppError::DbError(e.to_string()))?;

    Ok(peers)
}

/// Get peers by network
pub async fn get_peers_by_network(
    pool: &Pool<Postgres>,
    network: &str,
) -> Result<Vec<PeerNode>, AppError> {
    let peers = sqlx::query_as::<_, PeerNode>(
        r#"
        SELECT * FROM "PeerNode" WHERE LOWER("network") = LOWER($1) AND "active" = true ORDER BY "ip"
        "#,
    )
    .bind(network)
    .fetch_all(pool)
    .await
    .map_err(|e| AppError::DbError(e.to_string()))?;

    Ok(peers)
}

/// Get peers by country
pub async fn get_peers_by_country(
    pool: &Pool<Postgres>,
    country: &str,
) -> Result<Vec<PeerNode>, AppError> {
    let peers = sqlx::query_as::<_, PeerNode>(
        r#"
        SELECT * FROM "PeerNode" WHERE "country" = $1 AND "active" = true ORDER BY "network", "ip"
        "#,
    )
    .bind(country)
    .fetch_all(pool)
    .await
    .map_err(|e| AppError::DbError(e.to_string()))?;

    Ok(peers)
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
    .map_err(|e| AppError::DbError(e.to_string()))?;

    // Get active peers
    let active_peers = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT COUNT(*) FROM "PeerNode" WHERE "active" = true
        "#,
    )
    .fetch_one(pool)
    .await
    .map_err(|e| AppError::DbError(e.to_string()))?;

    // Get network statistics
    let networks = sqlx::query_as::<_, NetworkStats>(
        r#"
        SELECT "network", COUNT(*) as "peer_count"
        FROM "PeerNode"
        WHERE "active" = true
        GROUP BY "network"
        ORDER BY "peer_count" DESC
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(|e| AppError::DbError(e.to_string()))?;

    // Get country statistics
    let countries = sqlx::query_as::<_, CountryStats>(
        r#"
        SELECT "country" as "country", COUNT(*) as "peer_count"
        FROM "PeerNode"
        WHERE "active" = true AND "country" IS NOT NULL
        GROUP BY "country"
        ORDER BY "peer_count" DESC
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(|e| AppError::DbError(e.to_string()))?;

    // Get last update time as a string first and then parse it
    let last_update = sqlx::query_scalar::<_, chrono::DateTime<Utc>>(
        r#"
        SELECT MAX("last_seen") FROM "PeerNode"
        "#,
    )
    .fetch_one(pool)
    .await
    .map_err(|e| AppError::DbError(e.to_string()))?;

    Ok(PeerStats {
        total_peers,
        active_peers,
        networks,
        countries,
        last_update,
    })
}
