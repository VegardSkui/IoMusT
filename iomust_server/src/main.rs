use std::collections::{hash_map, HashMap};
use std::error::Error;
use std::net::SocketAddr;
use std::sync::Arc;

use iomust_messages::{ClientMessage, ServerMessage};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{mpsc, oneshot, Mutex};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Initialize logging
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    log::info!("launching iomust_server");

    // Read the address we should bind to from the program arguments
    let addr = std::env::args().nth(1).expect("missing address argument");

    // Create the shared state
    let state = Arc::new(Mutex::new(State::new()));

    // Bind a UDP socket for the audio address request service
    let socket = UdpSocket::bind(&addr).await?;
    log::info!("udp listening on `{}`", socket.local_addr()?);

    // Bind a TCP listener for incoming peer connections.
    let listener = TcpListener::bind(&addr).await?;
    log::info!("tcp listening on `{}`", listener.local_addr()?);

    let mut buf = [0; 8];
    loop {
        // Asynchronously wait for an inbound audio address or peer connection
        tokio::select! {
            Ok((8, addr)) = socket.recv_from(&mut buf) =>{
                // Decode the audio address request key
                let key = u64::from_le_bytes(buf);

                // Send the audio address of the peer if a request exists for the key
                if let Some(tx) = state.lock().await.audio_addr_requests.remove(&key) {
                    tx.send(addr).expect("failed to send audio address");
                }
            }
            Ok((stream, addr)) = listener.accept() => {
                let state = Arc::clone(&state);

                // Spawn an asynchronous task to handle the peer
                tokio::spawn(async move {
                    log::debug!("accepted connection from `{}`", addr);
                    if let Err(e) = process(state, stream, addr).await {
                        log::warn!("an error occured for `{}`: {:?}", addr, e);
                    }
                    log::debug!("closed connected to `{}`", addr);
                });
            }
        }
    }
}

/// Present peer state.
struct PresentPeer {
    tx: mpsc::UnboundedSender<ServerMessage>,
    audio_addr: SocketAddr,
    sample_rate: u32,
}

/// Shared state.
struct State {
    present_peers: HashMap<SocketAddr, PresentPeer>,
    audio_addr_requests: HashMap<u64, oneshot::Sender<SocketAddr>>,
}

impl State {
    fn new() -> Self {
        State {
            present_peers: HashMap::new(),
            audio_addr_requests: HashMap::new(),
        }
    }
}

async fn process(
    state: Arc<Mutex<State>>,
    mut stream: TcpStream,
    addr: SocketAddr,
) -> Result<(), Box<dyn Error>> {
    // Allocate a buffer for incoming client messages. Note that we do not expect messages longer
    // than 1024 bytes.
    let mut buf = [0; 1024];

    // Wait for a hey message from the peer to begin the process of entering
    let amt = stream.read(&mut buf).await?;
    let message: ClientMessage = serde_json::from_slice(&buf[..amt])?;
    let (audio_addr, sample_rate) = if let ClientMessage::Hey { sample_rate, .. } = message {
        // Create a oneshot channel to receive the audio address of the peer
        let (tx, rx) = oneshot::channel();

        // Generate an unique audio address request key and insert it into the map of audio address
        // requests along with the sender half of the oneshot channel
        let key = loop {
            let key: u64 = rand::random();

            let mut state = state.lock().await;
            if let hash_map::Entry::Vacant(entry) = state.audio_addr_requests.entry(key) {
                entry.insert(tx);
                break key;
            }
        };

        // Request the peer to tell us about their audio adress using the key generated above
        send_message(&mut stream, &ServerMessage::RequestAudioAddress { key }).await?;

        // Wait for the audio address to be received
        // TODO: Alternatively time out after X seconds
        (rx.await?, sample_rate)
    } else {
        // Error if the peer sent something else first
        return Err("expected hey first".into());
    };

    // Notify the peer that they have entered successfully
    send_message(&mut stream, &ServerMessage::SuccessfullyEntered).await?;

    log::info!(
        "`{}` entered with audio address `{}` and sample rate {}",
        addr,
        audio_addr,
        sample_rate
    );

    // Create a channel for sending server messages to the peer
    let (tx, mut rx) = mpsc::unbounded_channel();

    {
        // Aquire a lock on the shared state while telling peers about each other to avoid race
        // conditions leading to missed connections and differing peer states
        let mut state = state.lock().await;

        for peer in state.present_peers.values() {
            // Tell the existing peer about the newly entered peer
            peer.tx.send(ServerMessage::Entered {
                addr: audio_addr,
                sample_rate,
            })?;

            // Tell the new peer about the already present peer
            send_message(
                &mut stream,
                &ServerMessage::Entered {
                    addr: peer.audio_addr,
                    sample_rate: peer.sample_rate,
                },
            )
            .await?;
        }

        // Store the newly entered peer
        state.present_peers.insert(
            addr,
            PresentPeer {
                tx,
                audio_addr,
                sample_rate,
            },
        );
    }

    loop {
        // Asynchronously wait for server messages destined for the peer or incoming client messages
        // received from them
        tokio::select! {
            Some(message) = rx.recv() => send_message(&mut stream, &message).await?,
            result = stream.read(&mut buf) => match result {
                Ok(0) | Err(_) => break,
                Ok(amt) => {
                    // We do not expect any more messages from the peer
                    println!("got message of length {}", amt);
                },
            }
        }
    }

    // If this point is reach, something went wrong and the connection to the peer has been lost (or
    // they disconnected). Remove the peer and tell the remaining peers about the peer having left.
    {
        let mut state = state.lock().await;
        state.present_peers.remove(&addr);

        for peer in state.present_peers.values() {
            peer.tx.send(ServerMessage::Left { addr: audio_addr })?;
        }
    }

    Ok(())
}

/// Serializes and sends a [`ServerMessage`].
async fn send_message(stream: &mut TcpStream, message: &ServerMessage) -> std::io::Result<()> {
    log::trace!(
        "sending message to `{}`: {:?}",
        stream.peer_addr().unwrap(),
        message
    );
    let message = serde_json::to_string(message)?;
    (*stream).write(message.as_bytes()).await.map(|_| ())
}
