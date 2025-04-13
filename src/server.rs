use anyhow::{anyhow, Result};
use futures::{SinkExt, StreamExt};
use log::info;
use tokio::io::AsyncWriteExt;
use std::collections::HashMap;
use std::ops::RangeInclusive;
use std::sync::{Arc, Weak};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, Notify};
use tungstenite::http::Uri;

use crate::protocol::{ChannelId, ExposedAddress, ExposerInfo, JsonMessage, ServerPath};
use crate::util::{spawn_guarded, GuardedAbortHandle, GuardedJoinHandle};

type WsSender = futures::stream::SplitSink<tokio_tungstenite::WebSocketStream<TcpStream>, tungstenite::Message>;
type WsReceiver = futures::stream::SplitStream<tokio_tungstenite::WebSocketStream<TcpStream>>;

pub struct TunnelServer {
    pub tunnels: HashMap<String, Tunnel>,
    options: ServerOptions,
    id_counter: usize,
}

#[derive(Debug, Clone)]
pub struct ServerOptions {
    pub listen_addr: core::net::SocketAddr,
    pub open_port: bool,
    pub port_range: RangeInclusive<u16>,
    pub overwrite_existing_connection: bool,
    pub overwrite_existing_exposer: bool,
}

pub struct Tunnel {
    uid: usize,
    task: GuardedAbortHandle,
    state: Arc<Mutex<TunnelState>>,
}

/// Each Tunnel represents a connection to an exposer.
/// Multiple channels can be created on a single tunnel, e.g.
/// for multiple clients to connect or for multiple ports to be exposed.
pub struct TunnelState {
    info: ExposerInfo,
    server_write: Arc<Mutex<WsSender>>,    
    ports: Vec<ExposedPort>,
    channels: HashMap<u8, Channel>,
}

struct ExposedPort {
    exposed_addr: ExposedAddress,
    listen_task: GuardedJoinHandle<Result<()>>,
}

struct Channel {
    id: u8,
    tcp_sender: tokio::net::tcp::OwnedWriteHalf,
    send_task: GuardedAbortHandle,
    open_success: Arc<Notify>,
    exposed_addr: ExposedAddress,
}

struct CallbackHandler {
    uri: Option<Uri>,
}

impl tungstenite::handshake::server::Callback for &mut CallbackHandler {
    fn on_request(
        self,
        request: &tungstenite::handshake::server::Request,
        response: tungstenite::handshake::server::Response,
    ) -> std::result::Result<
        tungstenite::handshake::server::Response,
        tungstenite::handshake::server::ErrorResponse,
    > {
        log::info!("URI: {}", request.uri());

        self.uri = Some(request.uri().clone());

        Ok(response)
    }
}

async fn open_tcp_listener(port_range: RangeInclusive<u16>) -> Result<TcpListener> {
    log::debug!("Open port in range: {:?}", port_range);
    for port in port_range.clone() {
        log::debug!("Trying port: {}", port);
        let listener = TcpListener::bind(("::", port)).await;
        match listener {
            Ok(listener) => {
                log::info!("Opened port: {}", port);
                return Ok(listener);
            }
            Err(e) => {
                log::debug!("Failed to open port: {}", e);
            }
        }
    }

    // If we reach here, no port was available
    Err(anyhow::anyhow!(
        "No available ports in range: {:?}",
        port_range
    ))
}


async fn serve_channel(tcp_read: tokio::net::tcp::OwnedReadHalf, channel_id: ChannelId, exposed_address: ExposedAddress, ws_out: Arc<Mutex<WsSender>>, open_notify: Arc<Notify>) -> Result<()> {
    ws_out
        .clone()
        .lock_owned()
        .await
        .send(JsonMessage::OpenChannel { channel_id, exposed_address }.encode_ws())
        .await?;

    open_notify.notified().await;

    // Forward all traffic from the TCP port to the websocket
    crate::tcp_to_ws_encoded(channel_id, tcp_read, ws_out).await??;

    Ok(())
}

async fn get_tunnel_lock(tunnel: &Weak<Mutex<TunnelState>>) -> Result<tokio::sync::OwnedMutexGuard<TunnelState>> {
    Ok(tunnel
        .upgrade()
        .ok_or(anyhow::anyhow!("Tunnel closed"))?
        .lock_owned()
        .await)
}

