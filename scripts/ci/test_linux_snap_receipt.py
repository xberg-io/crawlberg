"""Check mounted-file identity guards with synthetic Snap files, without executing a browser."""

import importlib
import json
import re
import tempfile
import unittest
from collections.abc import Iterator
from contextlib import contextmanager
from pathlib import Path

probe = importlib.import_module("linux-child-attach")


def require(condition: bool) -> None:
    """Keep fixture checks active even when Python runs with optimization enabled."""
    if not condition:
        raise AssertionError("Mounted Snap fixture expectation failed")


@contextmanager
def rejects(message: str) -> Iterator[None]:
    """Require each negative to fail for its intended reason without a pytest dependency."""
    try:
        yield
    except ValueError as error:
        if re.search(message, str(error)) is None:
            raise AssertionError(f"Wrong rejection: {error}") from error
    else:
        raise AssertionError("Expected mounted-file identity rejection")


class SnapReceiptTests(unittest.TestCase):
    """Require separate launcher/engine hashes and fail on invalid or changed mount identities."""

    def setUp(self) -> None:
        """Create only synthetic local files; no Snap mount or real executable is used."""
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.directory = Path(temporary.name)
        self.root = self.directory / "snap/chromium"
        self.launcher = self.root.parent / "bin/chromium"
        self.launcher.parent.mkdir(parents=True)
        self.launcher.write_bytes(b"synthetic launcher bytes")
        self.engine = self.make_revision("1000")
        (self.root / "current").symlink_to("1000")

    def make_revision(self, revision: str) -> Path:
        """Provide the fixed upstream launcher mapping and a synthetic ARM64 ELF header."""
        directory = self.root / revision
        launcher = directory / "bin/chromium.launcher"
        launcher.parent.mkdir(parents=True)
        launcher.write_text('exec "$SNAP/usr/lib/chromium-browser/chrome" "$@"\n')
        engine = directory / "usr/lib/chromium-browser/chrome"
        engine.parent.mkdir(parents=True)
        header = bytearray(64)
        header[:7] = b"\x7fELF\x02\x01\x01"
        header[16:18] = (3).to_bytes(2, "little")
        header[18:20] = (183).to_bytes(2, "little")
        header[20:24] = (1).to_bytes(4, "little")
        header[52:54] = (64).to_bytes(2, "little")
        engine.write_bytes(header + b"synthetic engine bytes")
        return engine

    def identity(self) -> probe.SnapIdentity:
        """Use the explicit fixture root without changing production's fixed Snap path."""
        return probe.snap_browser_identity(str(self.launcher), snap_root=self.root)

    def test_records_engine_and_launcher_separately_and_accepts_stable_files(self) -> None:
        """A stable snapshot records the real fixture bytes with an explicit attestation limit."""
        identity = self.identity()
        require(identity["engine_sha256"] == probe.digest(self.engine))
        require(identity["launcher_sha256"] == probe.digest(self.launcher))
        require(identity["engine_sha256"] != identity["launcher_sha256"])
        require(identity["snap_revision"] == "1000")
        require(Path(identity["engine_path"]) == self.engine.resolve(strict=True))
        require("not running-process attestation" in identity["scope"])
        probe.assert_snap_browser_stable(self.directory, str(self.launcher), identity, snap_root=self.root)
        require(json.loads((self.directory / "browser-after.json").read_text()) == identity)

    def test_rejects_missing_engine(self) -> None:
        """A launcher alone never passes the engine gate."""
        self.engine.unlink()
        with rejects("Mounted Snap Chromium engine is missing"):
            self.identity()

    def test_rejects_empty_engine(self) -> None:
        """An empty file cannot stand in for an engine."""
        self.engine.write_bytes(b"")
        with rejects("nonempty revision-local ELF"):
            self.identity()

    def test_rejects_incorrect_elf(self) -> None:
        """A non-ELF file fails despite having enough bytes."""
        self.engine.write_bytes(b"not an ELF" * 20)
        with rejects("valid AArch64 ELF executable header"):
            self.identity()

    def test_rejects_wrong_architecture(self) -> None:
        """An x86 engine cannot masquerade as the ARM64 runner's mounted engine."""
        data = bytearray(self.engine.read_bytes())
        data[18:20] = (62).to_bytes(2, "little")
        self.engine.write_bytes(data)
        with rejects("valid AArch64 ELF executable header"):
            self.identity()

    def test_rejects_unknown_launcher_mapping(self) -> None:
        """A changed upstream mapping requires review rather than a heuristic fallback."""
        (self.root / "1000/bin/chromium.launcher").write_text("exec some-other-engine\n")
        with rejects("does not declare the expected fixed Chromium engine"):
            self.identity()

    def test_rejects_changed_revision_even_when_engine_bytes_match(self) -> None:
        """Identical engine bytes do not hide a changed selected Snap revision."""
        identity = self.identity()
        other = self.make_revision("1001")
        require(probe.digest(other) == identity["engine_sha256"])
        current = self.root / "current"
        current.unlink()
        current.symlink_to("1001")
        with rejects("mount identity changed during the test batch"):
            probe.assert_snap_browser_stable(self.directory, str(self.launcher), identity, snap_root=self.root)
        require(json.loads((self.directory / "browser-after.json").read_text())["snap_revision"] == "1001")

    def test_rejects_changed_engine_bytes(self) -> None:
        """The after receipt retains a valid-header engine whose bytes changed."""
        identity = self.identity()
        self.engine.write_bytes(self.engine.read_bytes() + b"changed")
        with rejects("mount identity changed during the test batch"):
            probe.assert_snap_browser_stable(self.directory, str(self.launcher), identity, snap_root=self.root)


if __name__ == "__main__":
    unittest.main()
