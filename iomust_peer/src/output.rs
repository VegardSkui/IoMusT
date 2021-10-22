use std::collections::HashMap;
use std::hash::Hash;
use std::sync::mpsc;

use cpal::traits::{DeviceTrait, StreamTrait};
use dasp::{Frame, Signal};

enum ControlMessage<T: Eq + Hash + Send> {
    AddStream {
        ident: T,
        sample_rate: cpal::SampleRate,
        consumer: ringbuf::Consumer<u16>,
    },
    RemoveStream {
        ident: T,
    },
}

/// Manages output streams.
///
/// The output manager spawns its own audio output management thread. Actual creation and removal
/// of audio streams happens on this thread due to audio stream not being thead safe. As a result,
/// functions for adding and removing streams do **not** block until complete.
///
/// Using multiple output streams assumes that the underlying sound driver supports mixing.
pub struct OutputManager<T: Eq + Hash + Send> {
    /// Sender half of the output management thread control message stream.
    tx: mpsc::Sender<ControlMessage<T>>,
}

impl<T: 'static + Eq + Hash + Send> OutputManager<T> {
    /// Creates a new output manager for the given output device.
    pub fn new(device: cpal::Device) -> Self {
        // Create a channel to send control messages to the output management thread
        let (tx, rx) = mpsc::channel();

        let (config, sample_format) = get_output_stream_config(&device);
        log::debug!("using output stream config: {:?}", config);
        log::debug!("using output sample format: {:?}", sample_format);

        // Start the output management thread
        std::thread::spawn(move || {
            let mut streams: HashMap<T, cpal::Stream> = HashMap::new();

            loop {
                match rx.recv().expect("receive failed") {
                    ControlMessage::AddStream {
                        ident,
                        sample_rate,
                        consumer,
                    } => {
                        // Build a new output stream
                        let stream = build_output_stream(
                            &device,
                            &config,
                            sample_format,
                            sample_rate,
                            consumer,
                        );

                        // Play the output stream
                        stream.play().expect("could not play output stream");

                        // Store the stream so it doesn't drop immediately
                        streams.insert(ident, stream);
                    }
                    ControlMessage::RemoveStream { ident } => {
                        // Drop the output stream
                        streams.remove(&ident);
                    }
                }
            }
        });

        OutputManager { tx }
    }

    /// Creates a new stream for the given identifier, playing data from the provided buffer.
    ///
    /// The stream starts playing as soon as it's created.
    pub fn add(&self, ident: T, sample_rate: cpal::SampleRate, consumer: ringbuf::Consumer<u16>) {
        self.tx
            .send(ControlMessage::AddStream {
                ident,
                sample_rate,
                consumer,
            })
            .expect("failed to send control message");
    }

    /// Removes the output stream associated with the given identifier.
    pub fn remove(&self, ident: T) {
        self.tx
            .send(ControlMessage::RemoveStream { ident })
            .expect("failed to send control message");
    }
}

/// Creates a signal by reading samples from the consumer half of a ring buffer.
///
/// When `next` is called, `ConsumerSignal` will try to pop a sample off the buffer. If there are
/// no more samples, the signal will yield silent frames. The signal will always provide mono
/// frames, and each sample provided by the buffer must constitute a mono frame.
struct ConsumerSignal {
    consumer: ringbuf::Consumer<u16>,
}

impl ConsumerSignal {
    fn new(consumer: ringbuf::Consumer<u16>) -> Self {
        ConsumerSignal { consumer }
    }
}

impl Signal for ConsumerSignal {
    type Frame = dasp::frame::Mono<u16>;

    fn next(&mut self) -> Self::Frame {
        match self.consumer.pop() {
            Some(sample) => [sample],
            None => Self::Frame::EQUILIBRIUM,
        }
    }
}

fn get_output_stream_config(device: &cpal::Device) -> (cpal::StreamConfig, cpal::SampleFormat) {
    // Get output stream configuration, preferring a sample rate of 48 kHz, but falling back to the
    // device's default
    let preferred_sample_rate = cpal::SampleRate(48000);
    let supported_config = device
        .supported_output_configs()
        .unwrap()
        .find(|config| {
            config.min_sample_rate() <= preferred_sample_rate
                && config.max_sample_rate() >= preferred_sample_rate
        })
        .map(|config| config.with_sample_rate(preferred_sample_rate))
        .unwrap_or_else(|| {
            device
                .default_output_config()
                .expect("could not get default output config")
        });

    // Prefer a buffer size of 64, but clamp to be within the supported range. Use the default
    // buffer size if the supported range is unknown.
    let mut stream_config = supported_config.config();
    stream_config.buffer_size = match supported_config.buffer_size() {
        cpal::SupportedBufferSize::Range { min, max } => {
            cpal::BufferSize::Fixed(64.clamp(*min, *max))
        }
        cpal::SupportedBufferSize::Unknown => cpal::BufferSize::Default,
    };

    (stream_config, supported_config.sample_format())
}

fn build_output_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    sample_format: cpal::SampleFormat,
    sample_rate: cpal::SampleRate,
    consumer: ringbuf::Consumer<u16>,
) -> cpal::Stream {
    // Use a function to build the data function to be used by the new output stream in order to
    // make it generic over the sample type
    fn build_data_fn<T: cpal::Sample>(
        consumer: ringbuf::Consumer<u16>,
        channels: cpal::ChannelCount,
        source_sample_rate: cpal::SampleRate,
        target_sample_rate: cpal::SampleRate,
    ) -> impl FnMut(&mut [T], &cpal::OutputCallbackInfo) {
        // Create a signal from the consumer half of the ring buffer and interpolate it to
        // match the required output sample rate
        let mut signal = ConsumerSignal::new(consumer).from_hz_to_hz(
            dasp::interpolate::linear::Linear::new([0], [0]),
            source_sample_rate.0.into(),
            target_sample_rate.0.into(),
        );

        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            for frame in data.chunks_mut(channels.into()) {
                // Create a sample from the first (and only) sample in the next frame
                let sample = cpal::Sample::from(&signal.next()[0]);

                // Play the sample on every channel in the frame
                frame.fill(sample);
            }
        }
    }

    let err_fn = |err| panic!("output err: {:?}", err);

    // Build the new output stream
    match sample_format {
        cpal::SampleFormat::I16 => device.build_output_stream(
            config,
            build_data_fn::<i16>(consumer, config.channels, sample_rate, config.sample_rate),
            err_fn,
        ),
        cpal::SampleFormat::U16 => device.build_output_stream(
            config,
            build_data_fn::<u16>(consumer, config.channels, sample_rate, config.sample_rate),
            err_fn,
        ),
        cpal::SampleFormat::F32 => device.build_output_stream(
            config,
            build_data_fn::<f32>(consumer, config.channels, sample_rate, config.sample_rate),
            err_fn,
        ),
    }
    .expect("could not build output stream")
}
