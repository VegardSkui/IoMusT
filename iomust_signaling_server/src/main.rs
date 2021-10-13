use std::collections::HashMap;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, RwLock};

use iomust_signaling_messages::{ClientMessage, ServerMessage};
use serde::Deserialize;

struct Peer {
    /// The audio UDP address of the peer. The existence of this address is equivalent to the peer
    /// being connected.
    addr: Option<SocketAddr>,
    sample_rate: Option<u32>,
    stream: TcpStream,
}

fn main() {
    // Initialize logging
    env_logger::init();
    log::info!("launching iomust_signaling_server");

    // Read the address we should bind to from the program arguments
    let addr = std::env::args().nth(1).expect("missing address argument");

    // Bind the the given address and listen for TCP connections
    let listener = TcpListener::bind(addr).expect("could not bind to address");
    log::info!("bound to `{}`", listener.local_addr().unwrap());

    let peers = Arc::new(RwLock::new(HashMap::<SocketAddr, Peer>::new()));

    std::thread::spawn({
        let peers = peers.clone();
        move || loop {
            // Get a write lock on `peers`
            let mut peers = peers.write().unwrap();

            // Read incoming client messages from each of the peer streams. Each entry is a tuple
            // of the source address and the message itself.
            let mut incoming: Vec<(SocketAddr, ClientMessage)> = Vec::new();
            let mut closed: Vec<SocketAddr> = Vec::new();
            for (addr, peer) in peers.iter() {
                // Create a JSON deserializer for the stream. It should be okay to clone the stream
                // here many times (for each iteration) since the stream is not written or read to
                // outside of this thread. We should therefore avoid issues arising from multiple
                // readers/writers at the same time.
                let stream = peer.stream.try_clone().expect("could not clone stream");
                let mut de = serde_json::Deserializer::from_reader(stream);

                // TODO: Each peer may have multiple incoming messages queued so we should actually
                // try to read from each client until a WouldBlock error occurs.

                match ClientMessage::deserialize(&mut de) {
                    Ok(message) => incoming.push((*addr, message)),
                    Err(e) => {
                        // Ignore WouldBlock IO errors as these are expected from silent peers when
                        // using non-blocking streams
                        if e.is_io() {
                            let e = std::io::Error::from(e);
                            if e.kind() != std::io::ErrorKind::WouldBlock {
                                log::warn!("disconnecting `{}` due to error: {}", addr, e);
                                closed.push(*addr);
                            }
                        } else {
                            log::warn!("disconnecting `{}` due to error: {}", addr, e);
                            closed.push(*addr);
                        }
                    }
                }
            }

            // While handling incoming messages we build up a vector of outgoing messages. Each
            // entry is a tuple of the destination address and the message itself.
            let mut outgoing: Vec<(SocketAddr, ServerMessage)> = Vec::new();

            // Remove closed peers (and thus actually closing the connections) and send disconnect
            // messages to other peers connected with audio if the newly closed peer was connected
            // with audio
            for addr in closed {
                if let Some(peer) = peers.remove(&addr) {
                    if let Some(audio_addr) = peer.addr {
                        for (peer_addr, peer) in peers.iter() {
                            if peer.addr.is_some() {
                                let message = ServerMessage::Disconnected { addr: audio_addr };
                                outgoing.push((*peer_addr, message));
                            }
                        }
                    }
                }
            }

            // Process each of the received client messages
            for (source_addr, message) in incoming {
                match message {
                    ClientMessage::Alive => todo!(),
                    ClientMessage::Bye => todo!(),
                    ClientMessage::Hey {
                        name,
                        port,
                        sample_rate,
                    } => {
                        // TODO: Make sure the peer isn't already connected with audio

                        // The actual audio address of the peer is it's source address and the
                        // provided audio port number
                        let mut addr = source_addr;
                        addr.set_port(port);

                        log::info!(
                            "`{}` connected as `{}` using audio addr `{}`",
                            source_addr,
                            name,
                            addr
                        );

                        for (peer_addr, peer) in peers.iter() {
                            if let Some(peer_audio_addr) = peer.addr {
                                // Message the newly connected peer about each already connected
                                // peer
                                let message = ServerMessage::Connected {
                                    addr: peer_audio_addr,
                                    sample_rate: peer.sample_rate.expect("missing sample_rate"),
                                };
                                outgoing.push((source_addr, message));

                                // Send a message to all already connected peers about the newly
                                // connected peer
                                let message = ServerMessage::Connected { addr, sample_rate };
                                outgoing.push((*peer_addr, message));
                            }
                        }

                        // Store the audio address and sample rate for the peer
                        if let Some(peer) = peers.get_mut(&source_addr) {
                            peer.addr = Some(addr);
                            peer.sample_rate = Some(sample_rate);
                        }
                    }
                }
            }

            // Send each of the outgoing messages
            for (dest_addr, message) in outgoing {
                if let Some(peer) = peers.get(&dest_addr) {
                    // Create a JSON serializer for the peer's stream. Again, it should be okay to
                    // clone the stream for the same reasons as given in the previous loop.
                    let stream = peer.stream.try_clone().expect("could not clone stream");

                    log::trace!("sending message to `{}`: {:?}", dest_addr, message);

                    // Serialize the message and send it to the destination
                    serde_json::to_writer(stream, &message)
                        .expect("serializing and writing message failed");
                }
            }

            // Release the lock on `peers` before sleeping a bit until the next iteration
            drop(peers);
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    });

    // Accept all incoming connections
    for connection in listener.incoming() {
        match connection {
            Ok(stream) => {
                let addr = stream.peer_addr().unwrap();
                log::info!("incoming connection from `{}`", addr);

                // Make the stream non-blocking such that it doesn't freeze our worker thread when
                // clients are silent
                stream
                    .set_nonblocking(true)
                    .expect("set_nonblocking call failed");

                // Store the peer
                let peer = Peer {
                    addr: None,
                    sample_rate: None,
                    stream,
                };
                peers.write().unwrap().insert(addr, peer);
            }
            Err(e) => log::warn!("connection failed: {}", e),
        }
    }
}
