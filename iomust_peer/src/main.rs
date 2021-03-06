use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::{Arc, Mutex, RwLock};

use clap::{crate_name, crate_version, App, Arg};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::RingBuffer;

use crate::output::OutputManager;
use crate::peer::PeerCommunicator;
use crate::server::ServerConnectionBuilder;

mod output;
mod peer;
mod server;

fn main() {
    // Initialize logging
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();
    log::info!("launching iomust_peer");

    let matches = App::new(crate_name!())
        .version(crate_version!())
        .after_help("-p and -s are mutually exclusive, use only one at a time.\n\n\
                     It is useful to set the peer communicaiton address manually with -a when\n\
                     connecting to peers using -p such that you know (and can share) your own address\n\
                     before launching the program.")
        .arg(
            Arg::with_name("addr")
                .short("a")
                .default_value("0.0.0.0:0")
                .help("Peer communication address"),
        )
        .arg(
            Arg::with_name("peer")
                .short("p")
                .multiple(true)
                .takes_value(true)
                .required(true)
                .conflicts_with("server")
                .help("Peers"),
        )
        .arg(
            Arg::with_name("server")
                .short("s")
                .takes_value(true)
                .required(true)
                .conflicts_with("peer")
                .help("Server address"),
        )
        .get_matches();

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
        // The `addr` option should always have a value since we've set a default, thus the
        // associated call to expect should never materialize
        PeerCommunicator::initialize(
            matches.value_of("addr").expect("missing address"),
            input_stream_config.channels,
        )
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

    let output_manager = Arc::new(Mutex::new(OutputManager::<SocketAddr>::new(output_device)));

    // Connect each of the provided peers
    if let Some(peers) = matches.values_of("peer") {
        for peer in peers {
            let (addr, sample_rate) = peer.split_once(',').expect("invalid peer format");
            let addr = SocketAddr::from_str(addr).expect("failed to parse peer address");
            let sample_rate = sample_rate
                .parse::<u32>()
                .expect("failed to parse peer sample rate");
            connect_peer(&peer_comm, &output_manager, addr, sample_rate);
        }
    }

    // Connect to and enter the server if an address was provided
    if let Some(addr) = matches.value_of("server") {
        ServerConnectionBuilder::new(addr)
            .entered_callback({
                // Connect to peers who enter
                let peer_comm = peer_comm.clone();
                let output_manager = Arc::clone(&output_manager);
                move |addr, sample_rate| {
                    connect_peer(&peer_comm, &output_manager, addr, sample_rate);
                }
            })
            .left_callback({
                // Disconnect from peer who leave
                let peer_comm = Arc::clone(&peer_comm);
                let output_manager = Arc::clone(&output_manager);
                move |addr| {
                    disconnect_peer(&peer_comm, &output_manager, addr);
                }
            })
            .connect_and_enter(
                peer_comm.read().unwrap().socket.try_clone().unwrap(),
                input_stream_config.sample_rate.0,
            )
            .expect("failed to connect and enter the iomust server");
    }

    // This main thread has nothing more to do, so park it indefinitely
    loop {
        std::thread::park()
    }
}

fn connect_peer(
    peer_comm: &Arc<RwLock<PeerCommunicator>>,
    output_manager: &Arc<Mutex<OutputManager<SocketAddr>>>,
    addr: SocketAddr,
    sample_rate: u32,
) {
    log::info!("connecting peer `{}`", addr);
    // Create a new output buffer for the peer
    // TODO: Come up with a buffer capacity without mostly guessing
    let (producer, consumer) = RingBuffer::new(2048).split();
    peer_comm.write().unwrap().add(addr, producer);
    output_manager
        .lock()
        .unwrap()
        .add(addr, cpal::SampleRate(sample_rate), consumer);
}

fn disconnect_peer(
    peer_comm: &Arc<RwLock<PeerCommunicator>>,
    output_manager: &Arc<Mutex<OutputManager<SocketAddr>>>,
    addr: SocketAddr,
) {
    log::info!("disconnecting peer `{}`", addr);
    peer_comm.write().unwrap().remove(&addr);
    output_manager.lock().unwrap().remove(addr);
}
