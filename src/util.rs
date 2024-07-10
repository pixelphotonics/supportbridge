use std::net::SocketAddr;

use anyhow::Result;
use tungstenite::http::Uri;

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

pub fn build_uri(address: &str, cmd: &str, name: &str, token: Option<&str>) -> Result<Uri> {
    let path_and_query: String = format!("/{}?name={}", cmd, name);

    let uri = Uri::builder()
        .scheme("ws")
        .authority(address)
        .path_and_query("/")

    

        .build()?;
    Ok(uri)
}