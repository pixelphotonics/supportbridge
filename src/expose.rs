use anyhow::Result;
use futures::{SinkExt, StreamExt};
use log::{debug, info};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

use crate::util::{spawn_guarded, GuardedJoinHandle};


async fn handle_connection(listen_stream: TcpStream, target_addr: SocketAddr) -> Result<()> {
    let ws_stream = tokio_tungstenite::accept_async(listen_stream).await?;
    let (ws_out, mut ws_in) = ws_stream.split();

    let ws_out_mut = Arc::new(Mutex::new(ws_out));

    let target_write: Arc<Mutex<Option<tokio::net::tcp::OwnedWriteHalf>>> = Arc::new(Mutex::new(None));
    let mut task_tcp_to_ws: Option<GuardedJoinHandle<Result<()>>> = None;

    while let Some(msg) = ws_in.next().await {
        let msg = msg?;
        match msg {
            tungstenite::Message::Text(textmsg) => {
                match textmsg.as_str() {
                    "init" => {
                        // Cancel old task by clearing the task variable
                        if let Some(task) = task_tcp_to_ws.take() {
                            task.abort();
                        }

                        // Acquire the ws_out lock to ensure that no more messages can be sent to the websocket from any old send task
                        let mut ws_out_lock = ws_out_mut.clone().lock_owned().await;

                        // Send ack_init to the connected client to mark that from this point on no more data from an old connection will be sent
                        ws_out_lock.send(tungstenite::Message::Text("ack_init".to_string())).await?;

                        let mut tcp_out_lock = target_write.clone().lock_owned().await;

                        // Create new socket connection
                        let new_socket = TcpStream::connect(target_addr).await.unwrap();
                        debug!("Open socket to target: {}", target_addr);
                        let (mut target_read, target_write_half) = new_socket.into_split();
                        tcp_out_lock.replace(target_write_half);

                        task_tcp_to_ws = Some(spawn_guarded(async move {
                            loop {
                                let mut buf = vec![0; 1024];
                                let n = target_read.read(buf.as_mut_slice()).await?;
                                if n == 0 {
                                    break;
                                }
                    
                                log::debug!("TCP->WS: {} bytes", n);
                                ws_out_lock
                                    .send(tungstenite::Message::Binary(buf[..n].to_vec()))
                                    .await?;
                            }

                            Ok(())
                        }));
                    },
                    _ => {
                        log::warn!("Unknown message: {}",textmsg);
                    }
                }
            },
            tungstenite::Message::Binary(binmsg) => {
                if let Some(target_write) = target_write.lock().await.as_mut() {
                    log::debug!("Forwarding message to target, {} bytes", binmsg.len());
                    target_write.write(&binmsg).await.expect("Failed to write to target");
                } else {
                    log::warn!("No target socket available. Init message required.");
                }
            },
            _ => {
                log::debug!("Unknown message: {:?}", msg);
            }
        }
    }


    Ok(())
}

pub async fn serve(bind: SocketAddr, tcp_addr: SocketAddr) -> Result<()> {
    let listener = TcpListener::bind(bind).await?;
    info!("Exposing {} to {}", tcp_addr, bind);

    while let Ok((stream, _)) = listener.accept().await {
        let peer = stream.peer_addr()?;
        info!("Peer address: {}", peer);

        tokio::spawn(handle_connection(stream, tcp_addr));
    }

    Ok(())
}
