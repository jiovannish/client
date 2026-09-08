# ADR 0002: Saved CLI API key

`jio login <api-key> [--host <endpoint>]` validates the supplied key through the
existing authenticated account-usage request before replacing local credentials.
A failed validation leaves the previous login intact. `jio login codex` retains
its existing meaning.

The CLI stores one endpoint/key pair in `~/.jio/credentials`, or under
`JIO_STATE_DIR`, using the existing private-directory and atomic private-file
helpers (0700 directory, 0600 file). The bounded UTF-8 file contains the normalized
endpoint and API key on separate newline-terminated lines. It is separate from
the size config so changing either does not overwrite the other.

All CLI API calls resolve `JIO_API_KEY` first, then the saved key. An explicitly
empty or invalid environment value fails. A saved key is used only for the same
normalized endpoint; another endpoint requires its own login or an explicit
environment key. The endpoint selected at login does not change the default
connection. SDKs continue accepting explicit keys or `JIO_API_KEY`.

The command never prints the key. Positional keys may remain in shell history or
be visible in process arguments; the saved file is plaintext readable by the local
user and host administrator. This is a single local login, without a keychain,
multiple profiles, automatic refresh, or a new server authentication protocol.
