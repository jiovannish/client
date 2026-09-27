# Current limitations

The API and SDKs do not yet carry a compatibility
promise, and this release is not a production-readiness claim.

- VM data is visible to the host operator. KVM isolation does not provide
  confidential or operator-blind execution.
- Hosted ephemeral sessions expire according to account policy and do not
  support stop/start. Revocation and expiry are enforced by periodic checks,
  not instant disconnection.
- On retained-session endpoints, clean stop/start preserves files. Recovery
  across a Core or host restart, process checkpoints, and recovery of running
  services are not supported.
- Running-session fork stays on the source worker and inherits SSH keys and
  application credentials. Hosted expiring sessions cannot be forked. Public
  ingress mappings and external connection continuity are not inherited.
- An SSH command timeout terminates the local SSH process. It does not guarantee
  cancellation of the guest process. Failed commands are not replayed automatically.
- Standalone Core uses one operator-selected template and does not expose hosted
  account usage or listing. Explicit size selection requires a size-aware endpoint.
- The legacy `jio run` artifact API is unavailable on the default hosted connection;
  use `jio create` and `jio exec`.
- Node.js and Python SDKs must be built from source; GitHub releases publish CLI binaries
  only. SDKs accept an explicit API key or `JIO_API_KEY`, not the CLI's saved login.
- SSH session access requires local VM credentials. The development direct-exec
  API instead authorizes bounded commands with the owning account API key.
- Direct exec permits one active command per VM, rejects a busy VM immediately,
  and caps each runtime generation at 10,000 accepted requests. Lost responses
  have an unknown outcome; requests are not replayed. Timeout/output overflow
  kills the command process group, but a guest administrator can escape that group.
  HTTP disconnect does not cancel an accepted command; lifecycle changes may wait
  for its bounded completion. Streaming, stdin and explicit cancellation use SSH.

[Back to the CLI guide](README.md)
