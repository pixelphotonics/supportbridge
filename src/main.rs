use clap::{Parser, Subcommand};
use tungstenite::http::Uri;
use std::net::SocketAddr;
use servicebridge::{server, util};

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
        server_addr: String,

        /// Name of the exposed machine
        name: String,
    },

    /// Connect an exposed websocket-to-TCP bridge with a server
    Relay {
        /// The address of the device where the WS-to-TCP bridge is running. Can be a hostname or IP address. A port can be specified with a colon.
        exposed_addr: String,

        /// The address of the central server to connect to. Can be a hostname or IP address. A port can be specified with a colon.
        server: String,

        /// Name of the exposed machine
        name: String,
    },

    /// List all open tunnels on the central server
    List {
        /// The address of the central server to connect to. Can be a hostname or IP address. A port can be specified with a colon.
        server_addr: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();

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

            let server_addr = format!("ws://{}/connect?name={}", server_addr, name);
            client::serve(bind, server_addr.try_into()?).await?;
        },
        Command::Relay { exposed_addr, server, name } => {
            use servicebridge::bridge;
            let exposed_addr = format!("ws://{}", exposed_addr);
            let server_addr = format!("ws://{}/register?name={}", server, name);
            bridge::bridge(server_addr.try_into()?, exposed_addr.try_into()?).await?;
        },
        Command::List { server_addr } => {
            use futures::{StreamExt};
            let server_addr = format!("ws://{}/list", server_addr);
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
