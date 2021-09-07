use std::collections::HashMap;
use std::convert::TryInto;
use std::net::{SocketAddr, UdpSocket};
use std::sync::{Arc, RwLock};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use iomust_signaling_messages::{ClientMessage, ServerMessage};

use crate::output::OutputManager;
use crate::signaling::SignalingConnection;

mod output;
mod signaling;

struct Peer {
    addr: SocketAddr,
    last_received_serial: u16,
}

impl Peer {
    pub fn new(addr: SocketAddr) -> Peer {
        Peer {
            addr,
            last_received_serial: 0,
        }
    }
}

fn main() {
    // Initialize logging
    env_logger::init();
    log::info!("launching iomust_peer");

    let signaling_server_addr = std::env::args()
        .nth(1)
        .expect("missing signaling server address argument");

    // Bind a UDP socket
    let socket = UdpSocket::bind("0:0").expect("could not bind to address");
    log::info!("bound to `{}`", socket.local_addr().unwrap());
    let recv = Arc::new(socket);
    let send = recv.clone();

    // Create a HashMap to store our peers
    let peers = Arc::new(RwLock::new(HashMap::<SocketAddr, Peer>::new()));

    // Get the default audio host on non-Windows platforms. On Windows, specifically request the
    // ASIO host in order to let us specify the buffer size.
    #[cfg(not(target_os = "windows"))]
    let host = cpal::default_host();
    #[cfg(target_os = "windows")]
    let host = cpal::host_from_id(cpal::HostId::Asio).expect("failed to initialize ASIO host");
    log::info!("using audio host: {:?}", host.id());

    // Get the default input and output audio devices
    let input_device = host
        .default_input_device()
        .expect("no input device available");
    log::info!(
        "using input device: {}",
        input_device
            .name()
            .expect("could not get input device name")
    );
    let output_device = host
        .default_output_device()
        .expect("no output device available");
    log::info!(
        "using output device: {}",
        output_device
            .name()
            .expect("could not get output device name")
    );

    // Get input stream configuration
    let supported_input_stream_config = get_supported_input_stream_config(&input_device);
    let input_stream_config = get_stream_config(&supported_input_stream_config);
    log::debug!(
        "using supported input stream config: {:?}",
        supported_input_stream_config
    );
    log::debug!("using input stream config: {:?}", input_stream_config);

    let send_instants = Arc::new(RwLock::new(HashMap::<u16, std::time::Instant>::new()));

    // Use a function to build the data function for the input stream in order to make it generic
    // over the sample format
    fn build_data_fn<T: cpal::Sample>(
        send: Arc<UdpSocket>,
        peers: Arc<RwLock<HashMap<SocketAddr, Peer>>>,
        send_instants: Arc<RwLock<HashMap<u16, std::time::Instant>>>,
    ) -> impl FnMut(&[T], &cpal::InputCallbackInfo) {
        let mut serial: u16 = 1;
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            // Store the instant the input is received (before processing and sending) for every
            // 2^10 = 1024 input
            if serial.trailing_zeros() >= 10 {
                send_instants
                    .write()
                    .unwrap()
                    .insert(serial, std::time::Instant::now());
            }
            // Map the audio data to a little-endian byte vector
            let sample_bytes: Vec<u8> = data
                .iter()
                .flat_map(|sample| sample.to_u16().to_le_bytes())
                .collect();

            // NOTE: As long as we agree on the native endianess we could skip the above conversion
            // and just transmute the type of "data".
            // But this should just be a no-op if it matches anyways? Can we verify this?  (maybe
            // with Compiler Explorer, godbolt.org)

            // Send the data to each peer
            for peer in peers.read().unwrap().values() {
                let payload = [
                    &serial.to_le_bytes(),
                    &peer.last_received_serial.to_le_bytes(),
                    sample_bytes.as_slice(),
                ]
                .concat();
                send.send_to(&payload, &peer.addr)
                    .expect("could not send data");
            }

            // Increment the serial number, wrapping around on overflow
            serial = serial.wrapping_add(1);
        }
    }

    let err_fn = |err| panic!("input err: {:?}", err);

    // Build the input stream
    let input_stream = match supported_input_stream_config.sample_format() {
        cpal::SampleFormat::I16 => input_device.build_input_stream(
            &input_stream_config,
            build_data_fn::<i16>(send, peers.clone(), send_instants.clone()),
            err_fn,
        ),
        cpal::SampleFormat::U16 => input_device.build_input_stream(
            &input_stream_config,
            build_data_fn::<u16>(send, peers.clone(), send_instants.clone()),
            err_fn,
        ),
        cpal::SampleFormat::F32 => input_device.build_input_stream(
            &input_stream_config,
            build_data_fn::<f32>(send, peers.clone(), send_instants.clone()),
            err_fn,
        ),
    }
    .expect("could not build input stream");

    // Play the input stream
    input_stream.play().expect("could not play input stream");

    // Store the producer for each peer in a hashmap
    let producers = Arc::new(RwLock::new(
        HashMap::<SocketAddr, ringbuf::Producer<u16>>::new(),
    ));

    let mut output_manager = OutputManager::<SocketAddr>::new(output_device);

    // Connect to the signaling server
    let signaling_conn = SignalingConnection::connect(signaling_server_addr)
        .expect("could not connect to the signaling server");

    // Handle incoming messages from the signaling server
    signaling_conn.set_callback({
        let peers = peers.clone();
        let producers = producers.clone();
        move |message: ServerMessage| match message {
            ServerMessage::Connected { addr } => {
                log::info!("peer `{}` connected", addr);
                let peer = Peer::new(addr);
                let producer = output_manager.add(addr);
                peers.write().unwrap().insert(addr, peer);
                producers.write().unwrap().insert(addr, producer);
            }
            ServerMessage::Disconnected { addr } => {
                log::info!("peer `{}` disconnected", addr);
                output_manager.remove(&addr);
                peers.write().unwrap().remove(&addr);
                producers.write().unwrap().remove(&addr);
            }
        }
    });

    // Send a hey message to the signaling server to connect us with other peers
    signaling_conn
        .send(&ClientMessage::Hey {
            name: String::from("Unnamed Peer"),
            port: recv.local_addr().unwrap().port(),
        })
        .expect("sending hey message to the signaling server failed");

    // Read received packets from peers
    loop {
        // buffer size of 64 * 2 channels * 2 bytes per sample + 2 bytes for series + 2 bytes for
        // last received series
        let mut buf = [0; 64 * 2 * 2 + 2 + 2];
        let (amt, src) = recv.recv_from(&mut buf).expect("could not receive");

        // Skip packets not from one of our peers
        if let Some(mut peer) = peers.write().unwrap().get_mut(&src) {
            let (serial_bytes, buf) = buf.split_at(2);
            let (last_received_serial_bytes, sample_bytes) = buf.split_at(2);

            peer.last_received_serial = u16::from_le_bytes(serial_bytes.try_into().unwrap());

            // Print the upper bound on delay using the time difference from when we sent a sample
            // to when we received a sample where the last received sample of our peer is that
            // sample. This will print a huge overestimate if one of the packets we measure are
            // lost or the next is received by our peer before they get to send out a packet with
            // last_received_series set to our measurement series.
            let last_received_serial =
                u16::from_le_bytes(last_received_serial_bytes.try_into().unwrap());
            if last_received_serial.trailing_zeros() >= 10 && last_received_serial != 0 {
                // TODO: Remove the second unwrap here as it could potentially lead to a crash
                let elapsed = send_instants
                    .read()
                    .unwrap()
                    .get(&last_received_serial)
                    .unwrap()
                    .elapsed();
                log::trace!("upper bound on delay from `{}` is {:#?}", src, elapsed);
            }

            // Decode the received samples back to u16 and add them to the peer's producer for
            // playback
            if let Some(producer) = producers.write().unwrap().get_mut(&peer.addr) {
                producer.push_iter(
                    // The number of sample bytes is the total received buffer length minus the 4
                    // bytes used for serial and last_received_serial
                    &mut sample_bytes[..amt - 4]
                        .chunks_exact(2)
                        .map(|c| u16::from_le_bytes(c.try_into().unwrap())),
                );
            }
        }
    }
}

fn get_supported_input_stream_config(device: &cpal::Device) -> cpal::SupportedStreamConfig {
    device
        .default_input_config()
        .expect("no default input config")
}

fn get_stream_config(supported_config: &cpal::SupportedStreamConfig) -> cpal::StreamConfig {
    let mut config = supported_config.config();
    config.buffer_size = cpal::BufferSize::Fixed(64);
    config
}
