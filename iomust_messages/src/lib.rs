use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

/// Messages sent from the clients to the signaling server.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ClientMessage {
    Alive,
    Bye,
    Hey { sample_rate: u32 },
}

/// Messages sent from the signaling server to the clients.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ServerMessage {
    Entered { addr: SocketAddr, sample_rate: u32 },
    Left { addr: SocketAddr },
    RequestAudioAddress { key: u64 },
    SuccessfullyEntered,
}
