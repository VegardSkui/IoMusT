use std::net::SocketAddr;
use std::sync::{Arc, RwLock};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use iomust_signaling_messages::{ClientMessage, ServerMessage};
use ringbuf::RingBuffer;

use crate::output::OutputManager;
use crate::peer::PeerCommunicator;
use crate::signaling::SignalingConnection;

mod output;
mod peer;
mod signaling;

fn main() {
    // Initialize logging
    env_logger::init();
    log::info!("launching iomust_peer");

    let signaling_server_addr = std::env::args()
        .nth(1)
        .expect("missing signaling server address argument");

    // Get the default audio host
    let host = cpal::default_host();
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

    // Get input stream configuration, preferring a sample rate of 48 kHz, but falling back to the
    // device's default
    let preferred_sample_rate = cpal::SampleRate(48000);
    let supported_input_stream_config = input_device
        .supported_input_configs()
        .unwrap()
        .find(|config| {
            config.min_sample_rate() <= preferred_sample_rate
                && config.max_sample_rate() >= preferred_sample_rate
        })
        .map(|config| config.with_sample_rate(preferred_sample_rate))
        .unwrap_or_else(|| {
            input_device
                .default_input_config()
                .expect("could not get default input config")
        });
    // Prefer a buffer size of 64, but clamp to be within the supported range. Use the default
    // buffer size if the supported range is unknown.
    let mut input_stream_config = supported_input_stream_config.config();
    input_stream_config.buffer_size = match supported_input_stream_config.buffer_size() {
        cpal::SupportedBufferSize::Range { min, max } => {
            cpal::BufferSize::Fixed(64.clamp(*min, *max))
        }
        cpal::SupportedBufferSize::Unknown => cpal::BufferSize::Default,
    };
    log::debug!(
        "using supported input stream config: {:?}",
        supported_input_stream_config
    );
    log::debug!("using input stream config: {:?}", input_stream_config);

    // Initialize a peer communicator
    let peer_comm = Arc::new(RwLock::new(
        PeerCommunicator::initialize(input_stream_config.channels)
            .expect("initializing peer communicator failed"),
    ));

    // Use a function to build the data function for the input stream in order to make it generic
    // over the sample format
    fn build_data_fn<T: cpal::Sample>(
        peer_comm: Arc<RwLock<PeerCommunicator>>,
    ) -> impl FnMut(&[T], &cpal::InputCallbackInfo) {
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            if let Err(err) = peer_comm.write().unwrap().send_samples(data) {
                log::error!("sending data failed: {}", err);
            }
        }
    }

    let err_fn = |err| panic!("input err: {:?}", err);

    // Build the input stream
    let input_stream = match supported_input_stream_config.sample_format() {
        cpal::SampleFormat::I16 => input_device.build_input_stream(
            &input_stream_config,
            build_data_fn::<i16>(peer_comm.clone()),
            err_fn,
        ),
        cpal::SampleFormat::U16 => input_device.build_input_stream(
            &input_stream_config,
            build_data_fn::<u16>(peer_comm.clone()),
            err_fn,
        ),
        cpal::SampleFormat::F32 => input_device.build_input_stream(
            &input_stream_config,
            build_data_fn::<f32>(peer_comm.clone()),
            err_fn,
        ),
    }
    .expect("could not build input stream");

    // Play the input stream
    input_stream.play().expect("could not play input stream");

    // Connect to the signaling server
    let signaling_conn = SignalingConnection::connect(signaling_server_addr)
        .expect("could not connect to the signaling server");

    // Handle incoming messages from the signaling server
    signaling_conn.set_callback({
        let peer_comm = peer_comm.clone();
        let mut output_manager = OutputManager::<SocketAddr>::new(output_device);
        move |message: ServerMessage| match message {
            ServerMessage::Connected { addr } => {
                log::info!("peer `{}` connected", addr);
                // Create a new output buffer for the peer
                // TODO: Come up with a buffer capacity without mostly guessing
                let (producer, consumer) = RingBuffer::new(2048).split();
                peer_comm.write().unwrap().add(addr, producer);
                output_manager.add(addr, consumer);
            }
            ServerMessage::Disconnected { addr } => {
                log::info!("peer `{}` disconnected", addr);
                peer_comm.write().unwrap().remove(&addr);
                output_manager.remove(&addr);
            }
        }
    });

    // Send a hey message to the signaling server to connect us with other peers
    signaling_conn
        .send(&ClientMessage::Hey {
            name: String::from("Unnamed Peer"),
            port: peer_comm.read().unwrap().local_addr().unwrap().port(),
        })
        .expect("sending hey message to the signaling server failed");

    // This main thread has nothing more to do, so park it indefinitely
    loop {
        std::thread::park()
    }
}
