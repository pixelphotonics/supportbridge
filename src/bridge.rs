use anyhow::Result;
use futures::{SinkExt, StreamExt};
use tokio::task::JoinHandle;

use crate::{protocol::ServerPath, util::build_request};


pub async fn bridge(server_addr: String, peer_name: String, exposed: String) -> Result<()> {
    let cmd = ServerPath::Register { name: peer_name };
    let request = build_request(&server_addr, cmd)?;

    let (ws_server_stream, _) = tokio_tungstenite::connect_async(request).await?;
    log::info!("Connected to server: {}", server_addr);

    let exposed_addr = format!("ws://{}", exposed);
    let (ws_exposer_stream, _) = tokio_tungstenite::connect_async(&exposed_addr).await?;
    log::info!("Connected to exposed address: {}", exposed);

    // Relay all messages from server to exposer and vice versa
    let (ws_server_out, ws_server_in) = ws_server_stream.split();
    let (ws_exposer_out, ws_exposer_in) = ws_exposer_stream.split();

    let (task1, task2) = crate::ws_bridge(
        Box::new(ws_exposer_in),
        Box::new(ws_exposer_out),
        Box::new(ws_server_in),
        Box::new(ws_server_out),
    )?;

    task1.await??;
    task2.await??;

    Ok(())
}