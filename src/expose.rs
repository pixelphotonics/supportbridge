use anyhow::Result;
use futures::{Sink, SinkExt, Stream, StreamExt};
use log::{debug, info};
use tungstenite::Message;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

type WsError = tungstenite::error::Error;
type WsResult = std::result::Result<Message, WsError>;

use crate::client::connect_to_server;
use crate::protocol::ServerPath;
use crate::util::{spawn_guarded, GuardedJoinHandle};
use crate::WriteBinary;

struct MutexTcpSender(Arc<Mutex<Option<tokio::net::tcp::OwnedWriteHalf>>>);

impl WriteBinary for MutexTcpSender {
    async fn write_binary(&mut self, data: &[u8]) -> Result<()> {
        if let Some(target_write) = self.0.lock().await.as_mut() {
            log::debug!("Forwarding message to target, {} bytes", data.len());
            target_write.write_all(data).await?;
        } else {
            log::warn!("No target socket available. Init message required.");
        }

        Ok(())
    }
}


async fn handle_connection<WS>(ws_stream: WS, target_addr: SocketAddr) -> Result<()>
where 
    WS: Sink<Message, Error = WsError>
        + Stream<Item = WsResult>
        + std::marker::Send
        + 'static
{
    let (ws_out, ws_in) = ws_stream.split();

    let ws_out_mut = Arc::new(Mutex::new(ws_out));

    let target_write: Arc<Mutex<Option<tokio::net::tcp::OwnedWriteHalf>>> = Arc::new(Mutex::new(None));
    let target_write_clone = MutexTcpSender(target_write.clone());
    let mut task_tcp_to_ws: Option<GuardedJoinHandle<Result<()>>> = None;

    let (txt_send, mut txt_receive) = futures::channel::mpsc::channel::<String>(32);

    let handle_txt_task: GuardedJoinHandle<anyhow::Result<()>> = spawn_guarded(async move {
        while let Some(textmsg) = txt_receive.next().await {
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
                    let (target_read, target_write_half) = new_socket.into_split();
                    tcp_out_lock.replace(target_write_half);

                    task_tcp_to_ws = Some(crate::tcp_to_ws(target_read, ws_out_lock));
                },
                _ => {
                    log::warn!("Unknown message: {}",textmsg);
                }
            }
        }

        Ok(())
    });

    let ws_to_tcp_task = crate::ws_to_tcp(Box::new(ws_in), target_write_clone, Some(txt_send));

    ws_to_tcp_task.await??;
    handle_txt_task.await??;

    Ok(())
}

/// Listen on the given bind address and expose the target_addr via a websocket connection.
/// In order to connect the exposer with the server, a third-party relay needs to be used.
pub async fn listen_to_ws(bind: SocketAddr, target_addr: SocketAddr) -> Result<()> {
    let listener = TcpListener::bind(bind).await?;
    info!("Exposing {} to {}", target_addr, bind);

    while let Ok((stream, _)) = listener.accept().await {
        let peer = stream.peer_addr()?;
        info!("Peer address: {}", peer);
        let ws_stream = tokio_tungstenite::accept_async(stream).await?;

        tokio::spawn(handle_connection(ws_stream, target_addr));
    }

    Ok(())
}

/// Expose the target_addr and directly connect to the server and register this exposer under the given name.
/// This can be used as long as the exposer can directly connect to the server and is not within a protected network.
pub async fn expose_and_register(ws_server: String, target_addr: SocketAddr, name: String) -> Result<()> {
    let ws_stream = connect_to_server(ws_server, ServerPath::Register { name }).await?;
    handle_connection(ws_stream, target_addr).await?;

    Ok(())
}
