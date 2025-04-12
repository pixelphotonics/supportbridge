use anyhow::{anyhow, Result};
use futures::stream::SplitSink;
use futures::{Sink, SinkExt, Stream, StreamExt};
use log::{debug, info};
use tungstenite::Message;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

type WsError = tungstenite::error::Error;
type WsResult = std::result::Result<Message, WsError>;

use crate::client::connect_to_server;
use crate::protocol::{ChannelId, JsonMessage, ServerPath};
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

#[derive(Debug, Clone)]
pub struct AllowedTarget {
    pub host: String,
    pub port: u16,
}

struct ExposerChannel {
    channel: ChannelId,
    tcp_sender: MutexTcpSender,
    send_task: GuardedJoinHandle<Result<()>>,
}

struct Exposer<WS> 
where 
    WS: Sink<Message, Error = WsError>
        + Stream<Item = WsResult>
        + std::marker::Send
        + 'static
{
    channels: HashMap<ChannelId, ExposerChannel>,
    allowed_targets: Vec<AllowedTarget>,
    out_sender: Arc<Mutex<SplitSink<WS, Message>>>,
}

impl<WS> Exposer<WS>
where  WS: Sink<Message, Error = WsError>
    + Stream<Item = WsResult>
    + std::marker::Send
    + 'static 
{
    async fn handle_text_msg(&mut self, msg_text: String) -> Result<()> {
        let msg_inner = serde_json::from_str(msg_text.as_str())?;
        match msg_inner {
            JsonMessage::Open { channel_id, exposed_address, force } => {
                // Lookup in the whitelist
                if !self.allowed_targets.iter().any(|allowed| allowed.host == exposed_address.address && allowed.port == exposed_address.port) {
                    return Err(anyhow!("Address / port not allowed."));
                }

                // Open connection
                let target_addr_string = format!("{}:{}", exposed_address.address, exposed_address.port);
                let new_socket = TcpStream::connect(&target_addr_string).await?;
                info!("Open socket to target: {}", target_addr_string);
                let (target_read, target_write_half) = new_socket.into_split();

                if self.channels.contains_key(&channel_id) {
                    if force.unwrap_or(false) {
                        // Close old channel
                        let old_channel = self.channels.remove(&channel_id).unwrap();
                        old_channel.tcp_sender.0.lock().await.take();
                        old_channel.send_task.await?;
                    } else {
                        return Err(anyhow!("Channel id already in use."));
                    }
                }

                // Acquire new id
                //let channel_id = (0..ChannelId::MAX).find(|id| self.channels.contains_key(id)).ok_or_else(|| anyhow!("All channels occupied."))?;

                // Start sending task
                let send_task = crate::tcp_to_ws_encoded(channel_id, target_read, self.out_sender.clone());

                self.channels.insert(channel_id, ExposerChannel {
                    channel: channel_id,
                    tcp_sender: MutexTcpSender(Arc::new(Mutex::new(Some(target_write_half)))),
                    send_task,
                });

                Ok(())
            },
            JsonMessage::CloseChannel { channel_id } => {
                if let Some(_channel) = self.channels.remove(&channel_id) {
                    // Dropping the channel will close the socket and abort the running task, if needed
                    log::error!("Closing channel {}", channel_id);
                }

                Ok(())
            },
            _ => Err(anyhow!("Invalid message: '{}'", msg_text)),
        }
    }

    async fn handle_binary_msg(&mut self, data: Vec<u8>) -> Result<()> {
        if let Some(msg) = crate::protocol::BinaryMessage::from_ws(&data[..]) {
            if let Some(channel) = self.channels.get_mut(&msg.channel_id) {
                channel.tcp_sender.write_binary(msg.data).await?;
            } else {
                return Err(anyhow!("Unknown channel id: {}", msg.channel_id));
            }
        } else {
            log::warn!("Invalid binary message");
        }

        Ok(())
    }
}




async fn handle_connection<WS>(ws_stream: WS, allowed_targets: Vec<AllowedTarget>) -> Result<()>
where 
    WS: Sink<Message, Error = WsError>
        + Stream<Item = WsResult>
        + std::marker::Send
        + 'static
{
    let (ws_out, mut ws_in) = ws_stream.split();

    let mut exposer = Exposer {
        channels: HashMap::new(),
        allowed_targets,
        out_sender: Arc::new(Mutex::new(ws_out)),
    };

    while let Some(msg) = ws_in.next().await {
        let response = match msg {
            Err(e) => {
                log::error!("Websocket error: {}", e);
                Ok(())
            },
            Ok(tungstenite::Message::Text(msg_text)) => {
                exposer.handle_text_msg(msg_text).await
            },
            Ok(tungstenite::Message::Binary(msg_binary)) => {
                // forward to corresponding channel
                exposer.handle_binary_msg(msg_binary).await
            },
            _ => {
                log::trace!("Unhandled websocket frame");
                Ok(())
            }
        };

        // Handle errors by sending an error message
        if let Err(e) = response {
            log::error!("Error handling message: {}", e);
            let json_err = JsonMessage::Error { message: e.to_string() };
            exposer.out_sender.lock().await.send(json_err.encode_ws()).await?;
        }
    }

    Ok(())
}

/// Listen on the given bind address and expose the target_addr via a websocket connection.
/// In order to connect the exposer with the server, a third-party relay needs to be used.
pub async fn listen_to_ws(bind: SocketAddr, allowed_targets: Vec<AllowedTarget>) -> Result<()> {
    let listener = TcpListener::bind(bind).await?;
    info!("Exposing to {}", bind);

    while let Ok((stream, _)) = listener.accept().await {
        let peer = stream.peer_addr()?;
        info!("Peer address: {}", peer);
        let ws_stream = tokio_tungstenite::accept_async(stream).await?;

        tokio::spawn(handle_connection(ws_stream, allowed_targets.clone()));
    }

    Ok(())
}

/// Expose the target_addr and directly connect to the server and register this exposer under the given name.
/// This can be used as long as the exposer can directly connect to the server and is not within a protected network.
pub async fn expose_and_register(ws_server: String, allowed_targets: Vec<AllowedTarget>, name: String) -> Result<()> {
    let ws_stream = connect_to_server(ws_server, ServerPath::Register { name }).await?;
    handle_connection(ws_stream, allowed_targets).await?;

    Ok(())
}
