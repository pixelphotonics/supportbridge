use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExposerInfo {
    pub name: String,
    pub connection_time: String,
    pub connected_client: Option<String>,
    pub peer_addr: String,
}