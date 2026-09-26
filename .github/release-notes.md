`jio forward` adds private TCP access to VM services such as PostgreSQL through authenticated SSH. Both the local listener and guest destination use IPv4 loopback; guest host keys remain pinned.

```sh
jio forward 5432 --local-port 15432
# In another terminal:
psql -h 127.0.0.1 -p 15432 -U <database-user> -d <database>
```

Keep the forward running; Ctrl-C closes it and leaves the VM and database running. `--local-port` avoids conflicts with a local database, and an optional session ID selects another VM. `jio expose` continues to publish HTTP apps through public HTTPS URLs.

**Template compatibility:** forwarding requires guest SSH policy that permits local TCP forwarding to `127.0.0.1`. Existing templates with `AllowTcpForwarding no` reject connections; upgrading the client alone does not update their policy. This client release does not deploy or modify VM templates. Database authentication is still required. The existing hosted gateway has a one-hour stream limit; automatic reconnection is not provided.

Validated locally with real PostgreSQL through direct SSH, hosted-style HTTPS/SSH, and SSH jump-host aliases, including authentication failures, restricted destinations, occupied ports, disconnects and cleanup. These checks used local fixtures; native PostgreSQL inside a rebuilt Jio VM remains unverified.

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

[CLI guide](https://github.com/jiovannish/client/blob/v0.4.0/docs/README.md) · [Security](https://github.com/jiovannish/client/blob/v0.4.0/SECURITY.md) · [Code of Conduct](https://github.com/jiovannish/client/blob/v0.4.0/CODE_OF_CONDUCT.md)
