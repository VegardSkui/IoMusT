use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::collections::{HashMap, VecDeque};
use std::convert::TryInto;
use std::net::{SocketAddr, UdpSocket};
use std::sync::{Arc, RwLock};

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

    let address = std::env::args().nth(1).expect("missing address argument");

    let peer_addresses: Vec<String> = std::env::args().skip(2).collect();

    // Bind a UDP socket
    let socket = UdpSocket::bind(address).expect("could not bind to address");
    log::info!("bound to {}", socket.local_addr().unwrap());
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

    // Get configurations for input and output
    let supported_input_stream_config = get_supported_input_stream_config(&input_device);
    let input_stream_config = get_stream_config(&supported_input_stream_config);
    log::info!(
        "using supported input stream config: {:?}",
        supported_input_stream_config
    );
    let supported_output_stream_config = get_supported_output_stream_config(&output_device);
    let output_stream_config = get_stream_config(&supported_output_stream_config);
    log::info!(
        "using output stream config: {:?}",
        supported_output_stream_config
    );

    // Error if not using f32 samples
    if supported_input_stream_config.sample_format() != cpal::SampleFormat::F32 {
        panic!("unsupported sample format on the input stream");
    }
    if supported_output_stream_config.sample_format() != cpal::SampleFormat::F32 {
        panic!("unsupported sample format on the output stream");
    }
    // TODO: Support all sample formats

    let mut serial: u16 = 1;
    let send_instants = Arc::new(RwLock::new(HashMap::<u16, std::time::Instant>::new()));

    // Build the input stream
    let input_stream = input_device
        .build_input_stream(
            &input_stream_config,
            {
                let peers = peers.clone();
                let send_instants = send_instants.clone();
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    // Store the instant the input is received (before processing and sending) for
                    // every 2^10 = 1024 input
                    if serial.trailing_zeros() >= 10 {
                        send_instants
                            .write()
                            .unwrap()
                            .insert(serial, std::time::Instant::now());
                    }

                    // Map the audio data to a little-endian byte vector
                    let sample_bytes: Vec<u8> = data
                        .iter()
                        .flat_map(|sample| sample.to_le_bytes())
                        .collect();

                    // NOTE: As long as we agree on the native endianess we could skip the above
                    // conversion and just transmute the type of "data".
                    // But this should just be a no-op if it matches anyways? Can we verify this?
                    // (maybe with Compiler Explorer, godbolt.org)

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

                    // NOTE: This overflows quite quickly due to only being a u16, consider
                    // changing
                    serial += 1;
                }
            },
            |err| {
                panic!("input err: {:?}", err);
            },
        )
        .expect("could not build input stream");

    // Play the input stream
    input_stream.play().expect("could not play input stream");

    // Create a new peer outputs manager
    let mut output_manager = PeerOutputsManager {
        output_device: &output_device,
        output_stream_config: &output_stream_config,
        streams: HashMap::new(),
        buffers: HashMap::new(),
    };

    // Add outputs for the supplied peer addresses
    for peer_addr in peer_addresses {
        let peer = Peer::new(peer_addr.parse().unwrap());
        output_manager.add_peer(&peer);
        peers.write().unwrap().insert(peer.addr, peer);
    }

    // Read received packets
    loop {
        // buffer size of 64 * 2 channels * 4 bytes per sample + 2 bytes for series + 2 bytes for
        // last received series
        let mut buf = [0; 64 * 2 * 4 + 2 + 2];
        let (_amt, src) = recv.recv_from(&mut buf).expect("could not receive");

        // Skip packets not from one of our peers
        if let Some(mut peer) = peers.write().unwrap().get_mut(&src) {
            let (serial_bytes, buf) = buf.split_at(2);
            let (last_received_seriel_bytes, sample_bytes) = buf.split_at(2);

            peer.last_received_serial = u16::from_le_bytes(serial_bytes.try_into().unwrap());

            // Print the upper bound on delay using the time difference from when we sent a sample
            // to when we received a sample where the last received sample of our peer is that
            // sample. This will print a huge overestimate if one of the packets we measure are
            // lost or the next is received by our peer before they get to send out a packet with
            // last_received_series set to our measurement series.
            let last_received_serial =
                u16::from_le_bytes(last_received_seriel_bytes.try_into().unwrap());
            if last_received_serial.trailing_zeros() >= 10 && last_received_serial != 0 {
                // TODO: Remove the second unwrap here as it could potentially lead to a crash
                let elapsed = send_instants
                    .read()
                    .unwrap()
                    .get(&last_received_serial)
                    .unwrap()
                    .elapsed();
                println!("upper bound on delay from {} is {:#?}", src, elapsed);
            }

            // Decode the received samples back to f32
            let samples: [f32; 128] = sample_bytes
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
                .collect::<Vec<f32>>()[..128]
                .try_into()
                .unwrap();
            output_manager.add_samples(&src, &samples);
        }
    }
}

// TODO: Could this be generic over the output stream config? At least the sample type?
struct PeerOutputsManager<'a> {
    output_device: &'a cpal::Device,
    output_stream_config: &'a cpal::StreamConfig,
    streams: HashMap<SocketAddr, cpal::Stream>,
    buffers: HashMap<SocketAddr, Arc<RwLock<VecDeque<[f32; 128]>>>>,
}

impl<'a> PeerOutputsManager<'a> {
    fn add_peer(&mut self, peer: &Peer) {
        log::info!("building output stream for {}", peer.addr);

        // Create an empty buffer for the stream
        let buffer = Arc::new(RwLock::new(VecDeque::new()));
        self.buffers.insert(peer.addr, buffer.clone());

        // Build a new output stream
        let output_stream = self
            .output_device
            .build_output_stream(
                self.output_stream_config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    if let Some(mut samples) = buffer.write().unwrap().pop_front() {
                        // NOTE: This will panic if data is somehow longer than samples
                        data.swap_with_slice(&mut samples[..data.len()]);
                    } else {
                        // Play silence
                        for sample in data.iter_mut() {
                            *sample = cpal::Sample::from(&0.0);
                        }
                    }
                },
                |err| {
                    panic!("output err: {:?}", err);
                },
            )
            .expect("could not build output stream");

        // Play the stream
        output_stream.play().expect("could not play output stream");

        // Store the stream so it doesn't drop immediately
        self.streams.insert(peer.addr, output_stream);
    }

    fn add_samples(&mut self, source: &SocketAddr, samples: &[f32; 128]) {
        // TODO: Return an error instead of panic on the first unwrap.
        // Even though it should never happen if we remember to call add_peer (and later
        // remove_peer?) as appropriate.
        self.buffers
            .get_mut(source)
            .unwrap()
            .write()
            .unwrap()
            .push_back(*samples);
    }
}

fn get_supported_input_stream_config(device: &cpal::Device) -> cpal::SupportedStreamConfig {
    device
        .default_input_config()
        .expect("no default input config")
}

fn get_supported_output_stream_config(device: &cpal::Device) -> cpal::SupportedStreamConfig {
    device
        .default_output_config()
        .expect("no default output config")
}

fn get_stream_config(supported_config: &cpal::SupportedStreamConfig) -> cpal::StreamConfig {
    let mut config = supported_config.config();
    config.buffer_size = cpal::BufferSize::Fixed(64);
    config
}