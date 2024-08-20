use clap::{Parser, Subcommand};
use std::net::SocketAddr;
use servicebridge::util;
use crate::util::build_url_base;

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Cli {

    #[command(subcommand)]
    command: Command,
}


#[derive(Subcommand)]
enum Command {
    /// Run the central (public) server
    Serve {
        /// The Ip address:port combination to listen on.
        #[clap(long, default_value = "[::]:8081")]
        bind: SocketAddr,
    },

    /// Run the websocket-to-TCP bridge
    Expose {
        /// The Ip address:port combination to listen on.
        #[clap(long, default_value = "[::]:8082")]
        bind: SocketAddr,

        /// The address of the TCP server to connect to. Can be a hostname or IP address. A port can be specified with a colon.
        address: String,
    },

    /// Connect to a server and bind a local TCP port.
    Connect {
        /// The Ip address:port combination to listen on.
        #[clap(long, default_value = "[::]:8083")]
        bind: SocketAddr,

        /// The address of the central server to connect to. Can be a hostname or IP address. A port can be specified with a colon.
        /// Can also be a websocket URL, such as ws://localhost:8081 or wss://example.com.
        /// If no URL schema is included, ws:// is assumed.
        /// 
        /// When using a URL, a username and password can be included in the URL, such as ws://user:pass@localhost:8081.
        /// For security reasons, it is recommended to use a secure connection (wss://) and a password.
        server_addr: String,

        /// Name of the exposed machine
        name: String,
    },

    /// Connect an exposed websocket-to-TCP bridge with a server
    Relay {
        /// The address of the device where the WS-to-TCP bridge is running. Can be a hostname or IP address. A port can be specified with a colon.
        exposed_addr: String,

        /// The address of the central server to connect to. Can be a hostname or IP address. A port can be specified with a colon.
        /// Can also be a websocket URL, such as ws://localhost:8081 or wss://example.com.
        /// If no URL schema is included, ws:// is assumed.
        /// 
        /// When using a URL, a username and password can be included in the URL, such as ws://user:pass@localhost:8081.
        /// For security reasons, it is recommended to use a secure connection (wss://) and a password.
        server: String,

        /// Name of the exposed machine
        name: String,
    },

    /// List all open tunnels on the central server
    List {
        /// The address of the central server to connect to. Can be a hostname or IP address. A port can be specified with a colon.
        /// Can also be a websocket URL, such as ws://localhost:8081 or wss://example.com.
        /// If no URL schema is included, ws:// is assumed.
        /// 
        /// When using a URL, a username and password can be included in the URL, such as ws://user:pass@localhost:8081.
        /// For security reasons, it is recommended to use a secure connection (wss://) and a password.
        server_addr: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = Cli::parse();

    match args.command {
        Command::Serve { bind } => {
            use servicebridge::server;

            server::serve(bind).await?;
        },
        Command::Expose { bind, address } => {
            use servicebridge::expose;

            expose::serve(bind, util::parse_address(&address).await?).await?;
        },
        Command::Connect { bind, server_addr, name } => {
            use servicebridge::client;

            let server_addr = format!("{}/connect?name={}", build_url_base(&server_addr, false)?, name);
            client::serve(bind, server_addr.try_into()?).await?;
        },
        Command::Relay { exposed_addr, server, name } => {
            use servicebridge::bridge;
            let exposed_addr = format!("ws://{}", exposed_addr);
            let server_addr = format!("{}register?name={}", build_url_base(&server, false)?, name);
            bridge::bridge(server_addr.try_into()?, exposed_addr.try_into()?).await?;
        },
        Command::List { server_addr } => {
            use futures::StreamExt;
            let server_addr = format!("{}/list", build_url_base(&server_addr, false)?);
            let (mut ws_server_stream, _) = tokio_tungstenite::connect_async(&server_addr).await?;

            while let Some(msg) = ws_server_stream.next().await {
                let msg = msg?;
                let infos: Vec<servicebridge::protocol::ExposerInfo> = serde_json::from_str(msg.to_text()?)?;

                println!("{:?}", infos);
            }
        },
    }

    Ok(())


}
