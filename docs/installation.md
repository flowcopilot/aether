# Install Aether

Aether runs on Linux and macOS. A host also needs an agent. See
[Configuration](configuration.md) for agent setup.

The release workflow builds these packages:

- Linux, x86-64 and Arm64: a binary archive and a Debian package. The builds
  use Ubuntu 24.04. Use Ubuntu 24.04 or a compatible system with glibc 2.39
  or later. Alpine Linux needs a source build with its own build tools.
- macOS 15 or later, Intel and Apple Silicon: a binary archive.

Windows is not supported. The node and its agent adapters use Unix sockets.

The commands below need a release made with the new release workflow.
For an older release, use the source build at the end of this page.

## Homebrew

After the maintainer has set up the `flowcopilot/homebrew-tap` repository:

```sh
brew install flowcopilot/tap/aether
aether --help
```

To update:

```sh
brew update
brew upgrade flowcopilot/tap/aether
```

The formula selects the archive for your processor and checks its SHA-256
checksum. On Linux, the same glibc requirement applies as for direct downloads.

## Debian and Ubuntu with apt

Install the [GitHub CLI](https://cli.github.com/) first, or download the files
from the [release page](https://github.com/flowcopilot/aether/releases).
Run the following commands in an empty folder:

```sh
arch=$(dpkg --print-architecture)
gh release download --repo flowcopilot/aether --pattern "aether_*_${arch}.deb" --pattern SHA256SUMS
sha256sum --check --ignore-missing SHA256SUMS
sudo apt install ./aether_*_"$arch".deb
aether --help
```

The supported Debian architecture names are `amd64` and `arm64`. The package
installs `/usr/bin/aether`. It does not start a service. To update, download
and install the package from the next release. There is no Aether apt
repository yet, so `apt update` does not find new Aether releases.

## Binary archive

Run the following commands in an empty folder. They need the GitHub CLI.

```sh
case "$(uname -s)" in
  Darwin) platform=apple-darwin ;;
  Linux) platform=unknown-linux-gnu ;;
  *) echo 'Unsupported operating system' >&2; exit 1 ;;
esac
case "$(uname -m)" in
  arm64|aarch64) cpu=aarch64 ;;
  x86_64) cpu=x86_64 ;;
  *) echo 'Unsupported processor' >&2; exit 1 ;;
esac
gh release download --repo flowcopilot/aether --pattern "aether-*-${cpu}-${platform}.tar.gz" --pattern SHA256SUMS
```

Check the files on Linux:

```sh
sha256sum --check --ignore-missing SHA256SUMS
```

Check the files on macOS:

```sh
shasum -a 256 --check --ignore-missing SHA256SUMS
```

If the check succeeds, install the binary:

```sh
tar -xzf aether-*.tar.gz
mkdir -p "$HOME/.local/bin"
install -m 755 aether "$HOME/.local/bin/aether"
```

Add `~/.local/bin` to `PATH`, then run `aether --help`.
The checksums detect damaged files. They are not release signatures.

## Cargo

Install Rust 1.98 or later, Git, a C compiler, and CMake. The build also
needs network access to download Rust dependencies and the Tailwind CLI.

```sh
cargo install --git https://github.com/flowcopilot/aether.git --locked aether
aether --help
```

This installs the current default branch. To install a release, add its tag,
for example `--tag v0.3.0` when that tag is available. Cargo installs the
binary in `~/.cargo/bin` by default. Run the same command to update.

Use the Git URL in the command. `cargo install aether` selects a package
from crates.io. This project does not yet publish packages there.

## Build with mise

```sh
git clone https://github.com/flowcopilot/aether.git
cd aether
mise install
mise run install-aether
```

This installs the binary in `~/.local/bin`.