async fn create_channel(stream: TcpStream, tunnel: Weak<Mutex<TunnelState>>, exposed_address: ExposedAddress) -> Result<()> {
    let mut tunnel_lock = get_tunnel_lock(&tunnel).await?;

    let ws_out = tunnel_lock.server_write.clone();
    let (tcp_read, tcp_write) = stream.into_split();
    let open_success = Arc::new(Notify::new());

    // Find free channel id
    let channel_id = (0..ChannelId::MAX).find(|id| tunnel_lock.channels.contains_key(id)).ok_or_else(|| anyhow!("All channels occupied."))?;

    let channel_task = spawn_guarded(serve_channel(tcp_read, channel_id, exposed_address.clone(), ws_out, open_success.clone()));

    let channel = Channel {
        id: channel_id,
        tcp_sender: tcp_write,
        send_task: channel_task.guarded_abort_handle(),
        open_success,
        exposed_addr: exposed_address,
    };
    
    tunnel_lock
        .channels
        .insert(channel_id, channel);

    // clean up once the task is done
    tokio::spawn(async move {
        let _result = channel_task.await;
        log::info!("Channel task finished");
        if let Ok(mut tunnel) = get_tunnel_lock(&tunnel).await {
            // If the WS socket to the exposer is still intact, let the exposer know that the channel is closed.
            let _ = tunnel.server_write.lock().await.send(JsonMessage::CloseChannel { channel_id }.encode_ws()).await;
            tunnel.channels.remove(&channel_id);
        }
    });

    Ok(())
}


async fn listen_tcp_port(port_range: RangeInclusive<u16>, tunnel: Weak<Mutex<TunnelState>>, exposed_address: ExposedAddress) -> Result<()> {
    let listener = open_tcp_listener(port_range).await?;
    while let Ok((stream, _)) = listener.accept().await {
        let peer = stream.peer_addr()?;
        info!("Peer address: {}", peer);

        match create_channel(stream, tunnel.clone(), exposed_address.clone()).await {
            Ok(_) => {}
            Err(e) => {
                log::error!("Error handling connection: {:?}", e);
            }
        }
    }

    Ok(())
}


async fn serve_tunnel(mut ws_in: WsReceiver, tunnel: Weak<Mutex<TunnelState>>, options: ServerOptions) -> Result<()> {
    // Register and wait for "ok" from exposer
    {
        let tunnel_lock = get_tunnel_lock(&tunnel).await?;
        tunnel_lock.server_write.lock().await.send(JsonMessage::OpenTunnel {  }.encode_ws()).await?;
    }

    // Process incoming messages
    while let Some(msg) = ws_in.next().await {
        match msg {
            Ok(tungstenite::Message::Text(msg_text)) => {
                let msg_inner = serde_json::from_str(msg_text.as_str())?;
                match msg_inner {
                    JsonMessage::OpenTunnelSuccess { exposed } => {
                        log::info!("Exposer opened tunnel for ports: {:?}", exposed);
                        let mut tunnel_lock = get_tunnel_lock(&tunnel).await?;

                        for exposed_addr in exposed {
                            let listen_task = spawn_guarded(listen_tcp_port(
                                options.port_range.clone(),
                                tunnel.clone(),
                                exposed_addr.clone(),
                            ));

                            tunnel_lock.ports.push(ExposedPort {
                                exposed_addr,
                                listen_task,
                            });
                        }
                    },
                    JsonMessage::OpenChannelSuccessful { channel_id } => {
                        log::info!("Channel opened: {}", channel_id);
                        let mut tunnel_lock = get_tunnel_lock(&tunnel).await?;
                        if let Some(channel) = tunnel_lock.channels.get_mut(&channel_id) {
                            channel.open_success.notify_waiters();
                        } else {
                            log::error!("Channel not found: {}", channel_id);
                        }
                    },
                    JsonMessage::CloseChannel { channel_id } => {
                        log::info!("Channel closed: {}", channel_id);
                        let mut tunnel_lock = get_tunnel_lock(&tunnel).await?;
                        if let Some(channel) = tunnel_lock.channels.get_mut(&channel_id) {
                            channel.send_task.abort();
                        }
                        else {
                            // If the channel is not in the list, we don't consider this an error.
                        }
                    },
                    _ => {}
                }
            }
            Ok(tungstenite::Message::Binary(encoded_msg)) => {
                if let Some(msg) = crate::protocol::BinaryMessage::from_ws(&encoded_msg[..]) {
                    let mut tunnel_lock = get_tunnel_lock(&tunnel).await?;
                    if let Some(channel) = tunnel_lock.channels.get_mut(&msg.channel_id) {
                        if let Err(_) = channel.tcp_sender.write_all(msg.data).await {
                            log::error!("Error writing to TCP stream, dropping channel.");
                            channel.send_task.abort();
                        }
                    } else {
                        return Err(anyhow!("Unknown channel id: {}", msg.channel_id));
                    }
                } else {
                    log::warn!("Invalid binary message");
                }
            }
            Err(e) => {
                log::error!("Error reading message: {}", e);
            },
            _ => {
                log::trace!("Unhandled websocket frame");
            }
        }
    }

    Ok(())

}


