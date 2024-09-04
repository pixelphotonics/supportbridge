use anyhow::Result;
use futures::{SinkExt, StreamExt};
use std::ops::DerefMut;
use tungstenite::Message;
use util::{spawn_guarded, GuardedJoinHandle};

pub mod bridge;
pub mod client;
pub mod expose;
pub mod protocol;
pub mod server;
pub mod util;


pub fn ws_bridge<WSTX, WSRX>(
    mut ws_up_rx: impl DerefMut<Target = WSRX> + Send + 'static,
    mut ws_up_tx: impl DerefMut<Target = WSTX> + Send + 'static,
    mut ws_down_rx: impl DerefMut<Target = WSRX> + Send + 'static,
    mut ws_down_tx: impl DerefMut<Target = WSTX> + Send + 'static,
) -> Result<(GuardedJoinHandle<Result<()>>, GuardedJoinHandle<Result<()>>)>
where
    WSTX: futures::sink::Sink<Message, Error = tungstenite::error::Error>
        + std::marker::Unpin
        + std::marker::Send
        + 'static,
    WSRX: futures::stream::Stream<Item = std::result::Result<Message, tungstenite::error::Error>>
        + std::marker::Unpin
        + std::marker::Send
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

    Ok((down_to_up, up_to_down))
}
