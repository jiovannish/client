Fixes the unclear `No such file or directory` error when `jio create` cannot
run `ssh-keygen`. The error now names the missing command and explains that
OpenSSH must be installed and available on `PATH`. The CLI guide includes
installation commands for Arch Linux and Ubuntu/Debian.

This remains an experimental CLI release. Account quotas, session lifetimes,
and VM host trust requirements are unchanged.

## Install

```sh
curl -fsSL https://github.com/jiovannish/client/releases/latest/download/install.sh | sh
```

The installer verifies the archive's SHA-256 checksum and replaces only the CLI binary. It
does not change API keys, VM keys, shell startup files or saved configuration.
The installer uses `~/.local/bin` when it is on `PATH`, otherwise `/usr/local/bin`
when writable or accessible with passwordless sudo. If neither is available, it
uses `~/.local/bin` and prints an `export PATH=...` command. License notices are
installed under `../share/jio` relative to the install directory.

Native binaries: macOS ARM64 and Intel (11+), Linux ARM64 and x86-64 (glibc 2.35+).
OpenSSH (`ssh` and `ssh-keygen`) is required for VM access. Windows and Alpine/musl
are not supported by these binaries. Linux client support does not imply ARM
support for the VM host runtime.

Run `jio login '<your-api-key>'`, then `jio usage`, `jio create`, `jio list` and
`jio connect`. Account quotas, session expiry and available sizes are enforced by
the service. Exiting a shell does not destroy the VM; use `jio destroy` to remove it.

Source is attached by GitHub for this exact tag. Each archive contains the CLI,
Apache license and bundled dependency notices. Checksums detect corrupted
downloads; they are not independent signatures of the publisher.

The npm and Python SDKs remain source-build only in this release.

## Documentation

[CLI guide](https://github.com/jiovannish/client/blob/main/docs/README.md) · [Security](https://github.com/jiovannish/client/blob/main/SECURITY.md) · [Code of Conduct](https://github.com/jiovannish/client/blob/main/CODE_OF_CONDUCT.md)
