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
    /*/// Free an existing connection and allow a new connection to be opened to the exposed port
    Free {
        name: String,
    },*/
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