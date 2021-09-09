use std::collections::HashMap;
use std::hash::Hash;

use cpal::traits::{DeviceTrait, StreamTrait};
use fragile::Fragile;
use ringbuf::RingBuffer;

pub struct OutputManager<T: Eq + Hash> {
    device: cpal::Device,
    stream_config: cpal::StreamConfig,
    sample_format: cpal::SampleFormat,
    // TODO: It would be nice to find a way around using the `fragile` crate. For now it's required
    // because we need `OutputManager` to be `Send`, but `cpal::Stream` is not `Send`. This is
    // related to the instruction in the `remove` method.
    streams: HashMap<T, Fragile<cpal::Stream>>,
}

impl<T: Eq + Hash> OutputManager<T> {
    /// Creates a new output manager for the given output device.
    pub fn new(device: cpal::Device) -> Self {
        let supported_config = device
            .default_output_config()
            .expect("no default output config");

        // Prefer a buffer size of 64, but clamp to be within the supported range. Use the default
        // buffer size if the supported range is unknown.
        let mut stream_config = supported_config.config();
        stream_config.buffer_size = match supported_config.buffer_size() {
            cpal::SupportedBufferSize::Range { min, max } => {
                cpal::BufferSize::Fixed(64.clamp(*min, *max))
            }
            cpal::SupportedBufferSize::Unknown => cpal::BufferSize::Default,
        };

        log::debug!(
            "using supported output stream config: {:?}",
            supported_config
        );
        log::debug!("using output stream config: {:?}", stream_config);

        OutputManager {
            device,
            stream_config,
            sample_format: supported_config.sample_format(),
            streams: HashMap::new(),
        }
    }

    /// Creates a new stream for the given identifier and returns the producer half of its buffer.
    ///
    /// The stream starts playing immediately. Use the returned producer half to add samples to be
    /// played.
    #[must_use]
    pub fn add(&mut self, ident: T) -> ringbuf::Producer<u16> {
        // Create a ring buffer for the new output stream
        // TODO: Should probably come up with a value other than just guessing
        let buffer = RingBuffer::new(2048);

        // Split the buffer into a producer and consumer
        let (producer, consumer) = buffer.split();

        // Use a function to build the data function to be used by the new output stream in order
        // to make it generic over the sample type
        fn build_data_fn<T: cpal::Sample>(
            mut consumer: ringbuf::Consumer<u16>,
        ) -> impl FnMut(&mut [T], &cpal::OutputCallbackInfo) {
            move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
                for sample in data {
                    // Play data from the ring buffer. If the buffer contains no more data, play
                    // silence.
                    *sample = match consumer.pop() {
                        Some(sample) => cpal::Sample::from(&sample),
                        None => cpal::Sample::from(&0.0),
                    };
                }
            }
        }

        let err_fn = |err| panic!("output err: {:?}", err);

        // Build the new output stream
        let stream = match self.sample_format {
            cpal::SampleFormat::I16 => self.device.build_output_stream(
                &self.stream_config,
                build_data_fn::<i16>(consumer),
                err_fn,
            ),
            cpal::SampleFormat::U16 => self.device.build_output_stream(
                &self.stream_config,
                build_data_fn::<u16>(consumer),
                err_fn,
            ),
            cpal::SampleFormat::F32 => self.device.build_output_stream(
                &self.stream_config,
                build_data_fn::<f32>(consumer),
                err_fn,
            ),
        }
        .expect("could not build output stream");

        // Play the output stream
        stream.play().expect("could not play output stream");

        // Store the stream so it doesn't drop immediately
        self.streams.insert(ident, Fragile::new(stream));

        // Return the producer half of the ring buffer
        producer
    }

    /// Removes the output stream associated with the given identifier.
    ///
    /// Must be called from the same thread which added the stream.
    pub fn remove(&mut self, ident: &T) {
        // Drop the output stream
        self.streams.remove(ident);
    }
}
