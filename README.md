# Jio client monorepo

This repository contains the supported ways to use Jio VMs:

- `cli`: the `jio` terminal client;
- `crates/jio-client`: the shared Rust lifecycle, validation, SSH, and local
  credential implementation;
- `bindings/python`: the `jio-sdk` Python package; and
- `bindings/node`: the `@jio/sdk` Node.js and TypeScript package.

The language SDKs are native bindings to the Rust client. They do not duplicate
Core's API contract or bypass its validation.

## Install the CLI

```sh
curl -fsSL https://github.com/jiovannish/client/releases/latest/download/install.sh | sh
```

The installer downloads the native macOS or Linux binary, verifies its SHA-256
checksum, and installs `jio` in `~/.local/bin`. Add that directory to your `PATH`
if needed. No Rust, Node.js, or sudo is required. OpenSSH (`ssh` and `ssh-keygen`)
is required to connect to VMs. Rerun the command to update; saved credentials and
configuration are preserved.

Binaries support macOS 11+ and Linux with glibc 2.35+, on ARM64 and x86-64.
For a custom location, set `JIO_INSTALL_DIR` to an absolute path on the `sh` command.

## Current programmable workflow

Today a developer can create a retained microVM, run bounded shell commands,
send command input, read and write bounded files, stop it cleanly, start the
same files as a new VM generation, inspect or reattach it, and explicitly
destroy it.

Log in once with your Jio API key. The connection and public TLS certificate are built in:

```sh
jio login '<your-api-key>'
jio usage
jio create
```

Login verifies the key before saving it with owner-only permissions in
`~/.jio/credentials` (or `$JIO_STATE_DIR/credentials`). Later CLI commands use
that key automatically. `JIO_API_KEY` takes precedence when set; SDKs still use
an explicit key or that environment variable. Login leaves your size preference
and VM credentials intact. Keys passed as arguments may remain in shell history.

Use `jio login '<your-api-key>' --host <endpoint>` for a custom server; pass that
same endpoint on later commands. The saved key is never sent to a different
endpoint. An invalid key or failed verification leaves the previous login intact.

No address or certificate path is needed. TLS and guest SSH identity verification
remain enabled. A fresh CLI selects Medium (2 vCPU / 4 GiB). Existing choices
are preserved; change them with `jio config` if needed. The service must advertise
the selected size; the client does not silently create a different-sized VM.

For development, explicit connection options and `JIO_ENDPOINT` (or its legacy
alias `JIO_HOST`) still override the default. `JIO_CA_CERT` can supply a custom
public CA certificate. Invalid overrides fail instead of silently falling back.

The CLI commands and SDK interfaces are unchanged. A hosted VM handle reuses one
authenticated SSH connection for exec/file/terminal calls. Independent CLI
invocations attach again. A disconnected handle fails without replaying commands;
reattach explicitly. Revocation/expiry close access with a bounded periodic check,
not instant revocation. Closing SSH does not destroy the VM.
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
Building the CLI from source requires Rust 1.88 or newer.

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
jio login '<your-api-key>'

jio usage
session="$(cargo run --quiet --release -p jio-cli -- create)"
cargo run --quiet --release -p jio-cli -- \
  exec "$session" 'cat /etc/os-release' --timeout 30
cargo run --quiet --release -p jio-cli -- stop "$session"
cargo run --quiet --release -p jio-cli -- start "$session"
cargo run --quiet --release -p jio-cli -- destroy "$session" --yes
```

`jio usage` reads the current API key's account limits and current CPU, memory and
disk reservations from Jio. Keys on the same account share
quotas. Pending/uncertain work stays reserved, stopped VMs retain disk, and
confirmed destruction releases it. It also shows the session lifetime limit
(30 minutes for friend accounts). These are allocations, not cumulative spend,
host utilization or billing. Standalone Core does not expose account usage.

`jio list` shows VM IDs and configured sizes for the current API key's account:

```text
VM ID                            - SIZE
0123456789abcdef0123456789abcdef - Large · 4 vCPU · 8 GiB
```

It includes starting and stopped VMs and skips confirmed destroyed entries.
Listing does not require local SSH keys, but connecting still does. The command
reads all pages (up to 10,000 account records); it fails instead of silently
returning a partial list. Standalone Core does not expose account VM listing.

`jio config` opens a compact inline size picker without clearing terminal history.
Enter opens `Size`, the up and
down arrows move between the fixed profiles, Enter saves the choice, and
Backspace or Escape goes back or exits. The selected value is stored privately
in `~/.jio/config` (or `$JIO_STATE_DIR/config`) and is used by later
`jio create` calls:

| Size | vCPUs | RAM |
| --- | ---: | ---: |
| Small | 1 | 2 GiB |
| Medium | 2 | 4 GiB |
| Large | 4 | 8 GiB |

The client sends a size only when the endpoint advertises it in its health
response. An unavailable size fails before creation; it never silently uses a
different template. Old fixed-template endpoints do not advertise their memory
size and cannot satisfy an explicit size request.

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

- Standalone Core currently admits one operator-selected template. The CLI can
  persist a preferred size, but explicit size selection requires an endpoint that
  advertises per-session size selection. Clients do not select a language
  runtime per session or assume one exists in the guest.
- Clean stop/start preserves the workspace and the configured ordinary system
  roots. Core-process restart, outer-host restart, process checkpoints, and
  recovery of running services are not supported.
- The current standalone transport requires OpenSSH (`ssh` and `ssh-keygen`) on
  a macOS or Linux client.
- The legacy one-shot `jio run` artifact API is not available on the default
  Jio connection; use `jio create` and `jio exec`.
- A command timeout terminates the local SSH process; it is not a durable guest
  process-cancellation protocol.
- The native API and SDKs are experimental and do not yet carry a compatibility
  promise.
- The npm and Python package names are buildable locally but are not configured
  for public registry release until their namespace and cross-platform packaging
  are finalized.

## Validation

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo doc --workspace --no-deps
npm run build:node
npm run test:node
```

## License

Copyright 2026 Jio contributors. Licensed under [Apache-2.0](LICENSE).
Third-party dependencies retain their own licenses.
