//! Protocol definitions for the server and client
//! 
//! The protocol is kept very simple and relies on websockets for communication.
//! 
//! The exchange between exposer, server and client is done using small JSON messages. The
//! message types are encoded in the `JsonMessage` enum and the type is tagged with the `type`
//! field. The messages are serialized using `serde_json` and sent as text messages over the
//! websocket connection.
//!  
//! 
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

pub type ChannelId = u8;

/// Message sent between client and exposer.
/// As a websocket message, this is just the channel id (single byte) followed by the data.
pub struct BinaryMessage<'a> {
    pub channel_id: ChannelId,
    pub data: &'a [u8],
}

impl<'a> BinaryMessage<'a> {
    pub fn from_ws(in_data: &'a [u8]) -> Option<Self> {
        if in_data.len() > 1 {
            Some (Self {
                channel_id: in_data[0],
                data: &in_data[1..]
            })
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExposedAddress {
    pub address: String,
    pub port: u16,
}

impl TryFrom<String> for ExposedAddress {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        // Special case: if it's an IPv6 address like "[::1]:80"
        let (host, port_str) = if value.starts_with('[') {
            // IPv6 should be in form "[addr]:port"
            let closing = value.find(']').ok_or("Invalid IPv6 format, missing ']'")?;
            let host = &value[1..closing];
            let rest = &value[(closing + 1)..];
            if !rest.starts_with(':') {
                return Err("Invalid IPv6 format, missing ':' after ']'");
            }
            (host, &rest[1..])
        } else {
            // Normal hostname or IPv4, split on the last colon
            match value.rsplit_once(':') {
                Some((host, port)) => (host, port),
                None => return Err("Missing port separator ':'"),
            }
        };
    
        let port: u16 = port_str.parse().map_err(|_| "Invalid port number")?;
    
        Ok(Self {
            address: host.to_string(),
            port,
        })
    }
}


#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum JsonMessage {
    /// Initiate a new tunnel connection. This is sent from the server to the exposer
    /// once in the beginning.
    /// The exposer will respond etierh with `OpenTunnelSuccess` or `Error`.
    OpenTunnel {
        
    },

    /// A new tunnel was opened. This is sent from the exposer to the server.
    OpenTunnelSuccess {
        exposed: Vec<ExposedAddress>,
    },

    /// Open a new channel. Sent from server to exposer.
    OpenChannel {
        /// The channel id as assigned by the server.
        channel_id: ChannelId,

        /// The port on the exposer to connect to.
        exposed_address: ExposedAddress,
    },

    /// This message is sent when a channel is closed. This can be sent by the exposer or the server.
    CloseChannel {
        /// The channel id to close.
        channel_id: ChannelId,

        /// The reason for closing the channel.
        error: Option<String>,
    },

    /// A new channel was initialized and the given id was assigned.
    OpenChannelSuccessful {
        channel_id: ChannelId,
    },

    /// Channel Error
    Error {
        message: String,
    },
}

impl From<JsonMessage> for tungstenite::Message {
    fn from(message: JsonMessage) -> tungstenite::Message {
        tungstenite::Message::Text(serde_json::to_string(&message).unwrap().into())
    }
}

impl From<JsonMessage> for axum::extract::ws::Message {
    fn from(message: JsonMessage) -> axum::extract::ws::Message {
        axum::extract::ws::Message::Text(serde_json::to_string(&message).unwrap().into())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientInfo {
    pub peer_addr: String,
    pub uses_port: bool,
}


#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExposedServerPort {
    /// The open port on the server
    pub port: u16,

    /// The address / port from the POV of the exposer which this port maps to
    pub exposed_addr: ExposedAddress,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelInfo {
    /// Name of the exposer machine (typically the hostname)
    pub name: String,

    /// Time when the tunnel was opened
    pub open_time: String,

    /// The address of the exposer
    pub peer_addr: String,

    /// Ports on the server
    pub ports: Vec<ExposedServerPort>,

    /// The name of the connected client
    pub channels: Vec<ChannelInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelInfo {

    pub id: ChannelId,

    /// The exposed port on the exposer
    pub exposed: ExposedAddress,

    /// The client address that is currently connected to this channel
    pub peer_addr: String,

    /// The time when the channel was opened
    pub open_time: String,
}

#[derive(Debug)]
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


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exposed_address_try_from_valid_ipv4() {
        let input = "127.0.0.1:8080".to_string();
        let result = ExposedAddress::try_from(input).unwrap();
        assert_eq!(result.address, "127.0.0.1");
        assert_eq!(result.port, 8080);
    }

    #[test]
    fn test_exposed_address_try_from_valid_ipv6() {
        let input = "[::1]:8080".to_string();
        let result = ExposedAddress::try_from(input).unwrap();
        assert_eq!(result.address, "::1");
        assert_eq!(result.port, 8080);
    }

    #[test]
    fn test_exposed_address_try_from_missing_port() {
        let input = "127.0.0.1".to_string();
        let result = ExposedAddress::try_from(input);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "Missing port separator ':'");
    }

    #[test]
    fn test_exposed_address_try_from_invalid_port() {
        let input = "127.0.0.1:abc".to_string();
        let result = ExposedAddress::try_from(input);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "Invalid port number");
    }

    #[test]
    fn test_exposed_address_try_from_invalid_ipv6_format() {
        let input = "[::1".to_string();
        let result = ExposedAddress::try_from(input);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "Invalid IPv6 format, missing ']'");
    }

    #[test]
    fn test_exposed_address_try_from_ipv6_missing_colon() {
        let input = "[::1]8080".to_string();
        let result = ExposedAddress::try_from(input);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "Invalid IPv6 format, missing ':' after ']'");
    }
}