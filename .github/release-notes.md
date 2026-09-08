First downloadable Jio CLI release, licensed under Apache-2.0.

Includes `jio login <api-key>`, hosted HTTPS access, saved VM sizes, account
usage, VM listing and the persistent-session commands. New configurations default
to Medium (2 vCPU / 4 GiB). This is an experimental client release, not a claim
that the runtime is production-ready or protects VM data from its host operator.

## Install

```sh
curl -fsSL https://github.com/jiovannish/client/releases/latest/download/install.sh | sh
```

The installer verifies the archive's SHA-256 checksum and replaces only the CLI binary. It
does not change API keys, VM keys, shell startup files or saved configuration.
The default location is `~/.local/bin/jio`; add `~/.local/bin` to `PATH` if needed.
License notices are installed under `~/.local/share/jio`.

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

[CLI guide](https://github.com/jiovannish/client/blob/main/doc/README.md) · [Security](https://github.com/jiovannish/client/blob/main/SECURITY.md) · [Code of Conduct](https://github.com/jiovannish/client/blob/main/CODE_OF_CONDUCT.md)
