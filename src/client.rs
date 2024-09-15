use crate::protocol::ServerPath;
use anyhow::Result;
use futures::StreamExt;
use std::net::SocketAddr;
use tokio::net::{TcpListener, TcpStream};


pub async fn connect_to_server(
    ws_server: String,
    cmd: ServerPath,
) -> Result<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>> {
    let request = crate::util::build_request(&ws_server, cmd)?;
    let (ws_server_stream, _) = tokio_tungstenite::connect_async(request).await?;
    Ok(ws_server_stream)
}

async fn handle_connection(
    listen_stream: TcpStream,
    ws_server: String,
    name: String,
) -> Result<()> {
    let ws_server_stream = connect_to_server(ws_server, ServerPath::Connect { name }).await?;
    let (ws_out, ws_in) = ws_server_stream.split();
    let (tcp_read, tcp_write) = listen_stream.into_split();

    let task_tcp_to_ws = crate::tcp_to_ws(tcp_read, Box::new(ws_out));
    let task_ws_to_tcp = crate::ws_to_tcp(Box::new(ws_in), tcp_write, None::<Vec<String>>);

    task_tcp_to_ws.await??;
    task_ws_to_tcp.await??;

    Ok(())
}

pub async fn serve(tcp_bind: SocketAddr, ws_server: String, name: String) -> Result<()> {
    let listener = TcpListener::bind(tcp_bind).await?;
    log::info!("Listening on {}", tcp_bind);

    while let Ok((stream, _)) = listener.accept().await {
        let peer = stream.peer_addr()?;
        log::info!("Peer address: {}", peer);

        let ws_server = ws_server.clone();
        let name = name.clone();

        tokio::spawn(async move {
            match handle_connection(stream, ws_server, name).await {
                Ok(_) => {
                    log::info!("Connection closed: {}", peer);
                }
                Err(e) => {
                    log::error!("Connection error: {:?}", e);
                }
            }
        });
    }

    Ok(())
}
