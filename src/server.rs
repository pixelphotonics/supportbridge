use std::collections::HashMap;
use anyhow::Result;
use std::sync::Arc;
use futures::{SinkExt, StreamExt};
use tokio::sync::Mutex;
use tokio::net::{TcpListener, TcpStream};
use log::{debug, info};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;


pub struct ServerConfig {
    pub listen_addr: core::net::SocketAddr,
}

pub struct TunnelServer {
    pub channels: HashMap<String, ()>,
}

pub struct Channel {

}

struct CallbackHandler {

}

impl tungstenite::handshake::server::Callback for &mut CallbackHandler {
    fn on_request(
        self,
        request: &tungstenite::handshake::server::Request,
        response: tungstenite::handshake::server::Response
    ) -> std::result::Result<tungstenite::handshake::server::Response, tungstenite::handshake::server::ErrorResponse> {
        log::info!("URI: {}", request.uri());
        
        Ok(response)
    }
}


async fn handle_connection(listen_stream: TcpStream) -> Result<()> {
    let mut callback_handler = CallbackHandler{};
    let ws_stream = tokio_tungstenite::accept_hdr_async(listen_stream, &mut callback_handler).await?;
    let (mut ws_out, ws_in) = ws_stream.split();

    Ok(())
    
}

pub async fn serve(config: ServerConfig) -> Result<()> {
    let listener = TcpListener::bind(config.listen_addr).await.expect("Can't listen");

    while let Ok((stream, _)) = listener.accept().await {
        let peer = stream.peer_addr().expect("connected streams should have a peer address");
        info!("Peer address: {}", peer);

        tokio::spawn(handle_connection(stream));
    }

    Ok(())
}