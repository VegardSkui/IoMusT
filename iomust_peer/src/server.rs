use std::io::Write;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};

use iomust_messages::{ClientMessage, ServerMessage};
use serde::Deserialize;

pub struct ServerConnectionBuilder<A: ToSocketAddrs> {
    addr: A,
    entered_callback: Option<Box<dyn FnMut(SocketAddr, u32) + Send>>,
    left_callback: Option<Box<dyn FnMut(SocketAddr) + Send>>,
}

impl<A: ToSocketAddrs> ServerConnectionBuilder<A> {
    pub fn new(addr: A) -> ServerConnectionBuilder<A> {
        ServerConnectionBuilder {
            addr,
            entered_callback: None,
            left_callback: None,
        }
    }

    /// Sets the callback which will be called when a new peer enters.
    pub fn entered_callback(
        mut self,
        entered_callback: impl FnMut(SocketAddr, u32) + Send + 'static,
    ) -> ServerConnectionBuilder<A> {
        self.entered_callback = Some(Box::new(entered_callback));
        self
    }

    /// Sets the callback which will be called when a peer leaves.
    pub fn left_callback(
        mut self,
        left_callback: impl FnMut(SocketAddr) + Send + 'static,
    ) -> ServerConnectionBuilder<A> {
        self.left_callback = Some(Box::new(left_callback));
        self
    }

    /// Connects to the server and enters with the given audio socket and sample rate.
    pub fn connect_and_enter(self, socket: UdpSocket, sample_rate: u32) -> std::io::Result<()> {
        // Open a TCP connection to the server
        let mut stream = TcpStream::connect(&self.addr)?;
        log::info!(
            "connected to iomust server at `{}` using addr `{}`",
            stream.peer_addr().unwrap(),
            stream.local_addr().unwrap()
        );

        // Start the entering process by sending a hey message
        send(&mut stream, &ClientMessage::Hey { sample_rate })?;

        // Create a JSON deserializer for incoming server messages
        let mut de = serde_json::Deserializer::from_reader(stream);

        // Wait for the audio address request from the server, and send a UDP request with the
        // received key when it arrives
        if let ServerMessage::RequestAudioAddress { key } =
            ServerMessage::deserialize(&mut de).expect("deserializing server message failed")
        {
            socket.send_to(&key.to_le_bytes(), self.addr)?;
        } else {
            panic!("expected audio address request");
        }

        // TODO: Try again if we don't recive a SuccessfullyEntered message within X seconds, the
        // UDP datagram might have been lost.

        // Wait for the server to notify us of a successfully entering
        if ServerMessage::deserialize(&mut de).expect("deserializing server message failed")
            != ServerMessage::SuccessfullyEntered
        {
            panic!("expected successfully entered message");
        }
        log::debug!("successfully entered");

        // Start a new thread to read incoming messages from the server as they arrive and pass them
        // on to the appropriate callbacks
        let mut entered_callback = self.entered_callback;
        let mut left_callback = self.left_callback;
        std::thread::spawn(move || loop {
            match ServerMessage::deserialize(&mut de) {
                Ok(message) => match message {
                    ServerMessage::Entered { addr, sample_rate } => {
                        // Call the entered callback, if it is set
                        if let Some(entered_callback) = &mut entered_callback {
                            entered_callback(addr, sample_rate);
                        }
                    }
                    ServerMessage::Left { addr } => {
                        // Call the left callback, if it is set
                        if let Some(left_callback) = &mut left_callback {
                            left_callback(addr);
                        }
                    }
                    message => {
                        log::warn!("got unexpected server message: {:?}", message)
                    }
                },
                Err(err) if err.is_eof() => panic!("lost server connection"),
                Err(err) => log::error!("deserializing server message failed: {}", err),
            }
        });

        Ok(())
    }
}

/// Serializes and sends a [`ClientMessage`] into a [`TcpStream`].
fn send(stream: &mut TcpStream, message: &ClientMessage) -> std::io::Result<()> {
    let message = serde_json::to_string(message)?;
    stream.write(message.as_bytes()).map(|_| ())
}
