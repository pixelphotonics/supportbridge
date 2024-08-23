use std::collections::HashMap;

use serde::{Serialize, Deserialize};
use tungstenite::http::Uri;
use anyhow::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExposerInfo {
    pub name: String,
    pub connection_time: String,
    pub connected_client: Option<String>,
    pub peer_addr: String,
}

pub enum ServerPath {
    Register {
        name: String,
    },
    Connect {
        name: String,
    },
    List,

    /*/// Free an existing connection and allow a new connection to be opened to the exposed port
    Free {
        name: String,
    },*/
}



/// Parse the query string into a HashMap
fn parse_query(query: &str) -> HashMap<String, String> {
    query.split('&').filter_map(|pair| {
        let mut parts = pair.split('=');
        
        let key = parts.next();
        let value = parts.next();

        if key.is_none() || value.is_none() {
            None
        } else {
            Some((key.unwrap().to_string(), value.unwrap().to_string()))
        }
        
    }).collect()
}

impl ServerPath {
    pub fn from_uri(uri: &Uri) -> Result<Self> {
        let path = uri.path();
        let query = uri.query().unwrap_or("");

        let query = parse_query(query);

        match path {
            "/register" => {
                let name = query.get("name").ok_or(anyhow::anyhow!("Name not found"))?;
                Ok(ServerPath::Register {
                    name: name.clone(),
                })
            },
            "/connect" => {
                let name = query.get("name").ok_or(anyhow::anyhow!("Name not found"))?;
                Ok(ServerPath::Connect {
                    name: name.clone(),
                })
            },
            "/list" => {
                Ok(ServerPath::List)
            },
            _ => Err(anyhow::anyhow!("Unknown path: {}", path)),
        }
    }

    pub fn to_string(&self) -> String {
        match self {
            ServerPath::Register { name } => {
                format!("register?name={}", name)
            },
            ServerPath::Connect { name } => {
                format!("connect?name={}", name)
            },
            ServerPath::List => {
                "list".to_string()
            },
        }
    }
}