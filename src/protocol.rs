use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize)]
pub struct ExposerInfo {
    pub name: String,
}