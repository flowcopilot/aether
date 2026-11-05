# October 2026 dependency security updates

The workspace lock selects rustls 0.23.46, rustls-webpki 0.103.16,
webpki-roots 1.0.10, webpki-root-certs 1.0.10, quinn-proto 0.11.18,
and the Hickory resolver, network, and protocol crates at 0.26.2.
The Hickory crates need matching patches to compile.

The hub selects Bandit 1.13.0 and its required Thousand Island 1.6.0.
The promo keeps npm and ESLint 9. Its updated Remotion lint config removes
braces from the dependency tree. source-map-js is 1.2.2.

## Relay dependency lock

`Dockerfile.relay` builds the published iroh-relay 1.1.0 source archive.
Docker verifies its SHA-256 against the checksum in the crates.io release record.
The build then replaces the published lock with `security/iroh-relay.Cargo.lock`
and uses `--locked`. The workspace lock does not control the relay.

The relay lock updates the same TLS roots, verifier, rustls, and Hickory packages.
The verifier also requires an AWS-LC update. The relay uses noq-proto, not the
workspace's quinn-proto dependency. Keep these dependency graphs separate during
future security checks.

To refresh the relay lock:

1. Download the published archive from the URL in `Dockerfile.relay` into a temporary directory.
2. Verify its SHA-256 against the Dockerfile and the crates.io release record.
3. Extract the archive and copy `security/iroh-relay.Cargo.lock` over its `Cargo.lock`.
4. Run targeted `cargo update -p PACKAGE --precise VERSION` commands in the extracted directory.
5. Build with `cargo build --features server --bin iroh-relay --locked` and check advisories for the resulting lock.
6. Copy the validated lock back to `security/iroh-relay.Cargo.lock`.

## Remaining validation and rollout

Rebuild all three images with fresh base images and without cached package layers.
The Dockerfiles install current Debian `ca-certificates` and `openssl` packages.
Record the installed package versions and confirm the applicable Debian security
fixes before rollout. A successful package install alone does not prove that a
TLS verifier rejects the compromised intermediates.

Run `mix deps.get`, `mix hex.audit`, and `mix test` with Elixir 1.20 and Erlang 29.
The Hex lock entries were verified against the official release metadata and
archive checksums. This does not replace the hub tests.

After image validation, repeat the direct, relay, and managed relay protocol
proofs in an isolated environment. Test the actual application TLS paths,
including the relay's platform verifier and ACME client. Roll out rebuilt
binaries and images only through the normal deployment process.

The workspace still contains two maintenance notices with no patched versions:
`paste` 1.0.15 through iroh, netwatch, and netlink-packet-core on Linux, and
`rustls-pemfile` 2.2.0 through axum-server.
Replacing these crates is separate maintenance work. Advisory scans cannot
establish complete protection.

## Cloudflare account checks

Tracked files contain Cloudflare documentation references, but no cloudflared
pin, Wrangler deployment, or Cloudflare Container configuration. If a deployed
hub uses Cloudflare, its owner must review custom origin certificates and
uploaded mTLS trust separately. Any external cloudflared deployment needs
2026.10.2 or later. Any customer Container or Sandbox needs its own dependency
and trust-store check, followed by a rebuild when affected.

Cloudflare's managed service changes do not establish the safety of customer
runtime trust. No compatibility date change is required by this incident.
This update does not change account TLS settings or deployed certificates.

Sources:

- [Rust certificate verifier advisory](https://rustsec.org/advisories/RUSTSEC-2026-0382.html)
- [Rust root update](https://github.com/rustls/webpki-roots/releases/tag/v%2F1.0.10)
- [Hickory 0.26.2 release](https://github.com/hickory-dns/hickory-dns/releases/tag/v0.26.2)
- [Bandit HTTP/2 advisory](https://github.com/mtrudel/bandit/security/advisories/GHSA-69m6-wcv4-rwcf)
- [Bandit host validation advisory](https://github.com/mtrudel/bandit/security/advisories/GHSA-v53v-9hvg-37jf)
- [iroh-relay release record](https://crates.io/api/v1/crates/iroh-relay/1.1.0)
- [Cloudflare customer actions](https://blog.cloudflare.com/emudhra-certificate-compromise/)
