//! Protocol definitions for the server and client
//! 
//! The protocol is kept very simple and relies on websockets for communication.
//! 
//! The communication with the server is done via HTTP GET requests with the following paths:
//!     
//!  - `/register?name=<name>`: Register a new exposer with the given name
//!  - `/connect?name=<name>`: Connect to an existing exposer with the given name
//!  - `/list`: List all currently registered exposer
//! 
//! This allows to implement authorization using a reverse proxy based on the different paths.
//! For example, the `/connect` path could be protected by a password, while the `/register`
//! path is open to everyone (e.g. customers can connect to the exposer, but only the service
//! engineer who needs to provide remote support can connect to the registered customers).
//! 
//! The typical communication flow is as follows:
//! 
//! * The exposer connects to the server (`/register?name=<name>`) and registers itself
//!   with a name. The server keeps the websocket connection open and waits for incoming
//!   connections.
//! * Clients can now connect to the server either through an opened port (if the server was
//!   started with the `--open_ports` option), or via a websocket connection on the
//!   path `/connect?name=<name>`.
//! * As soon as a client initiates a connection, the server sends `init` as websocket text
//!   message to the exposer, indicating that an existing connection should
//!   be closed and a new connection to the target needs to be established. The exposer
//!   answers with an `ack` text message.
//! * After this, all binary messages from the client are forwarded to the exposer (which
//!   forwards them to the target TCP connection) and vice versa.
//! 

use std::collections::HashMap;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tungstenite::http::Uri;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientInfo {
    pub peer_addr: String,
    pub uses_port: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExposerInfo {
    pub name: String,

    /// Time when the channel was opened
    pub open_time: String,
    pub peer_addr: String,
    pub open_port: Option<u16>,

    /// The name of the connected client
    pub connected_client: Option<ClientInfo>,
}

pub enum ServerPath {
    Register { name: String },
    Connect { name: String },
    List,
}

/// Parse the query string into a HashMap
fn parse_query(query: &str) -> HashMap<String, String> {
    query
        .split('&')
        .filter_map(|pair| {
            let mut parts = pair.split('=');

            let key = parts.next();
            let value = parts.next();

            if let Some((k, v)) = key.zip(value) {
                Some((k.to_string(), v.to_string()))
            } else {
                None
            }
        })
        .collect()
}

impl ServerPath {
    pub fn from_uri(uri: &Uri) -> Result<Self> {
        let path = uri.path();
        let query = uri.query().unwrap_or("");

        let query = parse_query(query);

        match path {
            "/register" => {
                let name = query.get("name").ok_or(anyhow::anyhow!("Name not found"))?;
                Ok(ServerPath::Register { name: name.clone() })
            }
            "/connect" => {
                let name = query.get("name").ok_or(anyhow::anyhow!("Name not found"))?;
                Ok(ServerPath::Connect { name: name.clone() })
            }
            "/list" => Ok(ServerPath::List),
            _ => Err(anyhow::anyhow!("Unknown path: {}", path)),
        }
    }
}


impl std::fmt::Display for ServerPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServerPath::Register { name } => {
                write!(f, "register?name={}", name)
            }
            ServerPath::Connect { name } => {
                write!(f, "connect?name={}", name)
            }
            ServerPath::List => {
                write!(f, "list")
            },
        }
    }
}