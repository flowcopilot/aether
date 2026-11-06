#!/usr/bin/env python3
"""Build release packages and a Homebrew formula using the standard library."""

import argparse
import hashlib
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parent.parent
TARGETS = {
    "x86_64-unknown-linux-gnu": ("linux", "amd64"),
    "aarch64-unknown-linux-gnu": ("linux", "arm64"),
    "x86_64-apple-darwin": ("macos", "amd64"),
    "aarch64-apple-darwin": ("macos", "arm64"),
}


def version():
    return tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"]


def archive_name(release, target):
    return f"aether-{release}-{target}.tar.gz"


def package(binary, target, output, release):
    output.mkdir(parents=True, exist_ok=True)
    system, arch = TARGETS[target]
    with tempfile.TemporaryDirectory() as temp:
        stage = Path(temp)
        shutil.copy2(binary, stage / "aether")
        (stage / "aether").chmod(0o755)
        shutil.copy2(ROOT / "LICENSE", stage / "LICENSE")
        shutil.copy2(ROOT / "README.md", stage / "README.md")
        with tarfile.open(output / archive_name(release, target), "w:gz") as archive:
            for name in ("aether", "LICENSE", "README.md"):
                archive.add(stage / name, arcname=name)
        if system == "linux":
            deb = stage / "debian/aether"
            (deb / "usr/bin").mkdir(parents=True)
            (deb / "DEBIAN").mkdir()
            shutil.copy2(stage / "aether", deb / "usr/bin/aether")
            doc = deb / "usr/share/doc/aether"
            doc.mkdir(parents=True)
            shutil.copy2(ROOT / "LICENSE", doc / "copyright")
            installed_size = sum(
                (path.stat().st_size + 1023) // 1024
                for path in (deb / "usr").rglob("*") if path.is_file()
            )
            control = (
                f"Package: aether\nVersion: {release}\nArchitecture: {arch}\n"
                f"Installed-Size: {installed_size}\n"
                "Maintainer: Aether contributors <flowcopilot@users.noreply.github.com>\n"
                "Section: net\nPriority: optional\n"
                "Homepage: https://github.com/flowcopilot/aether\n"
                "Description: Share your personal AI agent with people you trust\n"
            )
            # Resolve the actual shared-library requirements of this binary.
            (stage / "debian/control").write_text("Source: aether\n\n" + control)
            dependencies = subprocess.check_output(
                ["dpkg-shlibdeps", "-O", "-edebian/aether/usr/bin/aether"],
                cwd=stage, text=True
            ).strip().removeprefix("shlibs:Depends=")
            (deb / "DEBIAN/control").write_text(control + f"Depends: {dependencies}\n")
            subprocess.run(
                ["dpkg-deb", "--root-owner-group", "--build", str(deb),
                 str(output / f"aether_{release}_{arch}.deb")], check=True
            )


def finalize(output, release):
    # Refuse to create a formula for an incomplete release.
    for target in TARGETS:
        if not (output / archive_name(release, target)).is_file():
            raise ValueError(f"Missing archive: {target}")
    for arch in ("amd64", "arm64"):
        if not (output / f"aether_{release}_{arch}.deb").is_file():
            raise ValueError(f"Missing Debian package: {arch}")
    formula = [
        "class Aether < Formula",
        '  desc "Share your personal AI agent with people you trust"',
        '  homepage "https://github.com/flowcopilot/aether"',
        f'  version "{release}"',
        '  license "Apache-2.0"',
    ]
    for system, suffix in (("macos", "apple-darwin"), ("linux", "unknown-linux-gnu")):
        formula.append(f"  on_{system} do")
        if system == "macos":
            formula.append('    depends_on macos: :sequoia')
        for arch, cpu in (("arm", "aarch64"), ("intel", "x86_64")):
            name = archive_name(release, f"{cpu}-{suffix}")
            checksum = hashlib.sha256((output / name).read_bytes()).hexdigest()
            formula.extend([
                f"    on_{arch} do",
                f'      url "https://github.com/flowcopilot/aether/releases/download/v{release}/{name}"',
                f'      sha256 "{checksum}"',
                "    end",
            ])
        formula.append("  end")
    formula.extend([
        "", "  def install", '    bin.install "aether"', "  end", "",
        "  test do", '    assert_match "aether", shell_output("#{bin}/aether --help")',
        "  end", "end", "",
    ])
    (output / "aether.rb").write_text("\n".join(formula))
    assets = sorted(p for p in output.iterdir() if p.suffix in (".gz", ".deb", ".rb"))
    (output / "SHA256SUMS").write_text("".join(
        f"{hashlib.sha256(asset.read_bytes()).hexdigest()}  {asset.name}\n" for asset in assets
    ))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("package", "finalize", "version"))
    parser.add_argument("--target", choices=TARGETS)
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--output", type=Path, default=ROOT / "target/dist")
    args = parser.parse_args()
    release = version()
    if not re.fullmatch(r"\d+\.\d+\.\d+", release):
        parser.error("Release version must have the form MAJOR.MINOR.PATCH")
    if args.command == "version":
        print(release)
    elif args.command == "package":
        if not args.target or not args.binary:
            parser.error("package requires --target and --binary")
        package(args.binary.resolve(strict=True), args.target, args.output.resolve(), release)
    else:
        finalize(args.output.resolve(), release)


if __name__ == "__main__":
    main()
