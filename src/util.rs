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

pub fn build_request(address: &str, command: crate::protocol::ServerPath) -> Result<tungstenite::handshake::client::Request> {
    let parsed_url = if address.starts_with("ws://") || address.starts_with("wss://") {
        url::Url::parse(address)?
    } else {
        url::Url::parse(format!("ws://{}", address).as_str())?
    };

    let base_path = parsed_url.path().to_string();
    let path_and_query = if base_path.ends_with('/') {
        format!("{}{}", base_path, command.to_string())
    } else {
        format!("{}/{}", base_path, command.to_string())
    };

    log::info!("URL scheme '{}' authority: '{}', path: '{}'", parsed_url.scheme(), parsed_url.authority(), path_and_query);

    let uri = tungstenite::http::Uri::builder()
        .scheme(parsed_url.scheme())
        .authority(parsed_url.authority())
        .path_and_query(path_and_query)
        .build()
        .unwrap();

    let mut host = parsed_url.host_str().unwrap().to_string();
    if let Some(port) = parsed_url.port() {
        host = format!("{}:{}", host, port);
    }

    let req = tungstenite::handshake::client::Request::builder()
        .method("GET")
        .header("Host", host)
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", tungstenite::handshake::client::generate_key());

    // use username / password info if available
    let req = if parsed_url.username().len() > 0 || parsed_url.password().is_some() {
        use base64::prelude::*;
        let auth = format!("{}:{}", parsed_url.username(), parsed_url.password().unwrap_or(""));
        let auth = format!("Basic {}", BASE64_STANDARD.encode(auth));
        req.header("Authorization", auth)
    } else {
        req
    };

    let req = req
        .uri(uri)
        .body(())?;

    Ok(req)
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_request() {
        let req = build_request("localhost", crate::protocol::ServerPath::List).unwrap();
        assert_eq!(req.uri().to_string(), "ws://localhost/list");

        let req = build_request("localhost:8080", crate::protocol::ServerPath::List).unwrap();
        assert_eq!(req.uri().to_string(), "ws://localhost:8080/list");

        let req = build_request("ws://example.com:8080", crate::protocol::ServerPath::List).unwrap();
        assert_eq!(req.uri().to_string(), "ws://example.com:8080/list");
    }
}