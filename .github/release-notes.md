X-Large selection and cleaner terminal behavior in `jio config`.

- Add X-Large (8 vCPU, 16 GiB) to the size menu, subject to endpoint availability and account quota.
- Restore the cursor to the start of the prompt when closing the menu, without leaving blank lines or erasing terminal history.

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

[CLI guide](https://github.com/jiovannish/client/blob/main/docs/README.md) · [Security](https://github.com/jiovannish/client/blob/main/SECURITY.md) · [Code of Conduct](https://github.com/jiovannish/client/blob/main/CODE_OF_CONDUCT.md)
