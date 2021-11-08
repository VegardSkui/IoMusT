# IoMusT

## Running

Both `iomust_peer` and `iomust_server` is written in the Rust programming
language. If not already installed, it can be downloaded from
[rustup](https://rustup.rs/).

### `iomust_peer`

To connect to other peers using `iomust_server`, launch `iomust_peer` as
follows.

```cargo run --bin iomust_peer -- -s SERVER_ADDR```

Alternatively, you may specify your own address and peers manually using the
following command.  Multiple peers can be added using multiple `-p` arguments.

```cargo run --bin iomust_peer -- -a MY_IP:MY_PORT -p PEER_IP:PEER_PORT,PEER_SAMPLE_RATE```

### `iomust_server`

Launch `iomust_server` using the following command.

```cargo run --bin iomust_server -- IP:PORT```

### Logging

By default, the logging level is set to `INFO`. Override the default by setting
the `RUST_LOG` environment variable. Increase the logging level to `TRACE` to
print round-trip time measurements as they are completed.

## Rust version

`iomust_peer` is developed with Rust 1.56.1.

`iomust_server` is written to support Rust 1.48.0, the latest Rust version
available in the Debian Bullseye repositories.
