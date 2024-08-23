use std::collections::HashMap;
use std::ops::RangeInclusive;
use anyhow::{anyhow, Result};
use tokio::task::JoinHandle;
use tungstenite::http::Uri;
use std::sync::Arc;
use futures::{SinkExt, StreamExt};
use tokio::sync::Mutex;
use tokio::net::{TcpListener, TcpStream};
use log::info;

use crate::client::tcp_to_ws;
use crate::protocol::{ExposerInfo, ServerPath};
use crate::ProxyTasks;


pub struct TunnelServer {
    pub channels: HashMap<String, Channel>,
    options: ServerOptions,
}

pub struct ServerOptions {
    pub listen_addr: core::net::SocketAddr,
    pub open_port: bool,
    pub port_range: RangeInclusive<u16>,
    pub overwrite_existing_connection: bool,
}

pub struct Channel {
    overwrite_existing_connection: bool,
    info: ExposerInfo,
    server_write: Arc<Mutex<futures::stream::SplitSink<tokio_tungstenite::WebSocketStream<TcpStream>, tungstenite::Message>>>,
    server_read: Arc<Mutex<futures::stream::SplitStream<tokio_tungstenite::WebSocketStream<TcpStream>>>>,
    join_handle: Option<ProxyTasks>,
    open_port_task: Option<JoinHandle<Result<()>>>,
}

impl Drop for Channel {
    fn drop(&mut self) {
        if let Some(join_handle) = self.join_handle.take() {
            join_handle.abort();
        }

        if let Some(open_port_task) = self.open_port_task.take() {
            open_port_task.abort();
        }
    }
}

impl Channel {
    fn set_proxy_tasks(&mut self, proxy_tasks: ProxyTasks, peer_addr: String) -> Result<()> {
        if self.join_handle.is_some() {
            return Err(anyhow!("Proxy tasks already set"));
        }

        self.join_handle = Some(proxy_tasks);
        self.info.connected_client = Some(peer_addr);

        Ok(())
    }

    fn close_proxy_tasks(&mut self) {
        if let Some(join_handle) = self.join_handle.take() {
            log::info!("Closing client connection");
            join_handle.abort();
        }
        self.info.connected_client = None;
    }

    async fn prepare_new_connection(&mut self) -> Result<()>{
        if self.join_handle.is_some() {
            if self.overwrite_existing_connection {
                self.close_proxy_tasks();
            } else {
                return Err(anyhow!("Client connection already active"));
            }
        }

        let mut lock = self.server_write.lock().await;
        lock.send(tungstenite::Message::Text("init".into())).await?;
        Ok(())
    }
}

struct CallbackHandler {
    uri: Option<Uri>,
}

impl tungstenite::handshake::server::Callback for &mut CallbackHandler {
    fn on_request(
        self,
        request: &tungstenite::handshake::server::Request,
        response: tungstenite::handshake::server::Response
    ) -> std::result::Result<tungstenite::handshake::server::Response, tungstenite::handshake::server::ErrorResponse> {
        log::info!("URI: {}", request.uri());

        self.uri = Some(request.uri().clone());
        
        Ok(response)
    }
}

async fn open_tcp_listener(port_range: RangeInclusive<u16>) -> Result<TcpListener> {
    for port in port_range.clone() {
        let listener = TcpListener::bind(("[::]", port)).await;
        if let Ok(listener) = listener {
            log::info!("Opened port: {}", port);
            return Ok(listener);
        }
    }

    return Err(anyhow::anyhow!("No available ports in range: {:?}", port_range));
}

async fn open_connection_port(id: String, server: Arc<Mutex<TunnelServer>>, port_range: RangeInclusive<u16>) -> Result<()> {
    let tcp_listener = open_tcp_listener(port_range).await?;

    loop {
        let (stream, addr) = tcp_listener.accept().await?;

        // get server streams
        let mut server_state = server.lock().await;
        let channel = server_state.channels.get_mut(&id).ok_or(anyhow!("Unknown channel id: {}", id))?;

        channel.prepare_new_connection().await?;

        let ws_out = channel.server_write.clone().lock_owned().await;
        let ws_in = channel.server_read.clone().lock_owned().await;
        let bridge = tcp_to_ws(stream, ws_in, ws_out).await?;

        channel.set_proxy_tasks(bridge, format!("{}", addr))?;
    }
}


