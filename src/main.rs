use std::path::PathBuf;

use anyhow::anyhow;
use clap::{Parser, Subcommand};
use supportbridge::{protocol::ExposedAddress, util::parse_bind_address};

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
        /// The Ip address:port combination to listen on. If only a port number is given, the server will listen on [::], which will listen to all interfaces (Ipv4 and Ipv6) by default on Linux.
        #[clap(long, default_value = "[::]:8091")]
        bind: String,

        /// Don't overwrite existing channels when a new exposer connection is made with the same name.
        /// By default, the server will close the existing connection to the exposer and allow the new exposer to take its place.
        #[arg(short = 'e', long)]
        dont_overwrite_exposer: bool,

        /// Don't overwrite existing connection to channels when a new exposer connection is made.
        ///
        /// By default, the server will close the existing connection to the channel and allow the new client to connect to the exposer.
        #[arg(short = 'c', long)]
        dont_overwrite_connection: bool,

        /// The minimum port number to use when opening ports on the server.
        #[arg(long, default_value = "11000")]
        min_port: u16,

        /// The maximum port number to use when opening ports on the server.
        #[arg(long, default_value = "64000")]
        max_port: u16,

        /// Optional path to the HTML file to serve at the server root. If not given, the server will serve a simple default page.
        #[arg(short = 'r', long)]
        html: Option<PathBuf>,
    },

    /// Run the websocket-to-TCP bridge
    Expose {
        /// The Ip address:port combination to listen on. If only a port number is given, the server will listen on [::], which will listen to all interfaces (Ipv4 and Ipv6) by default on Linux.
        #[clap(long, default_value = "[::]:8092")]
        bind: String,

        /// The address of the TCP server to connect to. Can be a hostname or IP address. A port can be specified with a colon.
        #[arg(required = true)]
        target: Vec<String>,

        /// Optionally, the address of the central server to connect to. Can be a hostname or IP address. A port can be specified with a colon.
        /// If this is passed, the exposer will register itself with the server instead of listening for websocket connections.
        #[arg(short = 's', long)]
        server: Option<String>,

        /// Name of the exposed machine to register on the server. If not given, the system hostname will be used.
        #[arg(short = 'n', long)]
        name: Option<String>,
    },

    /// Connect an exposed websocket-to-TCP bridge with a server
    Relay {
        /// The address of the device where the WS-to-TCP bridge is running. Can be a hostname or IP address. A port can be specified with a colon.
        exposed_addr: String,

        /// The address of the central server to connect to. Can be a hostname or IP address. A port can be specified with a colon.
        /// Can also be a websocket URL, such as ws://localhost:8091 or wss://example.com.
        /// If no URL schema is included, ws:// is assumed.
        ///
        /// When using a URL, a username and password can be included in the URL, such as ws://user:pass@localhost:8091.
        /// For security reasons, it is recommended to use a secure connection (wss://) and a password.
        server: String,

        /// The exposed machine will be registered with the server under the given name.
        name: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = Cli::parse();

    match args.command {
        Command::Serve {
            bind,
            dont_overwrite_exposer,
            dont_overwrite_connection,
            min_port,
            max_port,
            html,
        } => {
            use supportbridge::server;

            let server_options = server::ServerOptions {
                listen_addr: parse_bind_address(&bind)?,
                port_range: min_port..=max_port,
                overwrite_existing_connection: !dont_overwrite_connection,
                overwrite_existing_exposer: !dont_overwrite_exposer,
                root_file: html,
            };

            server::serve(server_options).await?;
        }
        Command::Expose { bind, target, server, name } => {
            use supportbridge::expose;

            let target_addr = target.into_iter().map(|t| ExposedAddress::try_from(t).map_err(|e| anyhow!(e))).collect::<Result<_, _>>()?;

            if let Some(server) = server {
                let name = match name {
                    Some(name) => name,
                    None => hostname::get()?.into_string().unwrap_or_else(|_| "unknown".to_string()),
                };
                
                expose::expose_and_register(server, target_addr, name).await?;
                return Ok(());
            } else {
                expose::listen_to_ws(parse_bind_address(&bind)?, target_addr).await?;
            }            
        }
        Command::Relay {
            exposed_addr,
            server,
            name,
        } => {
            use supportbridge::bridge;
            bridge::bridge(server, name, exposed_addr).await?;
        }
    }

    Ok(())
}
