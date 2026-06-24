use anyhow::{anyhow, Result};
use futures::{SinkExt, StreamExt};
use log::info;
use tokio::io::AsyncWriteExt;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::sync::{Arc, Weak};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, Notify};

use crate::protocol::{ChannelId, ChannelInfo, ExposedAddress, ExposedServerPort, JsonMessage, TunnelInfo};
use crate::util::{now, spawn_guarded, GuardedAbortHandle, GuardedJoinHandle};

type Message = axum::extract::ws::Message;
type WsSender = futures::stream::SplitSink<axum::extract::ws::WebSocket, Message>;
type WsReceiver = futures::stream::SplitStream<axum::extract::ws::WebSocket>;

pub struct TunnelServer {
    pub tunnels: HashMap<String, Tunnel>,
    options: ServerOptions,
    id_counter: usize,
}

#[derive(Debug, Clone)]
pub struct ServerOptions {
    pub listen_addr: core::net::SocketAddr,
    pub port_range: RangeInclusive<u16>,
    pub root_file: Option<PathBuf>,
}

pub struct Tunnel {
    uid: usize,

    /// This is stored in the struct for the purpose of aborting it when the tunnel is closed (RAII).
    _task: GuardedAbortHandle,
    
    state: Arc<Mutex<TunnelState>>,
}

/// Each Tunnel represents a connection to an exposer.
/// Multiple channels can be created on a single tunnel, e.g.
/// for multiple clients to connect or for multiple ports to be exposed.
pub struct TunnelState {
    name: String,
    open_time: String,
    peer_addr: String,
    server_write: Arc<Mutex<WsSender>>,    
    ports: Vec<ExposedPort>,
    channels: HashMap<u8, Channel>,
}

struct ExposedPort {
    exposed_addr: ExposedAddress,
    server_port: u16,

    /// The task that listens for incoming TCP connections on the server port and creates channels for them.
    /// This is stored in the struct for the purpose of aborting it when the tunnel is closed (RAII).
    _listen_task: GuardedJoinHandle<Result<()>>,
}

struct Channel {
    info: ChannelInfo,

    tcp_sender: tokio::net::tcp::OwnedWriteHalf,
    send_task: GuardedAbortHandle,
    open_success: Arc<Notify>,
}


