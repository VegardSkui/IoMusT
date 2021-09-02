use std::{collections::HashMap, net::UdpSocket};

use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    StreamConfig,
};

fn main() {
    let socket = UdpSocket::bind("127.0.0.1:9892").expect("could not bind to udp socket");
    let recv = std::sync::Arc::new(socket);
    let send = recv.clone();

    let host = cpal::default_host();
    let input_device = host
        .default_input_device()
        .expect("no input device available");

    let mut supported_configs_range = input_device
        .supported_input_configs()
        .expect("error while querying configs");
    let mut supported_config: StreamConfig = supported_configs_range
        .next()
        .expect("no supported config")
        .with_max_sample_rate()
        .into();
    supported_config.buffer_size = cpal::BufferSize::Fixed(64);
    println!("using config: {:?}", supported_config);

    let send_instants: HashMap<u32, std::time::Instant> = HashMap::new();
    let si_s = std::sync::Arc::new(std::sync::Mutex::new(send_instants));
    let si_r = si_s.clone();
    let mut series: u32 = 1;

    let stream = input_device
        .build_input_stream(
            &supported_config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let start = std::time::Instant::now();
                si_s.lock().unwrap().insert(series, start);

                let sample_bytes: Vec<u8> = data
                    .iter()
                    .flat_map(|sample| sample.to_le_bytes())
                    .collect();

                let payload = [&series.to_le_bytes(), sample_bytes.as_slice()].concat();
                send.send_to(&payload, "127.0.0.1:9891")
                    .expect("could not send packet");
                series += 1;
            },
            move |err| {
                panic!("input err: {:?}", err);
            },
        )
        .unwrap();
    stream.play().unwrap();

    for _ in 0..10000 {
        let mut buf = [0; 10];
        let (amt, src) = recv.recv_from(&mut buf).expect("did not receive");
        let snr = u32::from_le_bytes(std::convert::TryInto::try_into(buf.split_at(4).0).unwrap());
        let elapsed = si_r
            .lock()
            .unwrap()
            .get(&snr)
            .expect("unexpected return snr")
            .elapsed();

        println!("interaction for snr={} took {:?}", snr, elapsed);
    }

    drop(stream);
}
