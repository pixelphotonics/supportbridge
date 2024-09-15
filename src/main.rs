use clap::{Parser, Subcommand};
use supportbridge::util::{self, parse_bind_address};

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
        #[clap(long, default_value = "[::]:8081")]
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
        /// The Ip address:port combination to listen on. If only a port number is given, the server will listen on [::], which will listen to all interfaces (Ipv4 and Ipv6) by default on Linux.
        #[clap(long, default_value = "[::]:8082")]
        bind: String,

        /// The address of the TCP server to connect to. Can be a hostname or IP address. A port can be specified with a colon.
        target: String,

        /// Optionally, the address of the central server to connect to. Can be a hostname or IP address. A port can be specified with a colon.
        /// If this is passed, the exposer will register itself with the server instead of listening for websocket connections.
        #[arg(short = 's', long)]
        server: Option<String>,
    },

    /// Open a local TCP port which maps to an exposer <name> via the supportbridge <server>.
    Open {
        /// The Ip address:port combination to listen on. If only a port number is given, the server will listen on [::], which will listen to all interfaces (Ipv4 and Ipv6) by default on Linux.
        #[clap(long, default_value = "[::]:8083")]
        bind: String,

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

        /// The exposed machine will be registered with the server under the given name.
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

        /// Print verbose information
        #[arg(short, long)]
        verbose: bool,
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
            open_ports,
            min_port,
            max_port,
        } => {
            use supportbridge::server;

            let server_options = server::ServerOptions {
                listen_addr: parse_bind_address(&bind)?,
                open_port: open_ports,
                port_range: min_port..=max_port,
                overwrite_existing_connection: !dont_overwrite_connection,
                overwrite_existing_exposer: !dont_overwrite_exposer,
            };

            server::serve(server_options).await?;
        }
        Command::Expose { bind, target, server } => {
            use supportbridge::expose;

            // todo: implement server registration
            expose::serve(parse_bind_address(&bind)?, util::parse_address(&target).await?).await?;
        }
        Command::Open {
            bind,
            server,
            name,
        } => {
            use supportbridge::client;
            client::serve(parse_bind_address(&bind)?, server, name).await?;
        }
        Command::Relay {
            exposed_addr,
            server,
            name,
        } => {
            use supportbridge::bridge;
            bridge::bridge(server, name, exposed_addr.try_into()?).await?;
        }
        Command::List {
            server_addr,
            verbose,
        } => {
            use futures::StreamExt;
            let request = util::build_request(&server_addr, supportbridge::protocol::ServerPath::List)?;
            let (mut ws_server_stream, _) = tokio_tungstenite::connect_async(request).await?;

            while let Some(msg) = ws_server_stream.next().await {
                let msg = msg?;
                match msg {
                    tungstenite::Message::Text(textmsg) => {
                        if let Ok(infos) = serde_json::from_str::<
                            Vec<supportbridge::protocol::ExposerInfo>,
                        >(&textmsg)
                        {
                            // Only for feature "tabwriter"
                            #[cfg(feature = "tabwriter")]
                            {
                                let mut table = infos
                                    .iter()
                                    .map(|channel| {
                                        format!(
                                            "| {}\t| {}\t| {}\t| {}\t| {} \t|",
                                            channel.name,
                                            channel.peer_addr,
                                            channel.open_time,
                                            channel
                                                .open_port
                                                .map(|p| p.to_string())
                                                .unwrap_or("-".to_string()),
                                            match &channel.connected_client {
                                                Some(client) => format!(
                                                    "({}) {}",
                                                    if client.uses_port { "P" } else { "W" },
                                                    client.peer_addr
                                                ),
                                                None => format!("-"),
                                            }
                                        )
                                    })
                                    .collect::<Vec<_>>();

                                table.insert(
                                    0,
                                    "| Name\t| Peer\t| Open since\t| Server port\t| Occupied \t|"
                                        .to_string(),
                                );
                                table.insert(
                                    1,
                                    "| ====\t| ====\t| ==========\t| ===========\t| ======== \t|"
                                        .to_string(),
                                );

                                use std::io::Write;
                                let mut tw = tabwriter::TabWriter::new(std::io::stdout());
                                tw.write_all(table.join("\n").as_bytes())?;
                                tw.flush()?;
                                println!("")
                            }

                            #[cfg(not(feature = "tabwriter"))]
                            println!("List: {:?}", infos);

                            break;
                        } else {
                            log::warn!("Invalid message from server: {}", textmsg);
                        }
                    }
                    tungstenite::Message::Binary(_) => {
                        log::warn!("Invalid message from server: binary");
                    }
                    tungstenite::Message::Close(_) => {
                        break;
                    }
                    _ => {
                        log::trace!("Other message type");
                    }
                }
            }
        }
    }

    Ok(())
}
