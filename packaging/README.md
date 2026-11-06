# Release packages

`.github/workflows/release.yml` builds native binaries on Linux and macOS,
for x86-64 and Arm64. Pull requests that change Rust or packaging files run
the same build and package checks. A manual run makes CI artifacts without
publishing a release.

## Make a release

1. Set `workspace.package.version` in `Cargo.toml` and update `Cargo.lock`.
2. Merge the change after the release workflow passes on the pull request.
3. Create and push a tag that matches the version, for example `v0.3.1`.

A tag push builds all four binaries with Rust 1.98.0 and `--locked`. Each
binary must pass `--help` before packaging. CI also installs the binary with Cargo, extracts and runs each
archive, then installs and runs each Debian package. `dpkg-shlibdeps`
sets the Debian shared-library requirements from the built binary.

After all builds pass, CI creates `SHA256SUMS` and `aether.rb`. It then
publishes a GitHub release with four archives, two Debian packages, the
formula, and the checksums. The tag must match the Cargo workspace version.
Only stable versions in the form `MAJOR.MINOR.PATCH` are supported.
The workflow does not sign or notarize binaries.

The GitHub release must not already exist. If publication fails, inspect
the release before a new attempt. Do not replace files in a published
release: a formula refers to their checksums.

## Set up Homebrew once

1. Create the public repository `flowcopilot/homebrew-tap`. Add an initial
   commit, for example a README.
2. In the Aether repository, set the Actions variable
   `HOMEBREW_TAP_REPOSITORY` to `flowcopilot/homebrew-tap`.
3. Create an Ed25519 SSH key pair. Add the public key to the tap repository
   as a deploy key with write access. Save the private key in the Aether
   Actions secret `HOMEBREW_TAP_DEPLOY_KEY`. The release workflow uses this
   key to push formula updates. Delete the local key files after you save
   the secret.

After each release, CI copies the generated formula to `Formula/aether.rb`
in the tap and commits it. Without the variable, CI skips this step. The
release still contains the formula for manual use. If the tap update fails,
fix the cause and rerun only that job, or copy the release formula into the
tap yourself. Do not rerun the successful release publication job.

The formula uses the binary archives and their SHA-256 checksums. It supports
macOS 15 or later and compatible Linux systems with glibc 2.39 or later.
Users can then run `brew install flowcopilot/tap/aether`.

## Cargo and apt scope

Cargo installation uses `--git` and needs no registry token. Publishing to
crates.io is a separate step. Check ownership of all three crate names,
add descriptions and version requirements to internal path dependencies,
and verify each package before publishing in dependency order:
`aether-core`, `aether-store`, then `aether`. Do not advertise
`cargo install aether` before the project owns and publishes that name.
See the [Cargo publishing guide](https://doc.rust-lang.org/cargo/reference/publishing.html).

The Debian packages support `apt install ./package.deb`. They do not provide
an apt repository for automatic updates. That needs a package host, a
signing key, and a signed repository index.

## Local package checks

With Python 3.11 or later, package an existing binary:

```sh
python3 scripts/release.py package --target aarch64-apple-darwin --binary path/to/aether
```

Linux packaging also needs `dpkg-dev`. Use the correct target for the binary.
After all four archives and both Debian packages are in `target/dist`:

```sh
python3 scripts/release.py finalize
cd target/dist
sha256sum --check SHA256SUMS
ruby -c aether.rb
```

The native builds and installation checks follow the
[GitHub runner reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners).
The formula follows the
[Homebrew formula guide](https://docs.brew.sh/Formula-Cookbook).
