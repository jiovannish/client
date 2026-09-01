# Jio client monorepo

This repository contains the supported ways to use Jio VMs:

- `cli`: the `jio` terminal client;
- `crates/jio-client`: the shared Rust lifecycle, validation, SSH, and local
  credential implementation;
- `bindings/python`: the `jio-sdk` Python package; and
- `bindings/node`: the `@jio/sdk` Node.js and TypeScript package.

The language SDKs are native bindings to the Rust client. They do not duplicate
Core's API contract or bypass its validation.

## Current programmable workflow

Today a developer can create a retained Python 3.12 microVM, run bounded shell
commands, send command input, read and write bounded files, inspect or reattach
the same live session, and explicitly destroy it.

Set an API key and either an SSH host for standalone Core or a loopback URL when
the program runs on the Core host:

```sh
export JIO_API_KEY='replace-with-a-32-byte-or-longer-key'
export JIO_ENDPOINT='ubuntu@jio-host'
```

`JIO_HOST` remains a compatibility alias for `JIO_ENDPOINT`.

Local SDK builds require Rust 1.85.1. The Python package supports Python 3.9+
and the Node package supports Node.js 18+.

### Python

Build and install the package from this checkout:

```sh
python -m pip install ./bindings/python
python examples/python/quickstart.py
```

The synchronous surface uses `Jio`; `AsyncJio` provides the same operations for
asyncio applications.

### Node.js and TypeScript

Build the native package and run the example:

```sh
npm install
npm run build:node
node examples/node/quickstart.mjs
```

All network, SSH, and file operations return Promises.

## SDK model

- `Jio.create()` creates a VM and stores its private client key under
  `~/.jio/sessions/<session-id>` by default.
- `Jio.get(id)` (also available as `inspect`) reads Core lifecycle metadata
  without requiring that key.
- `Jio.attach(id)` opens a programmable handle when this machine owns the
  locally pinned key.
- `Vm.exec()` runs one command through the guest login shell and returns its
  exit code plus binary stdout and stderr.
- `Vm.write_file()` and `Vm.read_file()` transfer at most 16 MiB per call.
- `Vm.destroy()` destroys the Core session and then removes its local key.

The CLI and both SDKs share the same state layout, so a VM created in code can be
opened later with `jio connect <session-id>` from the same client machine.

## Explicit limitations

- The only retained-session template currently available is
  `python3.12-source-v0`. The Node SDK controls that VM; it does not provide a
  Node.js guest runtime.
- Retention lasts only while the current Core process and VM remain alive.
  Workspace persistence, stop/start, pause/resume, checkpoints, and recovery
  after Core or host restart are not supported.
- The current standalone transport requires OpenSSH (`ssh` and `ssh-keygen`) on
  a macOS or Linux client.
- A hosted HTTPS endpoint cannot yet provide programmable SSH without an SSH
  gateway.
- A command timeout terminates the local SSH process; it is not a durable guest
  process-cancellation protocol.
- The native API and SDKs are experimental and do not yet carry a compatibility
  promise.
- The npm and Python package names are buildable locally but are not configured
  for public registry release until their namespace and repository license are
  decided.

## Validation

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo doc --workspace --no-deps
npm run build:node
npm run test:node
```
