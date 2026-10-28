"""Ensure consumer pin updates preserve custom harness configuration."""

from __future__ import annotations

import importlib.util
import json
import shutil
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("sync_consumer_versions", ROOT / "scripts/sync_consumer_versions.py")
assert SPEC is not None and SPEC.loader is not None
SYNC = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = SYNC
SPEC.loader.exec_module(SYNC)
CURRENT_VERSION = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"]
MAJOR, MINOR, _ = CURRENT_VERSION.split(".", 2)
NEXT_VERSION = f"{MAJOR}.{int(MINOR) + 1}.0"


class ConsumerVersionTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        (self.root / "Cargo.toml").write_text(f'[workspace.package]\nversion = "{NEXT_VERSION}"\n')
        for path in {pin.path for pin in SYNC.PINS}:
            destination = self.root / path
            destination.parent.mkdir(parents=True, exist_ok=True)
            content = (ROOT / path).read_text()
            for pin in (pin for pin in SYNC.PINS if pin.path == path and pin.generated):
                for match in reversed(list(SYNC.re.finditer(pin.pattern, content))):
                    start, end = match.span("version")
                    content = content[:start] + NEXT_VERSION + content[end:]
            destination.write_text(content)

        self.node_path = self.root / "crates/crawlberg-node/package.json"
        self.node_path.parent.mkdir(parents=True, exist_ok=True)
        node = json.loads((ROOT / "crates/crawlberg-node/package.json").read_text())
        node["version"] = NEXT_VERSION
        for name in node["optionalDependencies"]:
            node["optionalDependencies"][name] = NEXT_VERSION
        self.node_path.write_text(json.dumps(node))
        shutil.copytree(ROOT / "crates/crawlberg-node/npm", self.node_path.parent / "npm")
        for manifest in (self.node_path.parent / "npm").glob("*/package.json"):
            package = json.loads(manifest.read_text())
            package["version"] = NEXT_VERSION
            manifest.write_text(json.dumps(package))

    def contents(self) -> dict[str, str]:
        return {pin.path: (self.root / pin.path).read_text() for pin in SYNC.PINS}

    def test_check_reports_stale_manifests_without_writing(self) -> None:
        before = self.contents()
        changes = SYNC.synchronize(self.root, check=True)
        assert len(changes) == len({pin.path for pin in SYNC.PINS if not pin.generated})
        assert self.contents() == before

    def test_sync_preserves_custom_content_and_unrelated_versions(self) -> None:
        before = self.contents()
        SYNC.synchronize(self.root, check=False)
        after = self.contents()
        for path, original in before.items():
            original_pins = original
            updated_pins = after[path]
            for pin in (pin for pin in SYNC.PINS if pin.path == path):
                for content_name in ("original", "updated"):
                    content = original_pins if content_name == "original" else updated_pins
                    for match in reversed(list(SYNC.re.finditer(pin.pattern, content))):
                        start, end = match.span("version")
                        content = content[:start] + "VERSION" + content[end:]
                    if content_name == "original":
                        original_pins = content
                    else:
                        updated_pins = content
            assert original_pins == updated_pins, path
        assert SYNC.synchronize(self.root, check=True) == []
        assert SYNC.synchronize(self.root, check=False) == []
        assert self.contents() == after

    def assert_sync_error(self, expected: str) -> None:
        try:
            SYNC.synchronize(self.root, check=False)
        except ValueError as error:
            actual = str(error)
        else:
            self.fail("Consumer synchronization should reject the invalid pin")
        assert expected in actual

    def test_missing_pin_fails_before_any_manifest_is_written(self) -> None:
        path = self.root / "test_apps/swift_e2e/Package.swift"
        path.write_text(path.read_text().replace("release/swift/", "custom/"))
        before = self.contents()
        self.assert_sync_error("expected 1 release pin")
        assert self.contents() == before

    def test_stale_generated_pin_requires_regeneration_without_partial_writes(self) -> None:
        path = self.root / "test_apps/python/pyproject.toml"
        path.write_text(path.read_text().replace(f"crawlberg>={NEXT_VERSION}", "crawlberg>=1.0.0"))
        before = self.contents()
        assert "test_apps/python/pyproject.toml" in SYNC.synchronize(self.root, check=True)
        self.assert_sync_error("regenerate with Alef")
        assert self.contents() == before

    def assert_check_error(self, expected: str) -> None:
        try:
            SYNC.synchronize(self.root, check=True)
        except ValueError as error:
            actual = str(error)
        else:
            self.fail("Consumer validation should reject the invalid native package")
        assert SYNC.re.search(expected, actual), actual

    def test_unrelated_optional_dependencies_and_inactive_platforms_are_allowed(self) -> None:
        node = json.loads(self.node_path.read_text())
        node["optionalDependencies"]["unrelated-helper"] = "0.1.0"
        self.node_path.write_text(json.dumps(node))
        manifest = self.node_path.parent / "npm/linux-x64-musl/package.json"
        package = json.loads(manifest.read_text())
        package["version"] = "0.1.0"
        manifest.write_text(json.dumps(package))
        SYNC.synchronize(self.root, check=False)
        assert SYNC.synchronize(self.root, check=True) == []

    def test_stale_native_optional_dependency_is_rejected_without_writing(self) -> None:
        node = json.loads(self.node_path.read_text())
        name = next(iter(node["optionalDependencies"]))
        node["optionalDependencies"][name] = "1.0.0"
        self.node_path.write_text(json.dumps(node))
        before = self.node_path.read_text()
        self.assert_check_error("native.*version")
        assert self.node_path.read_text() == before

    def test_missing_native_optional_dependency_is_rejected(self) -> None:
        node = json.loads(self.node_path.read_text())
        node["optionalDependencies"].pop(next(iter(node["optionalDependencies"])))
        self.node_path.write_text(json.dumps(node))
        self.assert_check_error("native.*packages")

    def test_empty_native_targets_are_rejected(self) -> None:
        node = json.loads(self.node_path.read_text())
        node["napi"]["targets"] = []
        self.node_path.write_text(json.dumps(node))
        self.assert_check_error("native.*targets")

    def test_stale_native_platform_manifest_is_rejected(self) -> None:
        node = json.loads(self.node_path.read_text())
        name = next(iter(node["optionalDependencies"]))
        suffix = name.removeprefix(node["name"] + "-")
        manifest = self.node_path.parent / "npm" / suffix / "package.json"
        package = json.loads(manifest.read_text())
        package["version"] = "1.0.0"
        manifest.write_text(json.dumps(package))
        self.assert_check_error("native.*version")

    def test_wrong_native_platform_package_name_is_rejected(self) -> None:
        node = json.loads(self.node_path.read_text())
        name = next(iter(node["optionalDependencies"]))
        suffix = name.removeprefix(node["name"] + "-")
        manifest = self.node_path.parent / "npm" / suffix / "package.json"
        package = json.loads(manifest.read_text())
        package["name"] = "unrelated-native-package"
        manifest.write_text(json.dumps(package))
        self.assert_check_error("native.*name")

    def test_duplicate_dependency_fails_instead_of_silently_rewriting(self) -> None:
        path = self.root / "test_apps/ruby/Gemfile"
        path.write_text(path.read_text() + "\ngem 'crawlberg', '>= 1.0.0'\n")
        self.assert_sync_error("expected 1 release pin(s), found 2")


if __name__ == "__main__":
    unittest.main()
