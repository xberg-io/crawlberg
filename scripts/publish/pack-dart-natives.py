"""Package native libraries for the generated Dart release downloader."""

from __future__ import annotations

import argparse
import hashlib
import re
import tarfile
from pathlib import Path

PLATFORMS = {
    "macos-arm64": ("macos-aarch64", "libcrawlberg_dart.dylib"),
    "macos-x64": ("macos-x86_64", "libcrawlberg_dart.dylib"),
    "linux-x64": ("linux-x86_64", "libcrawlberg_dart.so"),
    "linux-arm64": ("linux-aarch64", "libcrawlberg_dart.so"),
    "windows-x64": ("windows-x86_64", "crawlberg_dart.dll"),
}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("version")
    parser.add_argument("native_directory", type=Path)
    parser.add_argument("output_directory", type=Path)
    args = parser.parse_args()
    if not re.fullmatch(r"\d+\.\d+\.\d+(?:-[A-Za-z0-9.-]+)?", args.version):
        parser.error("version must be a semantic release version without a leading v")
    for rid, (_, library) in PLATFORMS.items():
        source = args.native_directory / rid / library
        if not source.is_file() or source.stat().st_size == 0:
            parser.error(f"missing or empty native library: {source}")
    args.output_directory.mkdir(parents=True, exist_ok=True)
    for rid, (blob, library) in PLATFORMS.items():
        archive = args.output_directory / f"crawlberg-dart-v{args.version}-{blob}.tar.gz"
        with tarfile.open(archive, "w:gz") as bundle:
            bundle.add(args.native_directory / rid / library, arcname=library)
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        Path(f"{archive}.sha256").write_text(f"{digest}  {archive.name}\n")


if __name__ == "__main__":
    main()
