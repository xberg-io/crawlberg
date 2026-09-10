"""Synchronize release pins in hand-maintained consumer manifests."""

from __future__ import annotations

import argparse
import re
from dataclasses import dataclass
from pathlib import Path

import tomllib

VERSION = r"[0-9]+\.[0-9]+\.[0-9]+(?:-[A-Za-z0-9.-]+)?"
CAPTURE = rf"(?P<version>{VERSION})"


@dataclass(frozen=True)
class Pin:
    path: str
    pattern: str
    count: int = 1
    generated: bool = False


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
)


def synchronize(root: Path, *, check: bool) -> list[str]:
    version = tomllib.loads((root / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    changes: dict[str, str] = {}
    for pin in PINS:
        original = changes.get(pin.path, (root / pin.path).read_text())
        matches = list(re.finditer(pin.pattern, original))
        if len(matches) != pin.count:
            raise ValueError(f"{pin.path}: expected {pin.count} release pin(s), found {len(matches)}")
        updated = original
        for match in reversed(matches):
            start, end = match.span("version")
            updated = updated[:start] + version + updated[end:]
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
