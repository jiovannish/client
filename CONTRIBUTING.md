# Contributing

Open an issue for bugs or proposed changes, or send a focused pull request.
Include reproduction steps and the output of `jio --version` for bugs. Keep
credentials and private VM data out of issues and logs.

Follow the [Code of Conduct](CODE_OF_CONDUCT.md). Report vulnerabilities using
[SECURITY.md](SECURITY.md).

## Development

Install Rust through rustup; `rust-toolchain.toml` pins the toolchain.

```sh
cargo build --locked --release -p jio-cli
cargo fmt --all -- --check
cargo clippy --locked -p jio-cli -p jio-client --all-targets -- -D warnings
cargo test --locked -p jio-cli -p jio-client
RUSTDOCFLAGS='-D warnings' cargo doc --locked -p jio-cli -p jio-client --no-deps
node scripts/test-install.mjs
```

The installer check also requires Node.js. The CLI binary is `target/release/jio`.
Add a regression test for behavior changes; documentation changes only need
command and link checks.

`cli/` contains the CLI; `crates/jio-client/` contains the shared Rust client.
The [Node.js](bindings/node/README.md) and [Python](bindings/python/README.md)
SDKs wrap the same client and share its local VM credentials.

To build the Node.js SDK, run `npm ci`, `npm run build:node`, and
`npm run test:node`. To install the Python SDK, run
`python -m pip install ./bindings/python`. Examples are in [examples/](examples/).

Keep user documentation in [doc/](doc/README.md) and record changes to durable
contracts in [doc/adr/](doc/adr/).
