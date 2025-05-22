# LookingForPeer

A high-performance Rust web server for discovering and tracking blockchain network peers in the Cosmos ecosystem.

## Features

- Fast recursive peer discovery across multiple Cosmos-based networks
- Geographical data enrichment for all discovered peers
- Persistent storage in PostgreSQL database
- REST API for accessing peer information
- Configurable scan depth and frequency
- Support for multiple blockchain networks

## Installation

### Prerequisites

- Rust 1.70 or higher
- PostgreSQL database
- Cargo package manager

### Setup

1. Clone the repository:

   ```bash
   git clone https://github.com/chalabi2/LFP.git
   cd LFP
   ```

2. Create a `.env` file in the project root:

   ```
   # Database connection string
   DATABASE_URL=postgres://username:password@localhost:5432/lfp

   # Network RPC URLs
   NEXT_PUBLIC_GRAVITY_BRIDGE_3_RPC_URL=https://nodes.chandrastation.com/rpc/gravity/
   NEXT_PUBLIC_JUNO_1_RPC_URL=https://nodes.chandrastation.com/rpc/juno/
   NEXT_PUBLIC_OSMOSIS_1_RPC_URL=https://nodes.chandrastation.com/rpc/osmosis/
   NEXT_PUBLIC_OMNIFLIX_1_RPC_URL=https://nodes.chandrastation.com/rpc/omniflix/
   NEXT_PUBLIC_CHIHUAHUA_1_RPC_URL=https://nodes.chandrastation.com/rpc/chihuahua/
   NEXT_PUBLIC_ALTHEA_258432_1_RPC_URL=https://nodes.chandrastation.com/rpc/althea/
   NEXT_PUBLIC_CANTO_7700_1_RPC_URL=https://nodes.chandrastation.com/rpc/canto/
   NEXT_PUBLIC_QUICKSILVER_2_RPC_URL=https://nodes.chandrastation.com/rpc/quicksilver/
   NEXT_PUBLIC_SAGA_1_RPC_URL=https://nodes.chandrastation.com/rpc/saga/
   ```

3. Create the PostgreSQL database:

   ```bash
   createdb lfp
   ```

4. Build the project:
   ```bash
   cargo build --release
   ```

## Usage

### Starting the server

```bash
./target/release/lfp
```

### Command Line Options

```
USAGE:
    lfp [OPTIONS]

OPTIONS:
    -p, --port <PORT>                            Port to run the web server on [default: 3000]
    --database-url <DATABASE_URL>                Database URL for storing peer information
    --scan-interval <SCAN_INTERVAL>              Scan interval in seconds [default: 43200]
    --network <NETWORK>                          Specific blockchain network to scan
    --max-peers <MAX_PEERS>                      Maximum number of peers to discover per network (0 = unlimited) [default: 1000]
    --continuous <CONTINUOUS>                    Enable continuous background scanning [default: true]
    --max-depth <MAX_DEPTH>                      Maximum depth for recursive peer discovery (0 = unlimited) [default: 0]
    --request-timeout <REQUEST_TIMEOUT>          Timeout for HTTP requests in seconds [default: 5]
    --scan-on-startup <SCAN_ON_STARTUP>          Run an immediate scan on startup [default: true]
    --max-concurrent-requests <MAX_CONCURRENT>   The maximum number of concurrent requests [default: 100]
    -h, --help                                   Print help information
    -V, --version                                Print version information
```

### API Endpoints

- `GET /health` - Health check endpoint
- `GET /peers` - Get all peers
- `GET /peers/network/:network` - Get peers by network
- `GET /peers/country/:country` - Get peers by country
- `POST /peers/scan/now` - Trigger an immediate scan
- `GET /peers/stats` - Get peer statistics

## Architecture

lfp works by:

1. Starting with a known RPC node for each blockchain network
2. Fetching peer information from that node
3. Recursively following each peer's RPC address to discover more peers
4. Enriching peer data with geographical information
5. Storing the results in a PostgreSQL database
6. Exposing the data through a REST API
