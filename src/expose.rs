use std::net::SocketAddr;
use std::sync::Arc;
use futures::{SinkExt, StreamExt};
use tokio::sync::Mutex;
use tokio::net::{TcpListener, TcpStream};
use log::{debug, info};
use anyhow::Result;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;

pub struct ClientConnection {
    pub write_half: tokio::net::tcp::OwnedWriteHalf,
    pub close_sender: oneshot::Sender<()>,
}

async fn handle_connection(listen_stream: TcpStream, target_addr: SocketAddr) -> Result<()> {
    let ws_stream = tokio_tungstenite::accept_async(listen_stream).await?;
    let (mut ws_out, ws_in) = ws_stream.split();
    
    let (ws_send_sender, mut ws_send_receiver) = tokio::sync::mpsc::channel::<Vec<u8>>(1);

    // Handle messages on ws_send_receiver
    tokio::spawn(async move {
        while let Some(msg) = ws_send_receiver.recv().await {
            if let Err(e) = ws_out.send(tungstenite::Message::Binary(msg)).await {
                log::error!("Error sending to ws: {:?}", e);
                break;
            }
        }
    });

    // Read from the websocket and write to the target socket
    tokio::spawn(async move {
        let target_socket: Arc<Mutex<Option<ClientConnection>>> = Arc::new(Mutex::new(None));

        ws_in
            .for_each(|msg| async {
                if msg.is_err() {
                    log::error!("Error reading from ws: {:?}", msg.err().unwrap());
                    return;
                }

                let msg = msg.unwrap();

                if msg.is_text() {
                    match msg.to_text().unwrap() {
                        "init" => {
                            let target_socket = target_socket.clone();
                            let mut socket_lock = target_socket.lock().await;

                            // Create new socket connection
                            let new_socket = TcpStream::connect(target_addr).await.unwrap();
                            debug!("Open socket to target: {}", target_addr);
                            let (mut target_read, target_write) = new_socket.into_split();

                            let (close_sender, mut close_receiver) = oneshot::channel();
                            
                            let old_connection = socket_lock.replace(ClientConnection{
                                write_half: target_write,
                                close_sender,
                            });

                            if let Some(old_connection) = old_connection {
                                debug!("Close existing target port");
                                _ = old_connection.close_sender.send(());
                            }

                            let wssender = ws_send_sender.clone();

                            tokio::spawn(async move {
                                loop {
                                    let mut buf = vec![0; 1024];
                                    // select on target_read and close_receiver
                                    tokio::select! {
                                        read_result = target_read.read(&mut buf) => {
                                            match read_result {
                                                Ok(n) => {
                                                    if n == 0 {
                                                        break;
                                                    }

                                                    wssender.send(buf[..n].to_vec()).await.expect("Failed to send to ws");
                                                }
                                                Err(e) => {
                                                    debug!("Error reading from target: {:?}", e);
                                                    break;
                                                }
                                            
                                            }
                                        }
                                        _ = &mut close_receiver => {
                                            debug!("Closing connection");
                                        }
                                    }
                                }
                            });
                        },
                        _ => {
                            log::warn!("Unknown message: {}", msg.to_text().unwrap());
                        }
                    }
                } else if msg.is_binary() {
                    // forward to open socket, if any
                    if let Some(socket) = target_socket.lock().await.as_mut() {
                        log::debug!("Forwarding message to target, {} bytes", msg.len());
                        socket.write_half.write(&msg.into_data()).await.expect("Failed to write to target");
                    }
                }
            })
            .await;
    });

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