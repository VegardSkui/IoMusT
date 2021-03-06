use std::collections::HashMap;
use std::convert::{TryFrom, TryInto};
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use serde::Serialize;

#[derive(Clone, Copy)]
#[repr(u8)]
enum PeerMessageKind {
    Audio = 0,
    Ping = 1,
    Pong = 2,
}

impl TryFrom<u8> for PeerMessageKind {
    type Error = &'static str;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(PeerMessageKind::Audio),
            1 => Ok(PeerMessageKind::Ping),
            2 => Ok(PeerMessageKind::Pong),
            _ => Err("unknown peer message kind"),
        }
    }
}

/// A round-trip time measurement record.
#[derive(Debug, Serialize)]
struct RttRecord {
    /// Unix timestamp for when the ping message was sent.
    ping_time: u64,
    /// Address of the peer.
    peer: SocketAddr,
    /// The measured round-trip time in seconds.
    rtt: f32,
}

/// A peer communicator.
///
/// The peer communicator is responsible for sending/receiving audio packets to/from connected
/// peers. Received audio is pushed onto a peer's associated audio output buffer for playback.
pub struct PeerCommunicator {
    /// The number of input audio channels.
    channels: cpal::ChannelCount,
    /// All currently connected peers. The key is the audio address of the peer, and the value is
    /// the producer half of its associated audio output buffer.
    peers: Arc<Mutex<HashMap<SocketAddr, ringbuf::Producer<u16>>>>,
    /// The UDP socket used for peer communication.
    pub(crate) socket: UdpSocket,
}

impl PeerCommunicator {
    /// Initializes a new peer communicator to listen for audio data received from peers.
    ///
    /// This will also automatically start a new thread to listen for incoming data from peers.
    pub fn initialize<A: ToSocketAddrs>(
        addr: A,
        channels: cpal::ChannelCount,
    ) -> std::io::Result<PeerCommunicator> {
        // Bind a UDP socket
        let socket = UdpSocket::bind(addr)?;
        log::info!("bound to `{}`", socket.local_addr().unwrap());

        let peers = Arc::new(Mutex::new(
            HashMap::<SocketAddr, ringbuf::Producer<u16>>::new(),
        ));

        // Spawn a new thread to read incoming packets from out peers
        std::thread::spawn({
            // Create a CSV writer for round-trip time measurements
            let rtt_path = format!("rtt-{}.csv", epoch_time().as_secs());
            log::info!("writing rtt measurements to `{}`", rtt_path);
            let mut rtt_writer = csv::Writer::from_path(rtt_path)
                .expect("could not create csv writer for rtt measurements");

            let socket = socket.try_clone().expect("could not clone socket");
            let peers = peers.clone();
            move || loop {
                // buffer size of 256 * 2 channels * 2 bytes per sample + 1 byte for message kind
                // TODO: This should probably be somewhat dynamic to support unknown buffer sized
                // for our peers. Alternatively we can `expect` the input buffer size to be less
                // than a given value when configuring the input, and then use that maximum size of
                // buffer here. It should not be unbounded dynamic as that opens us up to memory
                // exhaustion attacks. If we see an input configuration larger than the maximum
                // supported audio packet size we could split it across multiple packets.
                let mut buf = [0; 256 * 2 * 2 + 1];
                let (amt, src) = match socket.recv_from(&mut buf) {
                    Ok((amt, src)) => (amt, src),
                    Err(e) => {
                        log::error!("receive failed: {}", e);
                        continue;
                    }
                };

                // Skip packets not from one of our peers
                if let Some(producer) = peers.lock().unwrap().get_mut(&src) {
                    let (kind, buf) = buf.split_first().expect("received empty buffer");
                    match (*kind).try_into() {
                        Ok(PeerMessageKind::Audio) => {
                            // Decode the received samples back to u16 and add them to the peer's
                            // producer for playback
                            producer.push_iter(
                                // The number of sample bytes is the total received buffer length
                                // minus the byte used for the message kind
                                &mut buf[..amt - 1]
                                    .chunks_exact(2)
                                    .map(|c| u16::from_le_bytes(c.try_into().unwrap())),
                            );
                        }
                        Ok(PeerMessageKind::Ping) => {
                            // Return whatever was received in the ping message to the sender
                            if let Err(err) =
                                send_message(&socket, &src, PeerMessageKind::Pong, &buf[..amt - 1])
                            {
                                log::error!("sending pong failed: {}", err);
                            }
                        }
                        Ok(PeerMessageKind::Pong) => {
                            // Read the ping time contained in the returned pong message
                            let ping_time = std::time::Duration::from_micros(u64::from_le_bytes(
                                buf[..8].try_into().unwrap(),
                            ));

                            // Calculate the total round-trip time
                            let rtt = epoch_time() - ping_time;

                            log::trace!("round-trip time to `{}` is {:#?}", src, rtt);

                            // Write the measurement to the CSV file
                            rtt_writer
                                .serialize(RttRecord {
                                    ping_time: ping_time.as_secs(),
                                    peer: src,
                                    rtt: rtt.as_secs_f32(),
                                })
                                .expect("failed to serialize rtt measurement");

                            // Flush the writer immediately so we don't miss any measurements when
                            // we are shut down
                            rtt_writer.flush().expect("failed to flush rtt csv writer");
                        }
                        Err(err) => log::warn!("could not parse received peer message: `{}`", err),
                    }
                }
            }
        });

        // Start another thread to ping each of our peers in regular intervals
        std::thread::spawn({
            let socket = socket.try_clone().expect("could not clone socket");
            let peers = peers.clone();
            move || loop {
                for peer in peers.lock().unwrap().keys() {
                    if let Err(err) = send_message(
                        &socket,
                        peer,
                        PeerMessageKind::Ping,
                        &epoch_time().as_micros().to_le_bytes(),
                    ) {
                        log::error!("sending ping failed: {}", err);
                    }
                }

                // Sleep the thread until the next round of pings
                std::thread::sleep(Duration::from_secs(1));
            }
        });

        Ok(PeerCommunicator {
            channels,
            peers,
            socket,
        })
    }

