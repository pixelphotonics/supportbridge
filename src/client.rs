use std::net::SocketAddr;
use std::ops::DerefMut;
use std::sync::Arc;
use futures::{SinkExt, StreamExt};
use tokio::sync::Mutex;
use tokio::net::{TcpListener, TcpStream};
use log::{debug, info};
use anyhow::Result;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;
use tungstenite::Message;
use crate::util::{spawn_guarded, GuardedJoinHandle};

pub struct Client {
    pub target_addr: core::net::SocketAddr,
    pub listen_addr: core::net::SocketAddr,

    pub target_socket: Option<TcpStream>,
    pub listen_socket: Option<TcpStream>,
}

pub struct ClientConnection {
    pub write_half: tokio::net::tcp::OwnedWriteHalf,
    pub close_sender: oneshot::Sender<()>,
}




pub fn tcp_to_ws<WSTX, WSRX> (
    tcp_stream: TcpStream,
    mut ws_in: impl DerefMut<Target = WSRX> + Send + 'static,
    mut ws_out: impl DerefMut<Target = WSTX> + Send + 'static,
) -> Result<(GuardedJoinHandle<Result<()>>, GuardedJoinHandle<Result<()>>)> 
where
    WSTX: futures::sink::Sink<Message, Error=tungstenite::error::Error> + std::marker::Unpin + std::marker::Send + 'static,
    WSRX: futures::stream::Stream<Item=std::result::Result<Message, tungstenite::error::Error>> + std::marker::Unpin + std::marker::Send + 'static,
{

    let (mut tcp_read, mut tcp_write) = tcp_stream.into_split();

    // Relay all messages from server to exposer and vice versa
    let ws_to_tcp: GuardedJoinHandle<Result<()>> = spawn_guarded(async move {
        log::debug!("Starting WS->TCP relay");
        while let Some(msg) = ws_in.next().await {
            let msg = msg?;
            if msg.is_binary() {
                log::debug!("WS->TCP: {} bytes", msg.len());
                tcp_write.write_all(msg.into_data().as_slice()).await?;
            }
        }

        Ok(())
    });

    let tcp_to_ws: GuardedJoinHandle<Result<()>> = spawn_guarded(async move {
        log::debug!("Starting TCP->WS relay");
        loop {
            let mut buf = vec![0; 1024];
            let n = tcp_read.read(buf.as_mut_slice()).await?;
            if n == 0 {
                break;
            }

            log::debug!("TCP->WS: {} bytes", n);
            ws_out.send(tungstenite::Message::Binary(buf[..n].to_vec())).await?;
        }

        Ok(())
    });

    Ok((tcp_to_ws, ws_to_tcp))

}


async fn handle_connection(listen_stream: TcpStream, ws_server: String, name: String) -> Result<()> {
    let cmd = crate::protocol::ServerPath::Connect { name };
    let request = crate::util::build_request(&ws_server, cmd)?;
    let (ws_server_stream, _) = tokio_tungstenite::connect_async(request).await?;
    let (ws_out, ws_in) = ws_server_stream.split();

    let (task1, task2) = tcp_to_ws(listen_stream, Box::new(ws_in), Box::new(ws_out))?;
    task1.await?;
    task2.await?;
    //bridge.join().await?;

    Ok(())
}

pub async fn serve(tcp_bind: SocketAddr, ws_server: String, name: String) -> Result<()> {

    let listener = TcpListener::bind(tcp_bind).await?;
    info!("Listening on {}", tcp_bind);

    while let Ok((stream, _)) = listener.accept().await {
        let peer = stream.peer_addr()?;
        info!("Peer address: {}", peer);

        let ws_server = ws_server.clone();
        let name = name.clone();

        tokio::spawn(async move {
            match handle_connection(stream, ws_server, name).await {
                Ok(_) => {
                    log::info!("Connection closed: {}", peer);
                },
                Err(e) => {
                    log::error!("Connection error: {:?}", e);
                }
            }
        });
    }

    Ok(())
}