# Current limitations

Jio is experimental. The API and SDKs do not yet carry a compatibility
promise, and this release is not a production-readiness claim.

- VM data is visible to the host operator. KVM isolation does not provide
  confidential or operator-blind execution.
- Hosted ephemeral sessions expire according to account policy and do not
  support stop/start. Revocation and expiry are enforced by periodic checks,
  not instant disconnection.
- On retained-session endpoints, clean stop/start preserves files. Recovery
  across a Core or host restart, process checkpoints, and recovery of running
  services are not supported.
- A command timeout terminates the local SSH process. It does not guarantee
  cancellation of the guest process. Failed commands are not replayed automatically.
- Standalone Core uses one operator-selected template and does not expose hosted
  account usage or listing. Explicit size selection requires a size-aware endpoint.
- The legacy `jio run` artifact API is unavailable on the default hosted connection;
  use `jio create` and `jio exec`.
- Node.js and Python SDKs must be built from source; GitHub releases publish CLI binaries
  only. SDKs accept an explicit API key or `JIO_API_KEY`, not the CLI's saved login.
- Session access requires local VM credentials. An API key and VM ID alone do not
  let another client machine connect.

[Back to the CLI guide](README.md)
