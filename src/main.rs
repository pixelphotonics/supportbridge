use clap::{Parser, Subcommand};
use tungstenite::http::Uri;
use std::net::SocketAddr;

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
        #[clap(long, default_value = "[::]:8080")]
        bind: SocketAddr,
    },

    /// Run the websocket-to-TCP bridge
    Expose {
        /// The Ip address:port combination to listen on.
        #[clap(long, default_value = "[::]:8080")]
        bind: SocketAddr,

        /// The address of the TCP server to connect to
        address: SocketAddr,
    },

    /// Connect to a server and bind a local TCP port.
    Connect {
        /// The Ip address:port combination to listen on.
        #[clap(long, default_value = "[::]:8080")]
        bind: SocketAddr,

        /// The address of the central server to connect to
        server_addr: Uri,

        /// Name of the exposed machine
        name: String,
    },

    /// Connect an exposed websocket-to-TCP bridge with a server
    Relay {
        exposed_addr: Uri,
        server_addr: Uri,

        /// Name of the exposed machine
        name: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();

    let args = Cli::parse();

    match args.command {
        Command::Serve { bind } => {
            use servicebridge::server;
            let config = server::ServerConfig {
                listen_addr: bind,
            };

            server::serve(config).await?;
        
        },
        Command::Expose { bind, address } => {
            use servicebridge::expose;

            expose::serve(bind, address).await?;
        },
        Command::Connect { bind, server_addr, name } => {
            
        },
        Command::Relay { exposed_addr, server_addr, name } => {
            use servicebridge::bridge;
            bridge::bridge(server_addr, exposed_addr).await?;
        },
    }

    Ok(())


}
