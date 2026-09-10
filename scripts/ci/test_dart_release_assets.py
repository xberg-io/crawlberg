"""Exercise the Dart downloader's release archive and checksum contract."""

from __future__ import annotations

import hashlib
import subprocess
import tarfile
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PLATFORMS = {
    "macos-arm64": ("macos-aarch64", "libcrawlberg_dart.dylib"),
    "macos-x64": ("macos-x86_64", "libcrawlberg_dart.dylib"),
    "linux-x64": ("linux-x86_64", "libcrawlberg_dart.so"),
    "linux-arm64": ("linux-aarch64", "libcrawlberg_dart.so"),
    "windows-x64": ("windows-x86_64", "crawlberg_dart.dll"),
}


class DartReleaseAssetsTests(unittest.TestCase):
    def test_archives_match_downloader_names_and_verified_payloads(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for rid, (_, library) in PLATFORMS.items():
                native = root / "native" / rid
                native.mkdir(parents=True)
                (native / library).write_bytes(rid.encode())
            result = subprocess.run(
                [
                    "python3",
                    str(ROOT / "scripts/publish/pack-dart-natives.py"),
                    "1.6.0",
                    str(root / "native"),
                    str(root / "dist"),
                ],
                capture_output=True,
                text=True,
                check=False,
            )
            assert result.returncode == 0, result.stderr
            assert len(list((root / "dist").iterdir())) == len(PLATFORMS) * 2
            for rid, (blob, library) in PLATFORMS.items():
                archive = root / "dist" / f"crawlberg-dart-v1.6.0-{blob}.tar.gz"
                assert (
                    archive.with_suffix(".gz.sha256").read_text().split()[0]
                    == hashlib.sha256(archive.read_bytes()).hexdigest()
                )
                with tarfile.open(archive) as bundle:
                    assert bundle.getnames() == [library]
                    payload = bundle.extractfile(library)
                    assert payload is not None
                    assert payload.read() == rid.encode()

    def test_missing_platform_prevents_partial_release_assets(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result = subprocess.run(
                ["python3", str(ROOT / "scripts/publish/pack-dart-natives.py"), "1.6.0", str(root), str(root / "dist")],
                capture_output=True,
                text=True,
                check=False,
            )
            assert result.returncode != 0
            assert not (root / "dist").exists()


if __name__ == "__main__":
    unittest.main()
