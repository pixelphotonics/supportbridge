use anyhow::{anyhow, Result};
use futures::stream::SplitSink;
use futures::{Sink, SinkExt, Stream, StreamExt};
use tungstenite::Message;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

type WsError = tungstenite::error::Error;
type WsResult = std::result::Result<Message, WsError>;

use crate::protocol::{ChannelId, ExposedAddress, JsonMessage, ServerPath};
use crate::util::GuardedAbortHandle;
use crate::connect_to_server;

struct ExposerChannel {
    tcp_sender: tokio::net::tcp::OwnedWriteHalf,
    send_task: GuardedAbortHandle,
}

struct Exposer<WS> 
where 
    WS: Sink<Message, Error = WsError>
        + Stream<Item = WsResult>
        + std::marker::Send
        + 'static
{
    channels: Arc<Mutex<HashMap<ChannelId, ExposerChannel>>>,
    allowed_targets: Vec<ExposedAddress>,
    out_sender: Arc<Mutex<SplitSink<WS, Message>>>,
}

impl<WS> Exposer<WS>
where  WS: Sink<Message, Error = WsError>
    + Stream<Item = WsResult>
    + std::marker::Send
    + 'static 
{
    async fn open_channel(&mut self, channel_id: ChannelId, exposed_address: ExposedAddress) -> Result<()> {
        // Lookup in the whitelist
        if !self.allowed_targets.iter().any(|allowed| *allowed == exposed_address) {
            return Err(anyhow!("Address / port not allowed."));
        }

        let mut channels = self.channels.clone().lock_owned().await;

        if let Some(existing_channel) = channels.get_mut(&channel_id) {
            existing_channel.send_task.abort();
            return Err(anyhow!("Channel id already in use. Closing channel to prevent mixups."));
        }

        // Open connection
        let target_addr_string = format!("{}:{}", exposed_address.address, exposed_address.port);
        let new_socket = TcpStream::connect(&target_addr_string).await?;
        log::info!("Chanel {}: Open socket to target: {}", channel_id, target_addr_string);
        let (target_read, target_write_half) = new_socket.into_split();

        // Start sending task
        let send_task = crate::tcp_to_ws_encoded(channel_id, target_read, self.out_sender.clone());

        channels.insert(channel_id, ExposerChannel {
            tcp_sender: target_write_half,
            send_task: send_task.guarded_abort_handle(),
        });

        // When the channel is closed, e.g. when the target closes the connection, we need to remove it from the list
        // clean up once the task is done
        let channels = self.channels.clone();
        let out_sender = self.out_sender.clone();
        tokio::spawn(async move {
            let _result = send_task.await;
            log::info!("Channel task finished");
            // If the WS socket to the exposer is still intact, let the exposer know that the channel is closed.
            let _ = out_sender.lock().await.send(JsonMessage::CloseChannel { channel_id, error: None }.into()).await;
            channels.lock().await.remove(&channel_id);
        });

        Ok(())
    }
    
    async fn handle_text_msg(&mut self, msg_text: &str) -> Result<Option<JsonMessage>> {
        let msg_inner = serde_json::from_str(msg_text)?;
        match msg_inner {
            JsonMessage::OpenTunnel { protocol_version } => {
                // Check that the protocol version is compatible
                if protocol_version != crate::protocol::PROTOCOL_VERSION {
                    log::error!("Incompatible protocol version. Expected {}, got {}.", crate::protocol::PROTOCOL_VERSION, protocol_version);
                    return Err(anyhow!("Incompatible protocol version. Expected {}, got {}.", crate::protocol::PROTOCOL_VERSION, protocol_version))
                }

                // Send the list of allowed targets to the server
                let json_msg = JsonMessage::OpenTunnelSuccess {
                    exposed: self.allowed_targets.clone(),
                };
                Ok(Some(json_msg))
            },
            JsonMessage::OpenChannel { channel_id, exposed_address } => {
                match self.open_channel(channel_id, exposed_address).await {
                    Ok(_) => Ok(Some(JsonMessage::OpenChannelSuccessful { channel_id })),
                    Err(e) => {
                        log::error!("Error opening channel: {}", e);
                        Ok(Some(JsonMessage::CloseChannel { channel_id, error: Some(e.to_string()) }))
                    },
                }
            },
            JsonMessage::CloseChannel { channel_id, error } => {
                if let Some(channel) = self.channels.lock().await.get_mut(&channel_id) {
                    // Aborting the task will lead to the clean-up task to run, which removes the channel from the list
                    log::error!("Closing channel {}. Error: {:?}", channel_id, error);
                    channel.send_task.abort();
                }

                Ok(None)
            },
            _ => Err(anyhow!("Invalid message: '{}'", msg_text)),
        }
    }

    async fn handle_binary_msg(&mut self, data: tungstenite::Bytes) -> Result<Option<JsonMessage>> {
        if let Some(msg) = crate::protocol::BinaryMessage::from_ws(&data[..]) {
            if let Some(channel) = self.channels.lock().await.get_mut(&msg.channel_id) {
                channel.tcp_sender.write_all(msg.data).await?;
            } else {
                return Err(anyhow!("Unknown channel id: {}", msg.channel_id));
            }
        } else {
            log::warn!("Invalid binary message");
        }

        Ok(None)
    }
}




