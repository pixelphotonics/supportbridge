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