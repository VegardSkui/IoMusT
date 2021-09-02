use std::{collections::VecDeque, convert::TryInto, net::UdpSocket};

use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    StreamConfig,
};

fn main() {
    let socket = UdpSocket::bind("127.0.0.1:9891").expect("could not bind udp socket");

    let playback_buffer: VecDeque<[f32; 1024]> = VecDeque::new();
    let pb_a = std::sync::Arc::new(std::sync::Mutex::new(playback_buffer));
    let pb_r = pb_a.clone();

    let host = cpal::default_host();
    let output_device = host
        .default_output_device()
        .expect("no output device available");
    println!("using output device: {:?}", output_device.name());

    let mut supported_configs_range = output_device
        .supported_output_configs()
        .expect("error while querying configs");
    let mut supported_config: StreamConfig = supported_configs_range
        .next()
        .expect("no supported config")
        .with_max_sample_rate()
        .into();
    supported_config.buffer_size = cpal::BufferSize::Fixed(64);
    println!("using config: {:?}", supported_config);

    let stream = output_device
        .build_output_stream(
            &supported_config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                if let Some(mut samp) = pb_a.lock().unwrap().pop_front() {
                    println!("data len: {}", data.len());
                    println!("samp len: {}", samp.len());
                    data.swap_with_slice(&mut samp[..data.len()]);
                } else {
                    for sample in data.iter_mut() {
                        *sample = 0.0;
                    }
                }
            },
            move |err| {
                panic!("output err: {:?}", err);
            },
        )
        .unwrap();
    stream.play().unwrap();

    loop {
        let mut buf = [0; 8192];
        let (_amt, src) = socket.recv_from(&mut buf).expect("did not receive");

        let (snr, samp) = buf.split_at(4);
        socket.send_to(&snr, &src).expect("could not send packet");

        let rsamp: Vec<f32> = samp
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
            .collect();
        let mut a: [f32; 1024] = [0.0; 1024];
        a.clone_from_slice(&rsamp[..1024]);
        pb_r.lock().unwrap().push_back(a);
    }
}
