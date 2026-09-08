# CLI guide

[Installation](#installation) · [Login](#login) · [VMs](#working-with-vms) · [Configuration](#configuration) · [Codex](#codex)

## Installation

```sh
curl -fsSL https://github.com/jiovannish/client/releases/latest/download/install.sh | sh
```

The installer selects your platform, verifies the archive's SHA-256 checksum,
and installs `jio` in `~/.local/bin` if that directory is on your `PATH`.
Otherwise it uses `/usr/local/bin` when that directory is on `PATH` and writable,
or when passwordless sudo is available (as in Jio's Ubuntu VMs). This makes `jio`
available immediately in the same shell. No Rust or Node.js is needed.

If neither option is available, it installs in `~/.local/bin` and prints the exact
`export PATH=...` command to run. A piped script cannot change its parent shell's
`PATH`. Custom `JIO_INSTALL_DIR` locations are always respected.

Supported platforms: macOS 11+ and Linux with glibc 2.35+, on ARM64 and x86-64.
Windows and Alpine/musl are not supported. VM access requires `ssh` and
`ssh-keygen` from OpenSSH.

If your shell cannot find `jio`, add this to your shell configuration:

```sh
export PATH="$HOME/.local/bin:$PATH"
```

Rerun the installer to update. It preserves API keys, VM keys, and configuration.
To choose another install location:

```sh
curl -fsSL https://github.com/jiovannish/client/releases/latest/download/install.sh | JIO_INSTALL_DIR=/absolute/path/bin sh
```

Check your version with `jio --version`. Versioned downloads and checksums are
on the [releases page](https://github.com/jiovannish/client/releases).

## Login

Get an API key from your Jio operator, then run:

```sh
jio login '<your-api-key>'
jio usage
```

Login verifies the key before saving it with owner-only permissions in
`~/.jio/credentials`. Failed verification preserves the previous login.
Keys passed on the command line may remain in shell history.

For automation, set `JIO_API_KEY`; it takes precedence over the saved login.
`jio usage` shows the account's quotas, reservations, and session lifetime.
Keys on the same account share quotas. Session limits depend on your account.

## Working with VMs

```sh
jio create
jio list
jio connect
```

New configurations use Medium: **2 vCPU / 4 GiB**. `create` asks whether to enter
the VM. `connect` opens the current VM; use `jio connect <session-id>` to choose
another. Exiting the shell leaves the VM running until it is destroyed or expires.

To run a command without entering a shell:

```sh
jio exec <session-id> 'cat /etc/os-release' --timeout 30
```

For scripts, redirected `create` output contains only the session ID:

```sh
session="$(jio create)"
jio exec "$session" 'printf "hello from Jio\n"' --timeout 30
jio destroy "$session" --yes
```

If creation reports an uncertain result, use the session ID in the error to
inspect the account with `jio list`, reconnect, or destroy that VM before
creating another. A connection failure does not mean creation failed.

`jio list` includes starting and stopped VMs. Listing needs an API key;
connecting also needs the VM's local credentials on the machine that created it.

### Cleanup

```sh
jio destroy
```

This asks before deleting the current VM and its retained files. Pass a session
ID to select another VM. Use `--yes` for non-interactive scripts.

On endpoints that support retained sessions, you can stop compute and later
start a new VM generation with the same files:

```sh
jio stop <session-id>
jio start <session-id>
```

Hosted ephemeral sessions do not support stop/start; use `destroy` to clean up.

## Configuration

```sh
jio config
```

Select **Size**, choose a profile with the arrow keys, and press Enter to save.
Escape goes back or exits. Changes apply to future VMs.

| Size | vCPU | RAM |
| --- | ---: | ---: |
| Small | 1 | 2 GiB |
| Medium (default) | 2 | 4 GiB |
| Large | 4 | 8 GiB |

Available profiles depend on the service and account quota. Unsupported sizes
fail before creation; Jio does not silently choose a different size.

| Setting | Purpose |
| --- | --- |
| `JIO_API_KEY` | Override the saved login. |
| `JIO_STATE_DIR` | Override the default `~/.jio` state directory. |
| `JIO_ENDPOINT` | Override the built-in Jio endpoint (`JIO_HOST` is a legacy alias). |
| `JIO_CA_CERT` | Use a custom CA certificate for a custom endpoint. |

You can also pass `--host <endpoint>` to connection commands. For a custom login,
use `jio login '<your-api-key>' --host <endpoint>` and select that endpoint on
later commands. Saved keys are only used for the endpoint they were verified with.

## Codex

Copy your local Codex login to the current VM:

```sh
jio login codex
```

To copy the login and open Codex in `/workspace` with approvals and sandboxing
disabled:

```sh
jio yolo codex
```

Both commands accept a session ID after `codex`. The login is stored in the VM's
persistent home directory. The `yolo` command uses a dedicated `jio-yolo` profile
and relies on the VM and its trusted host operator for isolation.

## Help

Run `jio --help` or `jio <command> --help`. Use `jio completion zsh`,
`jio completion bash`, or `jio completion fish` for shell completion setup.

See [limitations](limitations.md) and [contributing](../CONTRIBUTING.md) for more detail.
