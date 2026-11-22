"""Synchronize release pins in hand-maintained consumer manifests."""

from __future__ import annotations

import argparse
import json
import re
import tomllib
from dataclasses import dataclass
from pathlib import Path

VERSION = r"[0-9]+\.[0-9]+\.[0-9]+(?:-[A-Za-z0-9.-]+)?"
CAPTURE = rf"(?P<version>{VERSION})"
# ~keep PyPI respells a semver prerelease (1.14.0-rc.1 -> 1.14.0rc1), and alef writes that spelling.
PEP440_CAPTURE = r"(?P<version>[0-9]+\.[0-9]+\.[0-9]+(?:(?:a|b|rc)[0-9]+)?)"
PRERELEASE_KINDS = {"alpha": "a", "beta": "b", "rc": "rc"}


@dataclass(frozen=True)
class Pin:
    path: str
    pattern: str
    count: int = 1
    generated: bool = False
    pep440: bool = False


def pep440_version(version: str) -> str:
    core, _, prerelease = version.partition("-")
    if not prerelease:
        return core
    kind, _, number = prerelease.partition(".")
    if kind not in PRERELEASE_KINDS or not number.isdigit():
        raise ValueError(f"cannot spell {version} as a PEP 440 version")
    return f"{core}{PRERELEASE_KINDS[kind]}{number}"


PINS = (
    Pin("test_apps/ruby/Gemfile", rf"gem ['\"]crawlberg['\"], ['\"]>= {CAPTURE}['\"]"),
    Pin("test_apps/dart/pubspec.yaml", rf"(?m)^  crawlberg: \^{CAPTURE}$"),
    Pin("test_apps/elixir/mix.exs", rf'\{{:crawlberg, "~> {CAPTURE}"\}}'),
    Pin("test_apps/kotlin_android/build.gradle.kts", rf"io\.xberg\.crawlberg\.android:crawlberg-android:{CAPTURE}", 2),
    Pin("test_apps/node/package.json", rf'"@xberg-io/crawlberg": "\^{CAPTURE}"'),
    Pin("test_apps/wasm/package.json", rf'"@xberg-io/crawlberg-wasm": "\^{CAPTURE}"'),
    Pin("test_apps/go/go.mod", rf"github\.com/xberg-io/crawlberg/packages/go v{CAPTURE}"),
    Pin("e2e/go/go.mod", rf"github\.com/xberg-io/crawlberg/packages/go v{CAPTURE}"),
    Pin(
        "test_apps/java/pom.xml",
        rf"<groupId>io\.xberg\.crawlberg</groupId>\s*<artifactId>crawlberg</artifactId>\s*<version>{CAPTURE}</version>",
    ),
    Pin("test_apps/swift_e2e/Package.swift", rf'release/swift/{CAPTURE}"'),
    Pin("test_apps/zig/build.zig.zon", rf"crawlberg/releases/download/v{CAPTURE}/"),
    Pin("test_apps/zig/build.zig.zon", rf"/crawlberg-zig-v{CAPTURE}\.tar\.gz"),
    Pin("test_apps/python/pyproject.toml", rf'"crawlberg>={CAPTURE}"', generated=True),
    Pin("test_apps/rust/Cargo.toml", rf'crawlberg = \{{ version = "{CAPTURE}"', generated=True),
    Pin(
        "test_apps/csharp/XbergIo.Crawlberg.E2eTests.csproj",
        rf'PackageReference Include="XbergIo\.Crawlberg" Version="{CAPTURE}"',
        generated=True,
    ),
    Pin("test_apps/c/download_ffi.sh", rf"(?m)^VERSION='{CAPTURE}'$", generated=True),
    # ~keep The CLI proxy packages publish in lockstep with the product, and server.json names the
    # ~keep npm package at that same version; the MCP Registry rejects a package version that is not on npm.
    Pin("cli-proxy/npm/package.json", rf'(?m)^  "version": "{CAPTURE}"', generated=True),
    Pin("cli-proxy/pypi/pyproject.toml", rf'(?m)^version = "{PEP440_CAPTURE}"$', generated=True, pep440=True),
    Pin("server.json", rf'(?m)^ +"version": "{CAPTURE}"', 2, generated=True),
)


def native_package_suffix(target: str) -> str:
    architectures = {"x86_64": "x64", "aarch64": "arm64"}
    platforms = {
        "apple-darwin": "darwin",
        "unknown-linux-gnu": "linux-{architecture}-gnu",
        "unknown-linux-musl": "linux-{architecture}-musl",
        "pc-windows-msvc": "win32-{architecture}-msvc",
    }
    architecture, platform = target.split("-", 1)
    architecture = architectures[architecture]
    suffix = platforms[platform]
    return f"{suffix}-{architecture}" if platform == "apple-darwin" else suffix.format(architecture=architecture)


def validate_native_node_versions(root: Path, version: str) -> None:
    directory = root / "crates/crawlberg-node"
    package = json.loads((directory / "package.json").read_text())
    targets = package["napi"]["targets"]
    if not targets:
        raise ValueError("Node native targets must not be empty")
    prefix = package["name"] + "-"
    expected = {prefix + native_package_suffix(target) for target in targets}
    optional = package["optionalDependencies"]
    actual = {name for name in optional if name.startswith(prefix)}
    if actual != expected:
        raise ValueError(
            f"Node native packages do not match configured targets: expected {sorted(expected)}, got {sorted(actual)}"
        )
    if package["version"] != version:
        raise ValueError(f"Node native wrapper version must be {version}; regenerate with Alef")
    for name in sorted(expected):
        manifest = directory / "npm" / name.removeprefix(prefix) / "package.json"
        platform_package = json.loads(manifest.read_text())
        if platform_package["name"] != name:
            raise ValueError(f"Node native package name mismatch in {manifest.relative_to(root)}")
        if optional[name] != version or platform_package["version"] != version:
            raise ValueError(f"Node native package {name} version must be {version}; regenerate with Alef")


def synchronize(root: Path, *, check: bool) -> list[str]:
    version = tomllib.loads((root / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    validate_native_node_versions(root, version)
    changes: dict[str, str] = {}
    for pin in PINS:
        original = changes.get(pin.path, (root / pin.path).read_text())
        matches = list(re.finditer(pin.pattern, original))
        if len(matches) != pin.count:
            raise ValueError(f"{pin.path}: expected {pin.count} release pin(s), found {len(matches)}")
        updated = original
        spelled = pep440_version(version) if pin.pep440 else version
        for match in reversed(matches):
            start, end = match.span("version")
            updated = updated[:start] + spelled + updated[end:]
        if updated != original:
            if pin.generated and not check:
                raise ValueError(f"{pin.path}: generated release pin is stale; regenerate with Alef")
            changes[pin.path] = updated
    if not check:
        for path, content in changes.items():
            (root / path).write_text(content)
    return sorted(changes)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    try:
        changes = synchronize(root, check=args.check)
    except (OSError, ValueError, KeyError) as error:
        parser.exit(1, f"Consumer version validation failed: {error}\n")
    if changes and args.check:
        parser.exit(1, "Consumer release pins are stale: " + ", ".join(changes) + "\n")
    print(f"Consumer release pins {'checked' if args.check else 'synchronized'} ({len(PINS)} targets).")


if __name__ == "__main__":
    main()
