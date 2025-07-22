// General utility functions for the application
use std::net::IpAddr;
use std::str::FromStr;

/// Validates if a string represents a valid public IP address
pub fn is_valid_public_ip(ip: &str) -> bool {
    // Check if it's a valid IP format
    let parsed_ip = match IpAddr::from_str(ip) {
        Ok(addr) => addr,
        Err(_) => return false,
    };

    // Filter out private, local, and special purpose IPs
    match parsed_ip {
        IpAddr::V4(addr) => {
            let octets = addr.octets();

            // RFC 1918 (Private Use)
            if octets[0] == 10
                || (octets[0] == 172 && (octets[1] >= 16 && octets[1] <= 31))
                || (octets[0] == 192 && octets[1] == 168)
            {
                return false;
            }

            // Loopback, link-local, and other special ranges
            if octets[0] == 0 ||     // This network
               octets[0] == 127 ||   // Loopback
               (octets[0] == 169 && octets[1] == 254) || // Link-local
               (octets[0] == 192 && octets[1] == 0 && octets[2] == 0) || // IETF Protocol
               (octets[0] == 192 && octets[1] == 0 && octets[2] == 2) || // TEST-NET-1
               (octets[0] == 198 && octets[1] == 51 && octets[2] == 100) || // TEST-NET-2
               (octets[0] == 203 && octets[1] == 0 && octets[2] == 113) || // TEST-NET-3
               octets[0] >= 224 ||   // Multicast and reserved
               (octets[0] == 255 && octets[1] == 255 && octets[2] == 255 && octets[3] == 255)
            // Broadcast
            {
                return false;
            }

            true
        }
        IpAddr::V6(_) => {
            // For simplicity, we're allowing all IPv6 addresses that aren't loopback or link-local
            !parsed_ip.is_loopback() && !parsed_ip.is_unspecified()
        }
    }
}

/// Transforms an RPC address from a node_info structure to a usable URL
/// Handles local addresses (0.0.0.0, 127.0.0.1, localhost) by replacing them with the peer's real IP
pub fn transform_rpc_address(rpc_address: &str, peer_ip: &str) -> Option<String> {
    if rpc_address.is_empty() {
        return None;
    }

    // Check if it's a local address that needs IP replacement
    let needs_ip_replacement = rpc_address.contains("0.0.0.0")
        || rpc_address.contains("127.0.0.1")
        || rpc_address.contains("localhost");

    let protocol_added = if needs_ip_replacement {
        // Extract port from address
        let port = extract_port_from_address(rpc_address);

        // For 0.0.0.0 and other local addresses, replace with peer's real IP
        format!("http://{}:{}", peer_ip, port)
    } else {
        // Clean the RPC address format - handle tcp:// protocol better
        let cleaned_url = rpc_address
            .replace("tcp://", "")
            .replace("http://", "")
            .replace("https://", "");

        // If the cleaned URL still contains the peer's IP, just use it with http
        if cleaned_url.contains(peer_ip) {
            format!("http://{}", cleaned_url)
        } else {
            // Extract port and use peer's IP instead of whatever was in the address
            let port = extract_port_from_address(rpc_address);
            format!("http://{}:{}", peer_ip, port)
        }
    };

    // Validate the final URL makes sense
    if !protocol_added.contains(peer_ip) {
        tracing::warn!(
            "Transformed RPC address doesn't contain peer IP: {} -> {}",
            rpc_address,
            protocol_added
        );
    }

    Some(protocol_added)
}

/// Extract port from various RPC address formats
fn extract_port_from_address(address: &str) -> String {
    // First, try standard port extraction with :port format
    if let Some(port) = address.split(':').last() {
        // Handle trailing slashes or paths
        if let Some(clean_port) = port.split('/').next() {
            // Check if it's actually a port number
            if clean_port.parse::<u16>().is_ok() {
                return clean_port.to_string();
            }
        }
    }

    // TCP URLs in Tendermint often have a nonstandard format
    // Examples: "tcp://0.0.0.0:26657", "tcp://localhost:26657"
    if address.contains("tcp://") {
        // Extract port after the last colon, before any slash
        if let Some(port_part) = address.split(':').last() {
            if let Some(port) = port_part.split('/').next() {
                return port.to_string();
            }
        }
    }

    // If we can't extract the port, use the default Tendermint RPC port
    "26657".to_string()
}

// All functions in this file are unused - removing them

// File can be kept for future utilities
