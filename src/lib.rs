use anyhow::Result;
use futures::{stream::Stream, Sink, SinkExt, StreamExt};
use std::error::Error;
use tokio::io::{AsyncRead, AsyncWrite};
use std::{marker::{Send, Unpin}, ops::DerefMut};
use tungstenite::Message;
use util::{spawn_guarded, GuardedJoinHandle};

pub mod bridge;
pub mod client;
pub mod expose;
pub mod protocol;
pub mod server;
pub mod util;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub type WsError = tungstenite::error::Error;
pub type WsResult = std::result::Result<Message, WsError>;

pub trait WriteBinary {
    fn write_binary(&mut self, data: &[u8]) -> impl std::future::Future<Output = Result<()>> + std::marker::Send;
}

impl<T> WriteBinary for T
where
    T: AsyncWrite + Unpin + std::marker::Send,
{
    async fn write_binary(&mut self, data: &[u8]) -> Result<()> {
        self.write_all(data).await?;
        Ok(())
    }
}


/// Take a websocket stream and relay all binary messages from the websocket
/// to the TCP stream, while relaying all text messages to the text handler.
pub fn ws_to_tcp<WsRx, TcpTx, TxtRx, TxtErr>(
    mut ws_in: impl DerefMut<Target = WsRx> + Send + 'static,
    mut tcp_out: TcpTx,
    mut text_handler: Option<TxtRx>,
) -> GuardedJoinHandle<Result<()>>
where
    WsRx: Stream<Item = WsResult> + Unpin + Send + 'static,
    TcpTx: WriteBinary + Unpin + Send + 'static,
    TxtRx: Sink<String, Error = TxtErr> + Unpin + Send + 'static,
    TxtErr: Error + Send + Sync + 'static,
{
    spawn_guarded(async move {
        log::debug!("Starting WS->TCP relay");
        while let Some(msg) = ws_in.next().await {
            match msg? {
                tungstenite::Message::Text(textmsg) => {
                    log::trace!("WS: Text message: {}", textmsg);
                    if let Some(text_handler) = text_handler.as_mut() {
                        text_handler.send(textmsg).await?;
                    }
                },
                tungstenite::Message::Binary(binmsg) => {
                    log::trace!("WS->TCP: {} bytes", binmsg.len());
                    tcp_out.write_binary(&binmsg).await?;
                }
                tungstenite::Message::Close(_) => {
                    log::debug!("WS->TCP: Close message received");
                    break;
                },
                _ => { }
            }
        }

        Ok(())
    })
}

/// Take a TCP stream and relay all binary messages from the TCP stream
/// to the websocket stream.
pub fn tcp_to_ws<TRx, WTx>(
    mut rx_tcp: TRx,
    mut tx_ws: impl DerefMut<Target = WTx> + Send + 'static,
) -> GuardedJoinHandle<Result<()>>
where
    TRx: AsyncRead + Unpin  + Send + 'static,
    WTx: Sink<Message, Error = WsError>  + Unpin + Send + 'static,
{
    spawn_guarded(async move {
        log::debug!("Starting TCP->WS relay");
        loop {
            let mut buf = vec![0; 1024];
            let n = rx_tcp.read(buf.as_mut_slice()).await?;
            if n == 0 {
                break;
            }

            log::debug!("TCP->WS: {} bytes", n);
            tx_ws
                .send(tungstenite::Message::Binary(buf[..n].to_vec()))
                .await?;
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
