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
    let (mut ws_server_out, mut ws_server_in) = ws_server_stream.split();
    let (mut ws_exposer_out, mut ws_exposer_in) = ws_exposer_stream.split();

    let task_1: JoinHandle<Result<()>> = tokio::spawn(async move {
        while let Some(msg) = ws_server_in.next().await {
            let msg = msg?;
            log::debug!("server -> exposer: {} bytes", msg.len());
            ws_exposer_out.send(msg).await?;
        }

        Ok(())
    });

    let task_2: JoinHandle<Result<()>> = tokio::spawn(async move {
        while let Some(msg) = ws_exposer_in.next().await {
            let msg = msg?;
            log::debug!("exposer -> server: {} bytes", msg.len());
            ws_server_out.send(msg).await?;
        }

        Ok(())
    });

    // join tasks
    task_1.await??;
    task_2.await??;

    Ok(())
}