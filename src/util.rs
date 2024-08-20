use std::net::SocketAddr;

use anyhow::Result;

pub async fn parse_address(address: &str) -> Result<SocketAddr> {
    if let Ok(addr) = address.parse() {
        return Ok(addr);
    } else {
        // Try to parse a hostname:port combination
        let mut result = tokio::net::lookup_host(address).await?;
        let result = result.next().ok_or(anyhow::anyhow!("No address found for hostname: {}", address))?;
        Ok(result)
    }
}

pub fn build_url_base(address: &str, use_tls: bool) -> Result<String> {
    let address: String = if address.starts_with("ws://") || address.starts_with("wss://") {
        address.to_string()
    } else {
        let scheme = if use_tls { "wss" } else { "ws" };
        format!("{}://{}", scheme, address)
    };

    if address.ends_with('/') {
        Ok(address)
    } else {
        Ok(format!("{}/", address))
    }
}