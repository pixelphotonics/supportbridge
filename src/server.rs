use anyhow::{anyhow, Result};
use futures::{SinkExt, StreamExt};
use log::info;
use std::collections::HashMap;
use std::ops::RangeInclusive;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tungstenite::http::Uri;

use crate::protocol::{ClientInfo, ExposerInfo, ServerPath};
use crate::util::{spawn_guarded, GuardedJoinHandle};

pub struct TunnelServer {
    pub channels: HashMap<String, Channel>,
    options: ServerOptions,
}

pub struct ServerOptions {
    pub listen_addr: core::net::SocketAddr,
    pub open_port: bool,
    pub port_range: RangeInclusive<u16>,
    pub overwrite_existing_connection: bool,
    pub overwrite_existing_exposer: bool,
}

pub struct Channel {
    overwrite_existing_connection: bool,
    info: ExposerInfo,
    server_write: Arc<
        Mutex<
            futures::stream::SplitSink<
                tokio_tungstenite::WebSocketStream<TcpStream>,
                tungstenite::Message,
            >,
        >,
    >,
    server_read:
        Arc<Mutex<futures::stream::SplitStream<tokio_tungstenite::WebSocketStream<TcpStream>>>>,
    open_port_task: Option<JoinHandle<Result<()>>>,

    current_task: Option<GuardedJoinHandle<Result<()>>>,
}

impl Drop for Channel {
    fn drop(&mut self) {
        if let Some(open_port_task) = self.open_port_task.take() {
            open_port_task.abort();
        }
    }
}

impl Channel {
    fn spawn_idle_task(&mut self, server_ptr: Arc<Mutex<TunnelServer>>) {
        self.info.connected_client = None;
        let channel_name = self.info.name.clone();
        let ws_read = self.server_read.clone();
        let task = spawn_guarded(async move {
            // Idle state: listen to the unused websocket connection to see if it closes
            let mut ws_read = ws_read.lock().await;
            while let Some(_) = ws_read.next().await {
                // nothing to do, just wait for the connection to close
            }

            log::info!("Channel closed: {}", channel_name);
            server_ptr.lock().await.channels.remove(&channel_name);
            Ok(())
        });

        //self.abort_handles.push(join_handle.abort_handle());
        self.current_task = Some(task);
    }

    fn set_channel_user(
        &mut self,
        task: GuardedJoinHandle<Result<()>>,
        peer_addr: String,
        is_via_port: bool,
        server_ptr: Arc<Mutex<TunnelServer>>,
    ) {
        if self.info.connected_client.is_some() {
            if self.overwrite_existing_connection {
                log::info!("Overwriting existing connection");
            } else {
                log::warn!("Client connection already active, ignoring connection attempt");
                // dropping the passed task
                return;
            }
        }

        let channel_name = self.info.name.clone();

        // This task does not need to be aborted. If all tasks finish and this spawns the idle task,
        // this is fine, as the abort_handle of the idle task is added to the abort handle list within spawn_idle_task()
        let task = spawn_guarded(async move {
            log::debug!("set_channel_user1");
            task.await??;
            log::debug!("set_channel_user2");

            let mut server_state = server_ptr.lock().await;
            if let Some(channel) = server_state.channels.get_mut(&channel_name) {
                channel.spawn_idle_task(server_ptr.clone());
            }

            Ok(())
        });

        self.current_task = Some(task);
        self.info.connected_client = Some(ClientInfo {
            peer_addr,
            uses_port: is_via_port,
        });
    }
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

    return Err(anyhow::anyhow!(
        "No available ports in range: {:?}",
        port_range
    ));
}

async fn open_connection_port(
    id: String,
    server: Arc<Mutex<TunnelServer>>,
    port_range: RangeInclusive<u16>,
) -> Result<()> {
    let tcp_listener = open_tcp_listener(port_range).await?;
    {
        // store information about the open port
        let mut server_state = server.lock().await;
        let channel = server_state
            .channels
            .get_mut(&id)
            .ok_or(anyhow!("Unknown channel id: {}", id))?;
        channel.info.open_port = Some(tcp_listener.local_addr()?.port());
    }

    loop {
        let (stream, addr) = tcp_listener.accept().await?;

        log::debug!("Connection on open port: {}", addr);

        // get server streams
        let mut server_state = server.lock().await;
        let channel = server_state
            .channels
            .get_mut(&id)
            .ok_or(anyhow!("Unknown channel id: {}", id))?;

        let ws_out = channel.server_write.clone();
        let ws_in = channel.server_read.clone();

        let task = spawn_guarded(async move {
            let mut ws_out = ws_out.lock_owned().await;
            ws_out
                .send(tungstenite::Message::Text("init".into()))
                .await?;


            let (tcp_read, tcp_write) = stream.into_split();
            
            let task_tcp_to_ws = crate::tcp_to_ws(tcp_read, ws_out);
            let task_ws_to_tcp = crate::ws_to_tcp(ws_in.lock_owned().await, tcp_write, None::<Vec<String>>);

            task_tcp_to_ws.await??;
            task_ws_to_tcp.await??;
            Ok(())
        });

        channel.set_channel_user(task, format!("{}", addr), true, server.clone());
    }
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
            let mut server_state = server.lock().await;

            if let Some(channel) = server_state.channels.remove(&name) {
                println!(
                    "Dropping existing channel: {}, {}",
                    channel.info.name, channel.info.peer_addr
                );
                // Dropping the channel will close the connection
            }

            // Open a port for this channel
            let open_port_task = if server_state.options.open_port {
                // The server is still locked when this is spawened, so the open port task will only start once the server is unlocked at the end of the parent scope
                Some(tokio::spawn(open_connection_port(
                    name.clone(),
                    server.clone(),
                    server_state.options.port_range.clone(),
                )))
            } else {
                None
            };

            // Create new channel
            let (ws_out, ws_in) = ws_stream.split();

            let mut new_channel = Channel {
                server_write: Arc::new(Mutex::new(ws_out)),
                server_read: Arc::new(Mutex::new(ws_in)),
                info: ExposerInfo {
                    name: name.clone(),
                    open_time: chrono::Utc::now()
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    connected_client: None,
                    peer_addr: peer_addr,
                    open_port: None,
                },
                overwrite_existing_connection: server_state.options.overwrite_existing_connection,
                open_port_task,

                current_task: None,
            };

            new_channel.spawn_idle_task(server.clone());
            server_state.channels.insert(name.clone(), new_channel);
        }
        ServerPath::Connect { name } => {
            let mut server_state = server.lock().await;
            if let Some(channel) = server_state.channels.get_mut(&name) {
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
        }
        ServerPath::List => {
            let server_state = server.lock().await;
            let infos: Vec<_> = server_state
                .channels
                .iter()
                .map(|c| c.1.info.clone())
                .collect();
            let data = serde_json::to_string(&infos)?;
            ws_stream.send(tungstenite::Message::Text(data)).await?;
            ws_stream.close(None).await?;
        }
    }

    Ok(())
}

pub async fn serve(options: ServerOptions) -> Result<()> {
    let listener = TcpListener::bind(&options.listen_addr).await?;
    info!("Listening on {}", options.listen_addr);

    let server = Arc::new(Mutex::new(TunnelServer {
        channels: HashMap::new(),
        options,
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
