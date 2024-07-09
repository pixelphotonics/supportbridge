use std::net::SocketAddr;
use std::sync::Arc;
use futures::{SinkExt, StreamExt};
use tokio::sync::Mutex;
use tokio::net::{TcpListener, TcpStream};
use log::{debug, info};
use anyhow::Result;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;
use tungstenite::http::Uri;

pub struct Client {
    pub target_addr: core::net::SocketAddr,
    pub listen_addr: core::net::SocketAddr,

    pub target_socket: Option<TcpStream>,
    pub listen_socket: Option<TcpStream>,
}

pub struct ClientConnection {
    pub write_half: tokio::net::tcp::OwnedWriteHalf,
    pub close_sender: oneshot::Sender<()>,
}

async fn handle_connection(listen_stream: TcpStream, target_addr: Uri) -> Result<()> {
    let (ws_server_stream, _) = tokio_tungstenite::connect_async(&target_addr).await?;
    let (mut ws_out, mut ws_in) = ws_server_stream.split();

    let (mut tcp_read, mut tcp_write) = listen_stream.into_split();

    // Relay all messages from server to exposer and vice versa
    let task_1 = tokio::spawn(async move {
        while let Some(msg) = ws_in.next().await {
            let msg = msg.expect("Failed to get message");
            if msg.is_binary() {
                tcp_write.write_all(msg.into_data().as_slice()).await.expect("Failed to write to target");
            }
        }
    });

    let task_2 = tokio::spawn(async move {
        loop {
            let mut buf = vec![0; 1024];
            let n = tcp_read.read(buf.as_mut_slice()).await.expect("Failed to read from target");
            if n == 0 {
                break;
            }

            log::debug!("TCP->WS: {} bytes", n);
            ws_out.send(tungstenite::Message::Binary(buf[..n].to_vec())).await.expect("Failed to send to server");
        }
    });

    // join tasks
    task_1.await?;
    task_2.await?;

    Ok(())
}

pub async fn serve(tcp_bind: SocketAddr, ws_server: Uri) -> Result<()> {
    let listener = TcpListener::bind(tcp_bind).await.expect("Can't listen");
    info!("Listening on {}", tcp_bind);

    while let Ok((stream, _)) = listener.accept().await {
        let peer = stream.peer_addr().expect("connected streams should have a peer address");
        info!("Peer address: {}", peer);

        tokio::spawn(handle_connection(stream, ws_server.clone()));
    }

    Ok(())
}