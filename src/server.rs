use std::collections::HashMap;
use anyhow::Result;
use tungstenite::http::Uri;
use std::sync::Arc;
use futures::{SinkExt, StreamExt};
use tokio::sync::Mutex;
use tokio::net::{TcpListener, TcpStream};
use log::{debug, info};

use crate::protocol::ExposerInfo;


pub struct TunnelServer {
    pub channels: HashMap<String, Channel>,
}

pub struct Channel {
    info: ExposerInfo,
    server_write: Arc<Mutex<futures::stream::SplitSink<tokio_tungstenite::WebSocketStream<TcpStream>, tungstenite::Message>>>,
    server_read: Arc<Mutex<futures::stream::SplitStream<tokio_tungstenite::WebSocketStream<TcpStream>>>>,
    join_handle: Option<(tokio::task::JoinHandle<()>, tokio::task::JoinHandle<()>)>,
}

/// Parse the query string into a HashMap
fn parse_query(query: &str) -> HashMap<String, String> {
    query.split('&').filter_map(|pair| {
        let mut parts = pair.split('=');
        
        let key = parts.next();
        let value = parts.next();

        if key.is_none() || value.is_none() {
            None
        } else {
            Some((key.unwrap().to_string(), value.unwrap().to_string()))
        }
        
    }).collect()
}

enum ServerPath {
    Register {
        name: String,
    },
    Connect {
        name: String,
    },
    List,
}

impl ServerPath {
    fn from_uri(uri: &Uri) -> Result<Self> {
        let path = uri.path();
        let query = uri.query().unwrap_or("");

        let query = parse_query(query);

        match path {
            "/register" => {
                let name = query.get("name").ok_or(anyhow::anyhow!("Name not found"))?;
                Ok(ServerPath::Register {
                    name: name.clone(),
                })
            },
            "/connect" => {
                let name = query.get("name").ok_or(anyhow::anyhow!("Name not found"))?;
                Ok(ServerPath::Connect {
                    name: name.clone(),
                })
            },
            "/list" => {
                Ok(ServerPath::List)
            },
            _ => Err(anyhow::anyhow!("Unknown path: {}", path)),
        }
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


async fn handle_connection(server: Arc<Mutex<TunnelServer>>, listen_stream: TcpStream) -> Result<()> {
    let mut callback_handler = CallbackHandler{ uri: None };
    let peer_addr = listen_stream.peer_addr().map(|a| a.to_string()).unwrap_or_default();
    let mut ws_stream = tokio_tungstenite::accept_hdr_async(listen_stream, &mut callback_handler).await?;

    let server_cmd = ServerPath::from_uri(callback_handler.uri.as_ref().unwrap())?;

    match server_cmd {
        ServerPath::Register { name } => {
            log::debug!("Register server: '{}'", &name);
            let mut server_state = server.lock().await;
            if let Some(channel) = server_state.channels.remove(&name) {
                if let Some(join_handle) = channel.join_handle {
                    join_handle.0.abort();
                    join_handle.1.abort();
                }
            }

            // Create new channel
            let (ws_out, ws_in) = ws_stream.split();

            let server_write = Arc::new(Mutex::new(ws_out));
            let server_read = Arc::new(Mutex::new(ws_in));

            server_state.channels.insert(name.clone(), Channel {
                server_write,
                server_read,
                join_handle: None,
                info: ExposerInfo {
                    name,
                    connection_time: chrono::Utc::now().to_rfc3339(),
                    connected_client: None,
                    peer_addr: peer_addr,
                },
            });
        },
        ServerPath::Connect { name } => {
            let mut server_state = server.lock().await;
            if let Some(channel) = server_state.channels.get_mut(&name) {
                log::debug!("Connect to channel: {}", name);
                if let Some(join_handle) = channel.join_handle.take() {
                    log::debug!("Disconnect existing client connection");
                    join_handle.0.abort();
                    join_handle.1.abort();
                }

                let server_write = channel.server_write.clone();
                let server_read = channel.server_read.clone();
                let (mut client_write, mut client_read) = ws_stream.split();

                let task_1 = tokio::spawn(async move {
                    let mut server_read = server_read.lock().await;
                    while let Some(msg) = server_read.next().await {
                        let msg = msg.expect("Failed to get message");
                        client_write.send(msg).await.expect("Failed to send to client");
                    }
                });

                let task_2 = tokio::spawn(async move {
                    let mut server_write = server_write.lock().await;

                    server_write.send(tungstenite::Message::Text("init".into())).await.expect("Failed to send 'init' to server");

                    while let Some(msg) = client_read.next().await {
                        let msg = msg.expect("Failed to get message");
                        server_write.send(msg).await.expect("Failed to send to server");
                    }
                });

                channel.join_handle = Some((task_1, task_2));
                channel.info.connected_client = Some(peer_addr);
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