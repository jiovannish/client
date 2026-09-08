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

Today a developer can create a retained microVM, run bounded shell commands,
send command input, read and write bounded files, stop it cleanly, start the
same files as a new VM generation, inspect or reattach it, and explicitly
destroy it.

Set an API key and either an SSH host for standalone Core or a loopback URL when
the program runs on the Core host:

```sh
export JIO_API_KEY='replace-with-a-32-byte-or-longer-key'
export JIO_ENDPOINT='ubuntu@jio-host'
```

`JIO_HOST` remains a compatibility alias for `JIO_ENDPOINT`.

If create returns an uncertain error, local authority is retained under the
session ID included in that error. Inspect/reattach or destroy that ID before
creating another VM; a transport error does not establish that admission failed.

The client negotiates the current caller-assigned session-ID contract and the
transitional runtime-selected contract exposed by earlier standalone Core
builds. Transitional responses provide guest-, storage-, network-, and
SSH-ready boundaries only; finer-grained timing fields are returned as
`None`/`undefined` rather than synthesized.

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
- `Vm.stop()` cleanly flushes and detaches the persistent filesystems.
- `Vm.start()` creates the next VM generation from those files and atomically
  replaces the locally pinned, generation-specific SSH host key.
- `Vm.exec()` runs one command through the guest login shell and returns its
  exit code plus binary stdout and stderr.
- `Vm.write_file()` and `Vm.read_file()` transfer at most 16 MiB per call.
- `Vm.destroy()` destroys the Core session and then removes its local key.

The CLI and both SDKs share the same state layout, so a VM created in code can be
opened later with `jio connect <session-id>` from the same client machine.

### Run commands from the CLI

The CLI can run a bounded shell command without opening an interactive session.
The current implementation uses the same SSH transport as the SDK; the command
interface does not depend on that transport remaining SSH.

```sh
export JIO_ENDPOINT='ubuntu@jio-host'
export JIO_API_KEY='replace-with-a-32-byte-or-longer-key'

session="$(cargo run --quiet --release -p jio-cli -- create)"
cargo run --quiet --release -p jio-cli -- \
  exec "$session" 'cat /etc/os-release' --timeout 30
cargo run --quiet --release -p jio-cli -- stop "$session"
cargo run --quiet --release -p jio-cli -- start "$session"
cargo run --quiet --release -p jio-cli -- destroy "$session" --yes
```

When run from a terminal, `jio create` asks whether to enter the VM immediately.
When its output is redirected, it prints only the session ID for scripts.
Successful `create` and `connect` commands also select that VM as the current
session, so `jio connect` and agent commands can omit the session ID.
`jio destroy` also defaults to the current session, but asks before permanently
removing the VM and retained files. Non-interactive automation must pass
`--yes`.

```sh
cargo run --quiet --release -p jio-cli -- create
```

To transfer the local Codex login into the current VM:

```sh
jio login codex
```

To transfer the login and open Codex interactively in `/workspace` with Codex
approvals and sandboxing explicitly disabled:

```sh
jio yolo codex
```

This mode relies on the Jio VM as the external isolation boundary. The local
Codex login is copied into the VM's persistent home directory. Either command
can take an explicit session ID after `codex` to override the current session.
The `yolo` invocation installs and selects a dedicated `jio-yolo` Codex profile
that trusts `/workspace`, avoiding Codex's project-trust prompt without changing
the user's default Codex profile.

## Explicit limitations

- Standalone Core currently admits one operator-selected template; clients do
  not select a runtime per session, and the client does not assume a guest
  language runtime.
- Clean stop/start preserves the workspace and the configured ordinary system
  roots. Core-process restart, outer-host restart, process checkpoints, and
  recovery of running services are not supported.
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
