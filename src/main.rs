use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Cli {

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the central (public) server
    Serve {
        /// The Ip address:port combination to listen on.
        #[clap(long, default_value_t = String::from("[::]:8080"))]
        address: String,
    },

    /// Run the websocket-to-TCP bridge
    Expose {
        /// The Ip address:port combination to listen on.
        #[clap(long, default_value_t = String::from("[::]:8080"))]
        bind: String,

        /// The address of the TCP server to connect to
        address: String,
    },

    /// Connect to a server and bind a local TCP port.
    Connect {
        /// The Ip address:port combination to listen on.
        #[clap(long, default_value_t = String::from("[::]:8080"))]
        bind: String,

        /// The address of the central server to connect to
        server_addr: String,

        /// Name of the exposed machine
        name: String,
    },

    /// Connect an exposed websocket-to-TCP bridge with a server
    Relay {
        exposed_addr: String,
        server_addr: String,

        /// Name of the exposed machine
        name: String,
    },
}

fn main() {
    env_logger::init();
    
    println!("Hello, world!");


}
