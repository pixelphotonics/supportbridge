use std::collections::HashMap;
use anyhow::Result;
use tungstenite::http::Uri;
use std::sync::Arc;
use futures::{SinkExt, StreamExt};
use tokio::sync::Mutex;
use tokio::net::{TcpListener, TcpStream};
use log::{debug, info};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;


pub async fn bridge(server: Uri, exposed: Uri) -> Result<()> {
    let (ws_server_stream, _) = tokio_tungstenite::connect_async(&server).await?;
    let (ws_exposer_stream, _) = tokio_tungstenite::connect_async(&exposed).await?;

    // Relay all messages from server to exposer and vice versa
    let (mut ws_server_out, mut ws_server_in) = ws_server_stream.split();
    let (mut ws_exposer_out, mut ws_exposer_in) = ws_exposer_stream.split();

    let task_1 = tokio::spawn(async move {
        while let Some(msg) = ws_server_in.next().await {
            let msg = msg.expect("Failed to get message");
            log::debug!("server -> exposer: {} bytes", msg.len());
            ws_exposer_out.send(msg).await.expect("Failed to send to exposer");
        }
    });

    let task_2 = tokio::spawn(async move {
        while let Some(msg) = ws_exposer_in.next().await {
            let msg = msg.expect("Failed to get message");
            log::debug!("exposer -> server: {} bytes", msg.len());
            ws_server_out.send(msg).await.expect("Failed to send to server");
        }
    });

    // join tasks
    task_1.await?;
    task_2.await?;

    Ok(())
}