async fn open_tcp_listener(port_range: RangeInclusive<u16>) -> Result<(u16, TcpListener)> {
    log::debug!("Open port in range: {:?}", port_range);
    for port in port_range.clone() {
        log::debug!("Trying port: {}", port);
        let listener = TcpListener::bind(("::", port)).await;
        match listener {
            Ok(listener) => {
                log::info!("Opened port: {}", port);
                return Ok((port, listener));
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
        .send(JsonMessage::OpenChannel { channel_id, exposed_address }.into())
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

    let peer_addr = stream.peer_addr()?;

    let ws_out = tunnel_lock.server_write.clone();
    let (tcp_read, tcp_write) = stream.into_split();
    let open_success = Arc::new(Notify::new());

    // Find free channel id
    let channel_id = (0..ChannelId::MAX).find(|id| !tunnel_lock.channels.contains_key(id)).ok_or_else(|| anyhow!("All channels occupied."))?;

    let channel_task = spawn_guarded(serve_channel(tcp_read, channel_id, exposed_address.clone(), ws_out, open_success.clone()));

    let channel = Channel {
        info: ChannelInfo {
            id: channel_id,
            exposed: exposed_address,
            peer_addr: format!("{:?}", peer_addr),
            open_time: now(),
        },
        tcp_sender: tcp_write,
        send_task: channel_task.guarded_abort_handle(),
        open_success,
    };
    
    tunnel_lock
        .channels
        .insert(channel_id, channel);

    log::info!("Started channel: {}", channel_id);

    // clean up once the task is done
    tokio::spawn(async move {
        let _result = channel_task.await;
        log::info!("Channel task finished: {}", channel_id);
        if let Ok(mut tunnel) = get_tunnel_lock(&tunnel).await {
            // If the WS socket to the exposer is still intact, let the exposer know that the channel is closed.
            let _ = tunnel.server_write.lock().await.send(JsonMessage::CloseChannel { channel_id, error: None }.into()).await;
            tunnel.channels.remove(&channel_id);
        }
    });

    Ok(())
}


async fn listen_tcp_port(listener: TcpListener, tunnel: Weak<Mutex<TunnelState>>, exposed_address: ExposedAddress) -> Result<()> {
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
        tunnel_lock.server_write.lock().await.send(JsonMessage::OpenTunnel { protocol_version: crate::protocol::PROTOCOL_VERSION }.into()).await?;
    }

    // Process incoming messages
    while let Some(msg) = ws_in.next().await {
        match msg {
            Ok(Message::Text(msg_text)) => {
                let msg_inner = serde_json::from_str(msg_text.as_str())?;
                log::trace!("json msg received: {:?}", msg_inner);
                match msg_inner {
                    JsonMessage::OpenTunnelSuccess { exposed } => {
                        log::info!("Exposer opened tunnel for ports: {:?}", exposed);
                        let mut tunnel_lock = get_tunnel_lock(&tunnel).await?;

                        for exposed_addr in exposed {
                            let (port, listener) = open_tcp_listener(options.port_range.clone()).await?;

                            let listen_task = spawn_guarded(listen_tcp_port(
                                listener,
                                tunnel.clone(),
                                exposed_addr.clone(),
                            ));

                            tunnel_lock.ports.push(ExposedPort {
                                exposed_addr,
                                _listen_task: listen_task,
                                server_port: port,
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
                    JsonMessage::CloseChannel { channel_id, error } => {
                        log::info!("Channel closed: {}", channel_id);
                        let mut tunnel_lock = get_tunnel_lock(&tunnel).await?;
                        if let Some(channel) = tunnel_lock.channels.get_mut(&channel_id) {
                            channel.send_task.abort();
                            if let Some(reason) = error {
                                log::error!("Channel closed with error: {} - {}", channel_id, reason);
                            } else {
                                log::info!("Channel closed: {}", channel_id);
                            }
                        }
                        else {
                            // If the channel is not in the list, we don't consider this an error.
                        }
                    },
                    _ => {}
                }
            }
            Ok(Message::Binary(encoded_msg)) => {
                if let Some(msg) = crate::protocol::BinaryMessage::from_ws(&encoded_msg[..]) {
                    let mut tunnel_lock = get_tunnel_lock(&tunnel).await?;
                    if let Some(channel) = tunnel_lock.channels.get_mut(&msg.channel_id) {
                        if let Err(e) = channel.tcp_sender.write_all(msg.data).await {
                            log::error!("Error writing to TCP stream, dropping channel: {}", e);
                            channel.send_task.abort();
                        }
                    } else {
                        log::warn!("Unknown channel id: {}", msg.channel_id);
                        continue;
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
    ws_out: WsSender,
    ws_in: WsReceiver,
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
        log::info!(
            "Dropping existing tunnel: {}, {}",
            tunnel_state.name, tunnel_state.peer_addr
        );
        // Dropping `tunnel` will close the connection, as the guarded abort handle is dropped
    }

    // Create new tunnel
    let tunnel_state = Arc::new(Mutex::new(TunnelState {
        server_write: Arc::new(Mutex::new(ws_out)),
        channels: HashMap::new(),
        ports: Vec::new(),
        name: name.clone(),
        open_time: now(),
        peer_addr,
    }));

    let task = spawn_guarded(serve_tunnel(
        ws_in,
        Arc::downgrade(&tunnel_state),
        server_state.options.clone(),
    ));
    
    server_state.tunnels.insert(name.clone(), Tunnel {
        uid: new_tunnel_id,
        _task: task.guarded_abort_handle(),
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


async fn list_tunnels(state: axum::extract::State<Arc<Mutex<TunnelServer>>>) -> axum::response::Json<serde_json::Value> {
    let server_state = state.lock().await;
    let infos = futures::stream::iter(&server_state.tunnels)
        .then(|(_, tunnel)| async {
            let tunnel_state = tunnel.state.lock().await;
            TunnelInfo {
                name: tunnel_state.name.clone(),
                open_time: tunnel_state.open_time.clone(),
                peer_addr: tunnel_state.peer_addr.clone(),
                ports: tunnel_state.ports.iter().map(|port| ExposedServerPort {
                    port: port.server_port,
                    exposed_addr: port.exposed_addr.clone(),
                }).collect::<Vec<_>>(),
                channels: tunnel_state
                    .channels
                    .values()
                    .map(|channel| channel.info.clone())
                    .collect::<Vec<_>>(),
            }
        })
        .collect::<Vec<_>>()
        .await;

    axum::response::Json(serde_json::json!(infos))
}


async fn ws_handler(
    ws: axum::extract::WebSocketUpgrade,
    state: axum::extract::State<Arc<Mutex<TunnelServer>>>,
    addr: axum::extract::ConnectInfo<SocketAddr>,
    query: axum::extract::Query<HashMap<String, String>>,
) -> std::result::Result<axum::response::Response, axum::http::StatusCode> {

    let name = query
        .get("name")
        .cloned()
        .ok_or(axum::http::StatusCode::BAD_REQUEST)?;

    log::debug!("Websocket connection at {:?} connected.", addr.0);
    Ok(ws.on_upgrade(move |socket| handle_socket(socket, state.0, name, addr.0)))
}

/// Actual websocket statemachine (one will be spawned per connection)
async fn handle_socket(socket: axum::extract::ws::WebSocket, server: Arc<Mutex<TunnelServer>>, name: String, addr: SocketAddr) {
    let (sender, receiver) = socket.split();
    match open_tunnel(server, sender, receiver, format!("{:?}", addr), name.clone()).await {
        Ok(_) => {
            log::info!("Tunnel opened: '{}'", name);
        }
        Err(e) => {
            log::error!("Error opening tunnel: '{}' - {}", name, e);
        }
    }
}

async fn root_html(
    state: axum::extract::State<Arc<Mutex<TunnelServer>>>,
) -> std::result::Result<axum::response::Html<String>, axum::http::StatusCode> {
    if let Some(filepath) = state.lock().await.options.root_file.clone() {
        let content = tokio::fs::read_to_string(filepath).await.map_err(|_| axum::http::StatusCode::NOT_FOUND)?;
        Ok(axum::response::Html(content))
    } else {
        // Serve the default HTML file
        let html_content = include_str!("html/index.html");
        Ok(axum::response::Html(html_content.to_string()))
    }
}


pub async fn serve(options: ServerOptions) -> Result<()> {
    let server = Arc::new(Mutex::new(TunnelServer {
        tunnels: HashMap::new(),
        options: options.clone(),
        id_counter: 0,
    }));

    // build our application with a single route
    let app = axum::Router::new()
        .route("/", axum::routing::get(root_html))
        .route("/list", axum::routing::get(list_tunnels))
        .route("/register", axum::routing::any(ws_handler))
        .with_state(server);

    // run our app with hyper, listening globally on port 3000
    let listener = TcpListener::bind(&options.listen_addr).await?;
    info!("Listening on {}", options.listen_addr);
    info!("Visit http://{}/list for a list of open tunnels.", options.listen_addr);
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await?;

    Ok(())
}
