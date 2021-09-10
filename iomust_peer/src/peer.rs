use std::collections::HashMap;
use std::convert::TryInto;
use std::net::{SocketAddr, UdpSocket};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// A peer communicator.
///
/// The peer communicator is responsible for sending/receiving audio packets to/from connected
/// peers. Received audio is pushed onto a peer's associated audio output buffer for playback.
pub struct PeerCommunicator {
    /// All currently connected peers. The key is the audio address of the peer, and the value is a
    /// tuple of the producer half of its associated audio output buffer and the serial number of
    /// our last received audio packet from them.
    peers: Arc<Mutex<HashMap<SocketAddr, (ringbuf::Producer<u16>, u16)>>>,
    send_instants: Arc<Mutex<HashMap<u16, Instant>>>,
    serial: u16,
    /// The UDP socket used for peer communication.
    socket: UdpSocket,
}

impl PeerCommunicator {
    /// Initializes a new peer communicator to listen for audio data received from peers.
    ///
    /// This will also automatically start a new thread to listen for incoming data from peers.
    pub fn initialize() -> Result<Self, std::io::Error> {
        // Bind a UDP socket
        let socket = UdpSocket::bind("0.0.0.0:0")?;
        log::info!("bound to `{}`", socket.local_addr().unwrap());

        let peers = Arc::new(Mutex::new(HashMap::<
            SocketAddr,
            (ringbuf::Producer<u16>, u16),
        >::new()));

        let send_instants = Arc::new(Mutex::new(HashMap::<u16, Instant>::new()));

        // Spawn a new thread to read incoming packets from out peers
        std::thread::spawn({
            let send_instants = send_instants.clone();
            let socket = socket.try_clone().expect("could not clone socket");
            let peers = peers.clone();
            move || loop {
                // buffer size of 256 * 2 channels * 2 bytes per sample + 2 bytes for series + 2
                // bytes for last received series
                // TODO: This should probably be somewhat dynamic to support unknown buffer sized
                // for our peers. Alternatively we can `expect` the input buffer size to be less
                // than a given value when configuring the input, and then use that maximum size of
                // buffer here. It should not be unbounded dynamic as that opens us up to memory
                // exhaustion attacks. If we see an input configuration larger than the maximum
                // supported audio packet size we could split it across multiple packets.
                let mut buf = [0; 256 * 2 * 2 + 2 + 2];
                let (amt, src) = match socket.recv_from(&mut buf) {
                    Ok((amt, src)) => (amt, src),
                    Err(e) => {
                        log::error!("receive failed: {}", e);
                        continue;
                    }
                };

                // Skip packets not from one of our peers
                if let Some((producer, last_received_serial)) = peers.lock().unwrap().get_mut(&src)
                {
                    let (serial_bytes, buf) = buf.split_at(2);
                    let (last_received_serial_bytes, sample_bytes) = buf.split_at(2);

                    *last_received_serial = u16::from_le_bytes(serial_bytes.try_into().unwrap());

                    // Print the upper bound on delay using the time difference from when we sent a
                    // sample to when we received a sample where the last received sample of our
                    // peer is that sample. This will print a huge overestimate if one of the
                    // packets we measure are lost or the next is received by our peer before they
                    // get to send out a packet with last_received_series set to our measurement
                    // series.
                    let last_received_serial =
                        u16::from_le_bytes(last_received_serial_bytes.try_into().unwrap());
                    if last_received_serial.trailing_zeros() >= 10 && last_received_serial != 0 {
                        // TODO: Remove the second unwrap here as it could potentially lead to a
                        // crash
                        let elapsed = send_instants
                            .lock()
                            .unwrap()
                            .get(&last_received_serial)
                            .unwrap()
                            .elapsed();
                        log::trace!("upper bound on delay from `{}` is {:#?}", src, elapsed);
                    }

                    // Decode the received samples back to u16 and add them to the peer's producer
                    // for playback
                    producer.push_iter(
                        // The number of sample bytes is the total received buffer length minus the
                        // 4 bytes used for serial and last_received_serial
                        &mut sample_bytes[..amt - 4]
                            .chunks_exact(2)
                            .map(|c| u16::from_le_bytes(c.try_into().unwrap())),
                    );
                }
            }
        });

        Ok(PeerCommunicator {
            peers,
            send_instants,
            serial: 0,
            socket,
        })
    }

    /// Returns the local address of the socket used for peer communication.
    pub fn local_addr(&self) -> Result<SocketAddr, std::io::Error> {
        self.socket.local_addr()
    }

    /// Sends audio samples to all connected peers.
    pub fn send_samples<T: cpal::Sample>(&mut self, samples: &[T]) -> Result<(), std::io::Error> {
        // Store the instant the input is received (before processing and sending) for every 2^10 =
        // 1024 input
        if self.serial.trailing_zeros() >= 10 {
            self.send_instants
                .lock()
                .unwrap()
                .insert(self.serial, Instant::now());
        }

        // Map the audio data to a little-endian byte vector
        let sample_bytes: Vec<u8> = samples
            .iter()
            .flat_map(|sample| sample.to_u16().to_le_bytes())
            .collect();

        // NOTE: As long as we agree on the native endianess we could skip the above conversion and
        // just transmute the type of "data".
        // But this should just be a no-op if it matches anyways? Can we verify this? (maybe with
        // Compiler Explorer, godbolt.org)

        // Send the data to each peer
        for (addr, (_, last_received_serial)) in self.peers.lock().unwrap().iter() {
            let payload = [
                &self.serial.to_le_bytes(),
                &last_received_serial.to_le_bytes(),
                sample_bytes.as_slice(),
            ]
            .concat();
            self.socket.send_to(&payload, &addr)?;
        }

        // Increment the serial number, wrapping around on overflow
        self.serial = self.serial.wrapping_add(1);

        Ok(())
    }

    /// Adds a new peer by address.
    ///
    /// This will start accepting datagrams from the given address and push received audio data to
    /// its associated buffer.
    pub fn add(&mut self, addr: SocketAddr, producer: ringbuf::Producer<u16>) {
        self.peers.lock().unwrap().insert(addr, (producer, 0));
    }

    /// Removes a peer by address.
    ///
    /// Incoming data from the associated address will no longer be accepted.
    pub fn remove(&mut self, addr: &SocketAddr) {
        self.peers.lock().unwrap().remove(addr);
    }
}