async fn handle_connection<WS>(ws_stream: WS, allowed_targets: Vec<ExposedAddress>) -> Result<()>
where 
    WS: Sink<Message, Error = WsError>
        + Stream<Item = WsResult>
        + std::marker::Send
        + 'static
{
    let (ws_out, mut ws_in) = ws_stream.split();

    let mut exposer = Exposer {
        channels: Arc::new(Mutex::new(HashMap::new())),
        allowed_targets,
        out_sender: Arc::new(Mutex::new(ws_out)),
    };

    while let Some(msg) = ws_in.next().await {
        let response = match msg {
            Err(e) => {
                log::error!("Websocket error: {}", e);
                Ok(None)
            },
            Ok(tungstenite::Message::Text(msg_text)) => {
                exposer.handle_text_msg(msg_text.as_str()).await
            },
            Ok(tungstenite::Message::Binary(msg_binary)) => {
                // forward to corresponding channel
                exposer.handle_binary_msg(msg_binary).await
            },
            _ => {
                log::trace!("Unhandled websocket frame");
                Ok(None)
            }
        };

        // If applicable, send the response back to the server
        match response {
            Ok(Some(msg)) => {
                exposer.out_sender.lock().await.send(msg.into()).await?;
            },
            Ok(None) => {
                // No action needed
            },
            Err(e) => {
                log::error!("Error handling message: {}", e);
                let json_err = JsonMessage::Error { message: e.to_string() };
                exposer.out_sender.lock().await.send(json_err.into()).await?;
            }
        }
    }

    Ok(())
}

/// Listen on the given bind address and expose the target_addr via a websocket connection.
/// In order to connect the exposer with the server, a third-party relay needs to be used.
pub async fn listen_to_ws(bind: SocketAddr, allowed_targets: Vec<ExposedAddress>) -> Result<()> {
    let listener = TcpListener::bind(bind).await?;
    log::info!("Exposing to {}", bind);

    while let Ok((stream, _)) = listener.accept().await {
        let peer = stream.peer_addr()?;
        log::info!("Peer address: {}", peer);
        let ws_stream = tokio_tungstenite::accept_async(stream).await?;

        tokio::spawn(handle_connection(ws_stream, allowed_targets.clone()));
    }

    Ok(())
}

/// Expose the target_addr and directly connect to the server and register this exposer under the given name.
/// This can be used as long as the exposer can directly connect to the server and is not within a protected network.
pub async fn expose_and_register(ws_server: String, allowed_targets: Vec<ExposedAddress>, name: String) -> Result<()> {
    let ws_stream = connect_to_server(ws_server, ServerPath::Register { name }).await?;
    handle_connection(ws_stream, allowed_targets).await?;

    Ok(())
}