async fn handle_connection(server: Arc<Mutex<TunnelServer>>, listen_stream: TcpStream) -> Result<()> {
    let mut callback_handler = CallbackHandler{ uri: None };
    let peer_addr = listen_stream.peer_addr().map(|a| a.to_string()).unwrap_or_default();
    let mut ws_stream = tokio_tungstenite::accept_hdr_async(listen_stream, &mut callback_handler).await?;

    let uri = callback_handler.uri.as_ref().ok_or_else(|| anyhow::anyhow!("No URI found"))?;
    log::debug!("URI: {}", uri);
    let server_cmd = ServerPath::from_uri(uri)?;

    match server_cmd {
        ServerPath::Register { name } => {
            log::debug!("Register server: '{}'", &name);
            let mut server_state = server.lock().await;

            if let Some(channel) = server_state.channels.remove(&name) {
                println!("Dropping existing channel: {}, {}", channel.info.name, channel.info.peer_addr);
                // Dropping the channel will close the connection
            }

            // Open a port for this channel
            let open_port_task = if server_state.options.open_port {
                // The server is still locked when this is spawened, so the open port task will only start once the server is unlocked at the end of the parent scope
                Some(tokio::spawn(open_connection_port(name.clone(), server.clone(), server_state.options.port_range.clone())))
            } else {
                None
            };

            // Create new channel
            let (ws_out, ws_in) = ws_stream.split();

            let new_channel = Channel {
                server_write: Arc::new(Mutex::new(ws_out)),
                server_read: Arc::new(Mutex::new(ws_in)),
                join_handle: None,
                info: ExposerInfo {
                    name: name.clone(),
                    connection_time: chrono::Utc::now().to_rfc3339(),
                    connected_client: None,
                    peer_addr: peer_addr,
                },
                overwrite_existing_connection: server_state.options.overwrite_existing_connection,
                open_port_task,
            };

            server_state.channels.insert(name.clone(), new_channel);
        },
        ServerPath::Connect { name } => {
            let mut server_state = server.lock().await;
            if let Some(channel) = server_state.channels.get_mut(&name) {
                log::info!("Connect to channel: {}", name);
                channel.prepare_new_connection().await?;

                let (client_write, client_read) = ws_stream.split();

                let ws_proxy = crate::ws_bridge(
                    channel.server_read.clone().lock_owned().await,
                    channel.server_write.clone().lock_owned().await,
                    Box::new(client_read),
                    Box::new(client_write),
                ).await?;

                channel.set_proxy_tasks(ws_proxy, peer_addr)?;
            } else {
                log::error!("Channel not found: {}", name);
                ws_stream.close(None).await?;
            }
        },
        ServerPath::List => {
            let server_state = server.lock().await;
            let infos: Vec<_> = server_state.channels.iter().map(|c| c.1.info.clone()).collect();
            let data = serde_json::to_string(&infos)?;
            ws_stream.send(tungstenite::Message::Text(data)).await?;
        },
    }

    Ok(())
    
}

pub async fn serve(listen_addr: core::net::SocketAddr) -> Result<()> {
    let listener = TcpListener::bind(listen_addr).await?;
    info!("Listening on {}", listen_addr);

    let server  = Arc::new(Mutex::new(TunnelServer {
        channels: HashMap::new(),
        options: ServerOptions {
            listen_addr,
            open_port: true,
            port_range: 11000..=64000,
            overwrite_existing_connection: true,
        }
    }));

    while let Ok((stream, _)) = listener.accept().await {
        let peer = stream.peer_addr()?;
        info!("Peer address: {}", peer);

        match handle_connection(server.clone(), stream).await {
            Ok(_) => {},
            Err(e) => {
                log::error!("Error handling connection: {:?}", e);
            }
        }
    }

    Ok(())
}