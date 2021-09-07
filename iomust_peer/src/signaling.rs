use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};

use iomust_signaling_messages::{ClientMessage, ServerMessage};
use serde::Deserialize;

type Callback = dyn FnMut(ServerMessage) + Send + 'static;

pub struct SignalingConnection {
    stream: TcpStream,
    callback: Arc<Mutex<Option<Box<Callback>>>>,
}

impl SignalingConnection {
    /// Opens a connection to the signaling server located at the given address.
    ///
    /// This will also automatically start another thread to listen for `ServerMessage`s sent to us
    /// from the server.
    pub fn connect<A: ToSocketAddrs>(addr: A) -> Result<Self, std::io::Error> {
        // Open a TCP connection to the signaling server
        let stream = TcpStream::connect(addr)?;
        log::info!(
            "connected to signaling server at `{}` using addr `{}`",
            stream.peer_addr().unwrap(),
            stream.local_addr().unwrap()
        );

        let callback: Arc<Mutex<Option<Box<Callback>>>> = Arc::new(Mutex::new(None));

        // Start a new thread to read messages from the signaling server as they arrive
        std::thread::spawn({
            let callback = callback.clone();
            let stream = stream.try_clone().expect("could not clone stream");
            let mut de = serde_json::Deserializer::from_reader(stream);
            move || loop {
                match ServerMessage::deserialize(&mut de) {
                    Ok(message) => {
                        // Hand the message over to the callback if it's set
                        if let Some(callback) = &mut *callback.lock().unwrap() {
                            callback(message);
                        }
                    }
                    Err(e) => log::error!("deserializing server message failed: {}", e),
                }
            }
        });

        Ok(SignalingConnection { stream, callback })
    }

    /// Sends a `ClientMessage` to the signaling server.
    pub fn send(&self, message: &ClientMessage) -> Result<(), serde_json::Error> {
        let stream = self.stream.try_clone().expect("could not clone stream");
        serde_json::to_writer(stream, message)?;
        Ok(())
    }

    /// Sets the callback which will be called with messages from the signaling server.
    ///
    /// This removed any previously set callback. The callback is called on the thread that handles
    /// incoming messages from the signaling server and should therefore not block.
    pub fn set_callback(&self, callback: impl FnMut(ServerMessage) + Send + 'static) {
        *self.callback.lock().unwrap() = Some(Box::new(callback));
    }
}
