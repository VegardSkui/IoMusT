use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

/// Messages sent from the clients to the signaling server.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ClientMessage {
    Alive,
    Bye,
    Hey { name: String, port: u16 },
}

/// Messages sent from the signaling server to the clients.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ServerMessage {
    Connected { addr: SocketAddr },
    Disconnected { addr: SocketAddr },
}
