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

use crate::protocol::{ChannelId, ExposedAddress, ExposerInfo, JsonMessage};
use crate::util::{spawn_guarded, GuardedAbortHandle, GuardedJoinHandle};

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
        .send(JsonMessage::OpenChannel { channel_id, exposed_address }.into())
        .await;

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
    let channel_id = (0..ChannelId::MAX).find(|id| !tunnel_lock.channels.contains_key(id)).ok_or_else(|| anyhow!("All channels occupied."))?;

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
        tunnel_lock.server_write.lock().await.send(JsonMessage::OpenTunnel {  }.into()).await?;
    }

    // Process incoming messages
    while let Some(msg) = ws_in.next().await {
        match msg {
            Ok(Message::Text(msg_text)) => {
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
        println!(
            "Dropping existing tunnel: {}, {}",
            tunnel_state.info.name, tunnel_state.info.peer_addr
        );
        // Dropping `tunnel` will close the connection, as the guarded abort handle is dropped
    }

    // Create new channel
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


async fn list_tunnels(state: axum::extract::State<Arc<Mutex<TunnelServer>>>) -> axum::response::Json<serde_json::Value> {
    let server_state = state.lock().await;
    let infos = futures::stream::iter(&server_state.tunnels)
        .then(|(_, tunnel)| async {
            let tunnel_state = tunnel.state.lock().await;
            tunnel_state.info.clone()
        })
        .collect::<Vec<_>>()
        .await;

    axum::response::Json(serde_json::json!(infos))
}


async fn ws_handler(
    ws: axum::extract::WebSocketUpgrade,
    state: axum::extract::State<Arc<Mutex<TunnelServer>>>,
    //addr: axum::extract::ConnectInfo<tokio::net::unix::SocketAddr>,
    query: axum::extract::Query<HashMap<String, String>>,
) -> std::result::Result<axum::response::Response, axum::http::StatusCode> {

    let name = query
        .get("name")
        .cloned()
        .ok_or(axum::http::StatusCode::BAD_REQUEST)?;

    //println!("WS connection at {:?} connected.", addr.0);
    Ok(ws.on_upgrade(move |socket| handle_socket(socket, state.0, name)))
}

/// Actual websocket statemachine (one will be spawned per connection)
async fn handle_socket(socket: axum::extract::ws::WebSocket, server: Arc<Mutex<TunnelServer>>, name: String) {
    let (sender, receiver) = socket.split();
    match open_tunnel(server, sender, receiver, format!("Unknown"), name.clone()).await {
        Ok(_) => {
            log::info!("Tunnel opened: '{}'", name);
        }
        Err(e) => {
            log::error!("Error opening tunnel: '{}' - {}", name, e);
        }
    }
}


pub async fn serve(options: ServerOptions) -> Result<()> {
    use axum::{
        routing::get,
        Router,
        routing::any,
    };

    let server = Arc::new(Mutex::new(TunnelServer {
        tunnels: HashMap::new(),
        options: options.clone(),
        id_counter: 0,
    }));

    // build our application with a single route
    let app = Router::new()
        .route("/", get(|| async { "Hello, World!" }))
        .route("/list", get(list_tunnels))
        .route("/register", any(ws_handler))
        .with_state(server);

    // run our app with hyper, listening globally on port 3000
    let listener = TcpListener::bind(&options.listen_addr).await?;
    info!("Listening on {}", options.listen_addr);
    axum::serve(listener, app).await?;

    Ok(())
}