async fn open_tunnel(
    server: Arc<Mutex<TunnelServer>>,
    ws_stream: tokio_tungstenite::WebSocketStream<TcpStream>,
    peer_addr: String,
    name: String,
) -> Result<()>
{
    log::debug!("Register server: '{}'", &name);
    let mut server_state = server.clone().lock_owned().await;

    server_state.id_counter += 1;
    let new_tunnel_id = server_state.id_counter;

    if let Some(tunnel) = server_state.tunnels.remove(&name) {
        let tunnel_state = tunnel.state.lock().await;
        println!(
            "Dropping existing tunnel: {}, {}",
            tunnel_state.info.name, tunnel_state.info.peer_addr
        );
        // Dropping `tunnel` will close the connection, as the guarded abort handle is dropped
    }

    // Create new channel
    let (ws_out, ws_in) = ws_stream.split();
    let tunnel_state = Arc::new(Mutex::new(TunnelState {
        info: ExposerInfo {
            name: name.clone(),
            open_time: chrono::Utc::now()
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            connected_client: None,
            peer_addr,
            open_port: None,
        },
        server_write: Arc::new(Mutex::new(ws_out)),
        channels: HashMap::new(),
        ports: Vec::new(),
    }));

    let task = spawn_guarded(serve_tunnel(
        ws_in,
        Arc::downgrade(&tunnel_state),
        server_state.options.clone(),
    ));
    
    server_state.tunnels.insert(name.clone(), Tunnel {
        uid: new_tunnel_id,
        task: task.guarded_abort_handle(),
        state: tunnel_state.clone(),
    });

    // clean up once the task is done
    tokio::spawn(async move {
        let _result = task.await;
        log::info!("Tunnel task finished");
        let mut server_state = server.clone().lock_owned().await;
        if let Some(tunnel) = server_state.tunnels.get(&name) {
            if tunnel.uid == new_tunnel_id {
                server_state.tunnels.remove(&name);
            }
        }
    });

    Ok(())
}

async fn handle_connection(
    server: Arc<Mutex<TunnelServer>>,
    listen_stream: TcpStream,
) -> Result<()> {
    let mut callback_handler = CallbackHandler { uri: None };
    let peer_addr = listen_stream
        .peer_addr()
        .map(|a| a.to_string())
        .unwrap_or_default();
    let mut ws_stream =
        tokio_tungstenite::accept_hdr_async(listen_stream, &mut callback_handler).await?;

    let uri = callback_handler
        .uri
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No URI found"))?;
    log::debug!("URI: {}", uri);
    let server_cmd = ServerPath::from_uri(uri)?;

    match server_cmd {
        ServerPath::Register { name } => {
            log::debug!("Register server: '{}'", &name);
            open_tunnel(server, ws_stream, peer_addr, name).await?;
        }
        /*ServerPath::Connect { name } => {
            let mut server_state = server.lock().await;
            if let Some(channel) = server_state.tunnels.get_mut(&name) {
                log::info!("Connect to channel: {}", name);

                let ws_out = channel.server_write.clone();
                let ws_in = channel.server_read.clone();

                let task = spawn_guarded(async move {
                    let (client_write, client_read) = ws_stream.split();
                    let mut ws_out = ws_out.lock_owned().await;
                    ws_out
                        .send(tungstenite::Message::Text("init".into()))
                        .await?;

                    crate::ws_bridge(
                        ws_in.lock_owned().await,
                        ws_out,
                        Box::new(client_read),
                        Box::new(client_write),
                    ).await?;

                    Ok(())
                });

                channel.set_channel_user(task, peer_addr, false, server.clone());
            } else {
                log::error!("Channel not found: {}", name);
                ws_stream.close(None).await?;
            }
        }*/
        ServerPath::List => {
            let server_state = server.lock().await;
            let infos = futures::stream::iter(&server_state.tunnels)
                .then(|(_, tunnel)| async {
                    let tunnel_state = tunnel.state.lock().await;
                    tunnel_state.info.clone()
                })
                .collect::<Vec<_>>()
                .await;

            let data = serde_json::to_string(&infos)?;
            ws_stream.send(tungstenite::Message::Text(data)).await?;
            ws_stream.close(None).await?;
        }
        _ => {
            log::error!("Invalid command: {:?}", server_cmd);
            ws_stream.close(None).await?;
        }
    }

    Ok(())
}

pub async fn serve(options: ServerOptions) -> Result<()> {
    let listener = TcpListener::bind(&options.listen_addr).await?;
    info!("Listening on {}", options.listen_addr);

    let server = Arc::new(Mutex::new(TunnelServer {
        tunnels: HashMap::new(),
        options,
        id_counter: 0,
    }));

    while let Ok((stream, _)) = listener.accept().await {
        let peer = stream.peer_addr()?;
        info!("Peer address: {}", peer);

        match handle_connection(server.clone(), stream).await {
            Ok(_) => {}
            Err(e) => {
                log::error!("Error handling connection: {:?}", e);
            }
        }
    }

    Ok(())
}
