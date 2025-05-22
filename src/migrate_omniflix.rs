use dotenv::dotenv;
use std::env;

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

#[tokio::main]
async fn main() {
    // Load environment variables
    dotenv().ok();

    // Setup logging
    env_logger::init_from_env(env_logger::Env::new().default_filter_or("info"));

    // Get database URL from environment
    let database_url = env::var("DATABASE_URL").expect("DATABASE_URL must be set in environment");

    println!("Connecting to database...");

    // Create database connection pool
    let pool = match db::create_pool(&database_url).await {
        Ok(pool) => {
            println!("Database connection established");
            pool
        }
        Err(e) => {
            eprintln!("Failed to connect to database: {}", e);
            return;
        }
    };

    // Run the migration
    println!("Running Omniflixhub to Omniflix migration...");
    match db::migrate_omniflixhub_to_omniflix(&pool).await {
        Ok(count) => {
            println!("Migration successful! Updated {} records", count);
        }
        Err(e) => {
            eprintln!("Migration failed: {}", e);
        }
    }

    println!("Done!");
}
