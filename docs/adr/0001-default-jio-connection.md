# ADR 0001: Built-in Jio connection

The CLI and language bindings resolve connection settings in this order:
explicit option, `JIO_ENDPOINT`, legacy `JIO_HOST`, compiled-in Jio default.
An invalid or empty explicit setting fails; it never silently redirects a key
to another destination. API keys remain required and are never bundled.

Bundle only the public Jio CA certificate, scoped to the exact default HTTPS
origin (including the default port). Both API requests and the SSH tunnel use
this same certificate selection. Other origins use ordinary certificate roots
unless `JIO_CA_CERT` is explicitly supplied. An explicit CA file takes precedence
and an unreadable/invalid file fails closed. TLS verification, hostname checks,
disabled redirects and pinned guest SSH identities remain unchanged.

The bundled CA's DER SHA256 fingerprint is
`FAFA7677F02A842B1598EE20526CC357868BF74090B9F3E50604D6FCE4188A41`.
It was verified against the existing deployed Jio certificate chain. Rotating
this CA or the default address requires a client update; no automatic discovery
or global OS trust-store installation is introduced.

Fresh CLI configuration selects Medium (2 vCPU / 4 GiB). Existing saved
selections are preserved; change them with `jio config`. An unavailable selected
size fails before creation instead of silently choosing another template.
