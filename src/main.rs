use clap::{Parser, Subcommand};
use std::net::SocketAddr;
use supportbridge::util;
use util::build_url_base;

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

        /// Don't overwrite existing channels when a new exposer connection is made with the same name.
        /// By default, the server will close the existing connection to the exposer and allow the new exposer to take its place.
        #[arg(short='e', long)]
        dont_overwrite_exposer: bool,

        /// Don't overwrite existing connection to channels when a new exposer connection is made.
        /// 
        /// By default, the server will close the existing connection to the channel and allow the new client to connect to the exposer.
        #[arg(short='c', long)]
        dont_overwrite_connection: bool,

        /// Open a TCP port on the server to which connections can be made directly.
        /// This allows third-party tools to connect to the exposer directly by connecting to the opened port on the server.
        /// If this is false (the default), an additional client instance must be run to connect to the exposer via the server.
        #[arg(short, long)]
        open_ports: bool,

        /// The minimum port number to use when opening ports on the server.
        #[arg(long, default_value = "11000")]
        min_port: u16,

        /// The maximum port number to use when opening ports on the server.
        #[arg(long, default_value = "64000")]
        max_port: u16,
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
        Command::Serve { bind, dont_overwrite_exposer, dont_overwrite_connection, open_ports, min_port, max_port } => {
            use supportbridge::server;

            let server_options = server::ServerOptions {
                listen_addr: bind,
                open_port: open_ports,
                port_range: min_port..=max_port,
                overwrite_existing_connection: !dont_overwrite_connection,
                overwrite_existing_exposer: !dont_overwrite_exposer,
            };

            server::serve(server_options).await?;
        },
        Command::Expose { bind, address } => {
            use supportbridge::expose;

            expose::serve(bind, util::parse_address(&address).await?).await?;
        },
        Command::Connect { bind, server_addr, name } => {
            use supportbridge::client;
            client::serve(bind, server_addr, name).await?;
        },
        Command::Relay { exposed_addr, server, name } => {
            use supportbridge::bridge;
            bridge::bridge(server, name, exposed_addr.try_into()?).await?;
        },
        Command::List { server_addr } => {
            use futures::StreamExt;
            let server_addr = format!("{}list", build_url_base(&server_addr, false)?);
            let (mut ws_server_stream, _) = tokio_tungstenite::connect_async(&server_addr).await?;

            while let Some(msg) = ws_server_stream.next().await {
                let msg = msg?;
                let infos: Vec<supportbridge::protocol::ExposerInfo> = serde_json::from_str(msg.to_text()?)?;

                println!("{:?}", infos);
            }
        },
    }

    Ok(())


}
