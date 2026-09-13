Clearer command discovery and Bun-based client tooling.

- Keep `jio run` available for compatible lab endpoints, while hiding it from the main help and command suggestions.
- Explain that `jio run` is unsupported on hosted Jio and direct users to `jio create` and `jio exec`.
- Simplify status wording in CLI messages and documentation while retaining the documented limitations.
- Use Bun and TypeScript for repository tooling, packaging scripts and Node SDK checks.

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
