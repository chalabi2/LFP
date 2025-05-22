#!/bin/bash

# Run the peer finder with Redis and database connections
export REDIS_URL="redis://localhost:6379"
export DATABASE_URL="postgresql://chalabi:password@localhost:5432/peerfinder"
export RUST_LOG=info,lfp=debug,tokio=info,sqlx=warn,crate::discovery=trace

# Run the application with optimized settings for recursive discovery
echo "Running peerFinder with Redis cache and database connections for all networks"
./target/release/lfp \
  --port 8080 \
  --max-peers 10000 \
  --max-depth 5 \
  --continuous \
  --scan-on-startup \
  --max-concurrent-requests 30 \
  --concurrent-chains 3 \
  --request-timeout 15 \
  --scan-interval 3600

