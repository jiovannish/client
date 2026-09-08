# Jio

Create and connect to microVMs from your terminal.

[Documentation](docs/README.md) · [Releases](https://github.com/jiovannish/client/releases) · [Contributing](CONTRIBUTING.md)

## Getting started

Install the latest CLI for macOS or Linux:

```sh
curl -fsSL https://github.com/jiovannish/client/releases/latest/download/install.sh | sh
```

Then log in with your Jio API key:

```sh
jio login '<your-api-key>'
jio create
jio connect
```

The installer chooses a location on `PATH` when available. Requires OpenSSH.
See [installation](docs/README.md#installation) for supported platforms and updates.

## Documentation

Read the [CLI guide](docs/README.md) for commands, configuration, and examples.
[Node.js](bindings/node/README.md) and [Python](bindings/python/README.md) SDKs
are available to build from source.

Jio is experimental. See [current limitations](docs/limitations.md).

## Contributing

Bug reports and pull requests are welcome. Read [Contributing](CONTRIBUTING.md)
and our [Code of Conduct](CODE_OF_CONDUCT.md).

## Security

Report vulnerabilities privately using our [security policy](SECURITY.md).

## License

[Apache-2.0](LICENSE).
