`jio create --exec` creates a VM and runs its first command in one API request. Short hosted `jio exec` commands now use the guest control channel; commands requesting more than 30 seconds continue to use SSH.

```sh
jio create --exec 'printf hello'
jio exec <session-id> 'uname -a'
jio update
```

- `jio update` installs the latest release into the current executable’s directory, verifies the archive checksum and binary version, and preserves saved credentials and VM state. The directory must be writable.
- Public HTTP exposures and private TCP forwards have clearer errors and port listings.
- The Rust SDK exposes direct execution and create-and-exec, with an opt-in hosted lifecycle acceptance example. Node.js and Python bindings retain SSH execution.

Direct execution requires a compatible Server, Core and guest template. It is bounded to 30 seconds and 64 KiB of combined output. An unsupported endpoint falls back to SSH for `jio exec`; unknown execution outcomes are not retried automatically. SDK binaries remain source-build only.

## Install

macOS / Linux:

```sh
curl -fsSL https://github.com/jiovannish/client/releases/latest/download/install.sh | sh
```

Windows (PowerShell):

```powershell
irm https://github.com/jiovannish/client/releases/latest/download/install.ps1 | iex
```

Windows (CMD):

```bat
powershell.exe -NoProfile -ExecutionPolicy Bypass -Command "irm https://github.com/jiovannish/client/releases/latest/download/install.ps1 | iex"
```

The installers verify archive checksums and preserve API keys, VM keys, and saved configuration. Windows installs into `%LOCALAPPDATA%\Jio\bin` and updates the user PATH; reopen CMD afterward. Enable **OpenSSH Client** in Windows Settings → Optional features.

Native binaries: macOS ARM64 and Intel (11+), Linux ARM64 and x86-64 (glibc 2.35+), and Windows x64. OpenSSH is required. Windows ARM64 and Alpine/musl binaries are not included.

Each archive includes the CLI, Apache-2.0 license, and dependency notices. Checksums detect download corruption; they are not independent publisher signatures. Node.js and Python SDKs remain source-build only.

Published app URLs are public; use application authentication for private content. Persistent port publication requires a compatible Server and systemd guest; older templates require republishing after restart.

[CLI guide](https://github.com/jiovannish/client/blob/v0.5.0/docs/README.md) · [Security](https://github.com/jiovannish/client/blob/v0.5.0/SECURITY.md) · [Code of Conduct](https://github.com/jiovannish/client/blob/v0.5.0/CODE_OF_CONDUCT.md)
