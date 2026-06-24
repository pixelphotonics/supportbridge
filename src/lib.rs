use anyhow::Result;
use futures::{stream::Stream, Sink, SinkExt, StreamExt};
use protocol::{ChannelId, ServerPath};
use std::{error::Error, sync::Arc};
use tokio::io::{AsyncRead};
use std::{marker::{Send, Unpin}, ops::DerefMut};
use tungstenite::Message;
use util::{spawn_guarded, GuardedJoinHandle};

pub mod bridge;
pub mod expose;
pub mod protocol;
pub mod server;
pub mod util;
use tokio::io::AsyncReadExt;

pub type WsError = tungstenite::error::Error;
pub type WsResult = std::result::Result<Message, WsError>;


/// Take a TCP stream and relay all binary messages from the TCP stream
/// to the websocket stream using the custom protocol.
pub fn tcp_to_ws_encoded<TRx, WTx, M, E>(
    channel_id: ChannelId,
    mut rx_tcp: TRx,
    tx_ws: Arc<tokio::sync::Mutex<WTx>>,
) -> GuardedJoinHandle<Result<()>>
where
    TRx: AsyncRead + Unpin  + Send + 'static,
    WTx: Sink<M, Error = E>  + Unpin + Send + 'static,
    M: From<Vec<u8>> + Send + 'static,
    E: Error + Send + 'static,
{
    spawn_guarded(async move {
        log::debug!("Starting TCP->WS relay");
        loop {
            let mut buf = vec![0; 1024];
            let n = rx_tcp.read(&mut buf[1..]).await?;
            if n == 0 {
                break;
            }

            buf[0] = channel_id;
            buf.truncate(n + 1);

            let msg = M::from(buf);

            log::debug!("TCP->WS: {} bytes", n);
            let res = tx_ws
                .lock()
                .await
                .send(msg)
                .await;

            if let Err(e) = res {
                log::error!("Error sending message to websocket: {}", e);
            }
        }

        Ok(())
    })
}


pub async fn ws_bridge<WSTX, WSRX>(
    mut ws_up_rx: impl DerefMut<Target = WSRX> + Send + 'static,
    mut ws_up_tx: impl DerefMut<Target = WSTX> + Send + 'static,
    mut ws_down_rx: impl DerefMut<Target = WSRX> + Send + 'static,
    mut ws_down_tx: impl DerefMut<Target = WSTX> + Send + 'static,
) -> Result<()>
where
    WSTX: Sink<Message, Error = tungstenite::error::Error>
        + Unpin
        + Send
        + 'static,
    WSRX: Stream<Item = WsResult>
        + Unpin
        + Send
        + 'static,
{
    let up_to_down: GuardedJoinHandle<Result<()>> = spawn_guarded(async move {
        while let Some(msg) = ws_up_rx.next().await {
            let msg = msg?;
            log::trace!("up -> down: {} bytes", msg.len());
            ws_down_tx.send(msg).await?;
        }

        Ok(())
    });

    let down_to_up: GuardedJoinHandle<Result<()>> = spawn_guarded(async move {
        while let Some(msg) = ws_down_rx.next().await {
            let msg = msg?;
            log::trace!("down -> up: {} bytes", msg.len());
            ws_up_tx.send(msg).await?;
        }

        Ok(())
    });

    down_to_up.await??;
    up_to_down.await??;

    Ok(())
}



pub async fn connect_to_server(
    ws_server: String,
    cmd: ServerPath,
) -> Result<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>> {
    let request = crate::util::build_request(&ws_server, cmd)?;
    let (ws_server_stream, _) = tokio_tungstenite::connect_async(request).await?;
    Ok(ws_server_stream)
}