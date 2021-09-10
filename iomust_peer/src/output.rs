use std::collections::HashMap;
use std::hash::Hash;

use cpal::traits::{DeviceTrait, StreamTrait};
use fragile::Fragile;

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
        // Get output stream configuration, preferring a sample rate of 48 kHz, but falling back to
        // the device's default
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

    /// Creates a new stream for the given identifier, playing data from the provided buffer.
    ///
    /// The stream starts playing immediately.
    pub fn add(&mut self, ident: T, consumer: ringbuf::Consumer<u16>) {
        // Use a function to build the data function to be used by the new output stream in order
        // to make it generic over the sample type
        fn build_data_fn<T: cpal::Sample>(
            mut consumer: ringbuf::Consumer<u16>,
            channels: cpal::ChannelCount,
        ) -> impl FnMut(&mut [T], &cpal::OutputCallbackInfo) {
            move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
                for frame in data.chunks_mut(channels.into()) {
                    // Pop a sample from the buffer, if the buffer contains no more data, play
                    // silence
                    let sample = match consumer.pop() {
                        Some(sample) => cpal::Sample::from(&sample),
                        None => cpal::Sample::from(&0.0),
                    };

                    // Play the sample on every channel in the frame
                    frame.fill(sample);
                }
            }
        }

        let err_fn = |err| panic!("output err: {:?}", err);

        // Build the new output stream
        let stream = match self.sample_format {
            cpal::SampleFormat::I16 => self.device.build_output_stream(
                &self.stream_config,
                build_data_fn::<i16>(consumer, self.stream_config.channels),
                err_fn,
            ),
            cpal::SampleFormat::U16 => self.device.build_output_stream(
                &self.stream_config,
                build_data_fn::<u16>(consumer, self.stream_config.channels),
                err_fn,
            ),
            cpal::SampleFormat::F32 => self.device.build_output_stream(
                &self.stream_config,
                build_data_fn::<f32>(consumer, self.stream_config.channels),
                err_fn,
            ),
        }
        .expect("could not build output stream");

        // Play the output stream
        stream.play().expect("could not play output stream");

        // Store the stream so it doesn't drop immediately
        self.streams.insert(ident, Fragile::new(stream));
    }

    /// Removes the output stream associated with the given identifier.
    ///
    /// Must be called from the same thread which added the stream.
    pub fn remove(&mut self, ident: &T) {
        // Drop the output stream
        self.streams.remove(ident);
    }
}
