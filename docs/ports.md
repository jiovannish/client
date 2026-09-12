# Publish an app

The service must enable public ingress. Version 0.2.1 adds
reconnection after clean restart on the new systemd Ubuntu template.
Publish the port from your local terminal, then start your app inside the VM:

```sh
jio expose 3000
# https://<vm-id>-3000.apps.jiovanni.sh
jio expose 3001
# https://<vm-id>-3001.apps.jiovanni.sh
jio ports
jio unexpose 3000
```

Each command accepts an optional session ID and `--host` endpoint override.
Apps may listen on `127.0.0.1` or `::1`. HTTP, SSE and WebSockets are supported.
Closing your local terminal leaves the app published. On systemd templates,
the CLI installs `jio-ingress-<port>.service`, which reconnects after a
clean VM restart using a renewable credential scoped to that VM and port.
The Server must support renewal. Unpublishing, API-key revocation, VM destruction
or absolute VM expiry ends access; reboot does not extend the VM lifetime.
Legacy templates keep the temporary helper: expose the port again after restart.
The URL is public: use application authentication for private content.

Older snapshots may leave loopback down. `jio expose` brings it up
when installing the helper. If you start the app first, run
`sudo ip link set dev lo up` inside the VM before binding to localhost. Future
templates should initialize loopback before declaring the session ready.

## Custom domains

```sh
jio expose 3000 --domain app.example.com
# Or add an alias to an already published port:
jio domains add app.example.com 3000
jio domains status app.example.com
jio domains remove app.example.com
```

Copy the returned TXT proof and A record into your DNS provider. A subdomain can
instead use the returned CNAME target (`gateway.apps.jiovanni.sh`); an apex uses A.
Remove conflicting AAAA records. You keep your current DNS provider and never
share its API key with Jio. The default Jio URL remains available.

Status moves from `pending_dns` to `pending_certificate`; visit the HTTPS URL to
trigger issuance. `published` records a subsequent HTTPS request reaching Jio.
Certificate issuance and propagation can take time. Removing an alias leaves the
VM and default URL intact.

## Helper installation

The CLI downloads its matching Linux release from GitHub, verifies its archive
checksum, caches the binary privately and transfers it over existing authenticated
SSH. The CLI stages helpers in `/var/tmp` to avoid the small `/tmp`
filesystem in older Ubuntu templates. Only a port-specific capability enters the VM; your account API
key stays local. Four ports per VM and sixteen simultaneous connections per port
are supported, subject to worker capacity. Raw TCP/UDP and wildcard custom domains
are not supported.

Local development can select a matching Linux binary with `JIO_INGRESS_BINARY=/absolute/path/to/jio jio expose 3000`.
