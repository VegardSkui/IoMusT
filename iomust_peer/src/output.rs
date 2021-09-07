use std::collections::HashMap;
use std::hash::Hash;

use cpal::traits::{DeviceTrait, StreamTrait};
use ringbuf::RingBuffer;

// TODO: Should this be made generic over the output sample type? Another option may be to make the
// ringbuffer operate on cpal::Sample.
pub struct OutputManager<T: Eq + Hash> {
    device: cpal::Device,
    stream_config: cpal::StreamConfig,
    streams: HashMap<T, cpal::Stream>,
}

impl<T: Eq + Hash> OutputManager<T> {
    pub fn new(device: cpal::Device, stream_config: cpal::StreamConfig) -> Self {
        OutputManager {
            device,
            stream_config,
            streams: HashMap::new(),
        }
    }

    /// Creates a new stream for the given identifier and returns the producer half of its buffer.
    ///
    /// The stream starts playing immediately. Use the returned producer half to add samples to be
    /// played.
    #[must_use]
    pub fn add(&mut self, ident: T) -> ringbuf::Producer<f32> {
        // Create a ring buffer for the new output stream
        // TODO: Should probably come up with a value other than just guessing
        let buffer = RingBuffer::new(2048);

        // Split the buffer into a producer and consumer
        let (producer, mut consumer) = buffer.split();

        // Build a new output stream
        let stream = self
            .device
            .build_output_stream(
                &self.stream_config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    // Play data from the ring buffer. If the buffer contains no more data, play
                    // silence.
                    for sample in data {
                        *sample = consumer.pop().unwrap_or(0.0);
                    }
                },
                |err| panic!("output err: {:?}", err),
            )
            .expect("could not build output stream");

        // Play the output stream
        stream.play().expect("could not play output stream");

        // Store the stream so it doesn't drop immediately
        self.streams.insert(ident, stream);

        // Return the producer half of the ring buffer
        producer
    }
}