    /// Sends audio samples to all connected peers.
    pub fn send_samples<T: cpal::Sample>(&mut self, samples: &[T]) -> std::io::Result<()> {
        // Downmix the audio to mono by taking the value of the first channel and convert to a
        // little-endian byte vector. The endianness conversion is a no-op on little-endian
        // systems.
        let sample_bytes: Vec<u8> = samples
            .chunks(self.channels.into())
            .flat_map(|samples| samples[0].to_u16().to_le_bytes())
            .collect();

        // Send the data to each peer
        for addr in self.peers.lock().unwrap().keys() {
            send_message(&self.socket, addr, PeerMessageKind::Audio, &sample_bytes)?;
        }

        Ok(())
    }

    /// Adds a new peer by address.
    ///
    /// This will start accepting datagrams from the given address and push received audio data to
    /// its associated buffer.
    pub fn add(&mut self, addr: SocketAddr, producer: ringbuf::Producer<u16>) {
        self.peers.lock().unwrap().insert(addr, producer);
    }

    /// Removes a peer by address.
    ///
    /// Incoming data from the associated address will no longer be accepted.
    pub fn remove(&mut self, addr: &SocketAddr) {
        self.peers.lock().unwrap().remove(addr);
    }
}

/// Returns the current Unix time from the current system time.
fn epoch_time() -> Duration {
    // This unwrap should never fail because the current system time should always be significantly
    // larger than the Unix epoch
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
}

/// Sends a message prefixed with a [`PeerMessageKind`] over UDP.
fn send_message(
    socket: &UdpSocket,
    addr: &SocketAddr,
    kind: PeerMessageKind,
    content: &[u8],
) -> Result<(), std::io::Error> {
    socket.send_to(&[&[kind as u8], content].concat(), addr)?;
    Ok(())
}
