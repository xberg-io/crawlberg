from __future__ import annotations

import json
import os
import re
import subprocess
import tempfile
import time
import unittest
from pathlib import Path
from typing import ClassVar

ROOT = Path(__file__).resolve().parents[3]
MARKER = b"stage=known_diagnostic_stage boundary=begin attempt=3 generation=11\n"
FIXTURE_MAIN = r"""
fn main() {
    let mut sink = PageOpenDiagnostics::default();
    let marker = b"stage=known_diagnostic_stage boundary=begin attempt=3 generation=11\n";
    std::io::Write::write_all(&mut sink, marker).expect("diagnostic write must succeed");
    std::io::Write::flush(&mut sink).expect("diagnostic flush must succeed");
    assert_eq!(sink.0.lock().expect("diagnostic memory must lock").as_slice(), marker);
    if std::env::args().any(|argument| argument == "--hang") {
        std::thread::sleep(std::time::Duration::from_secs(30));
    }
}
"""


class LiveDiagnosticSinkTests(unittest.TestCase):
    binary: ClassVar[Path]

    @classmethod
    def setUpClass(cls) -> None:
        directory = tempfile.TemporaryDirectory()
        cls.addClassCleanup(directory.cleanup)
        work = Path(directory.name)
        source = (ROOT / "crates/crawlberg/tests/test_browser_pool_lifecycle.rs").read_text(encoding="utf-8")
        matches = re.findall(
            r"(?ms)^#\[derive\(Clone, Default\)\]\nstruct PageOpenDiagnostics.*?(?=^impl PageOpenDiagnostics \{)",
            source,
        )
        if len(matches) != 1:
            raise ValueError(f"expected one actual diagnostic sink, observed={len(matches)}")
        fixture = work / "sink.rs"
        fixture.write_text("use std::sync::{Arc, Mutex};\n" + matches[0] + FIXTURE_MAIN, encoding="utf-8")
        cls.binary = work / ("sink.exe" if os.name == "nt" else "sink")
        compiled = subprocess.run(
            ["rustc", "--edition=2024", "--deny=warnings", str(fixture), "-o", str(cls.binary)],
            check=False,
            capture_output=True,
            text=True,
        )
        if compiled.returncode != 0:
            raise RuntimeError("compiling actual diagnostic sink failed: " + compiled.stdout + compiled.stderr)

    def test_should_emit_exact_stage_bytes_and_preserve_memory(self) -> None:
        result = subprocess.run([str(self.binary)], capture_output=True, check=False)
        assert result.returncode == 0, result.stderr
        assert result.stdout == b""
        assert result.stderr == MARKER

    def test_should_preserve_live_stage_evidence_when_producer_is_killed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "stderr.log"
            with path.open("wb") as stream:
                process = subprocess.Popen([str(self.binary), "--hang"], stderr=stream, stdout=subprocess.DEVNULL)
                try:
                    deadline = time.monotonic() + 2
                    while path.read_bytes() != MARKER and time.monotonic() < deadline and process.poll() is None:
                        time.sleep(0.02)
                    assert process.poll() is None, "producer must still be running before forced termination"
                    process.kill()
                    assert process.wait(timeout=5) != 0
                finally:
                    if process.poll() is None:
                        process.kill()
                        process.wait(timeout=5)
            assert path.read_bytes() == MARKER


if __name__ == "__main__":
    if not __debug__:
        raise RuntimeError("sink regression requires Python assertions enabled")
    result = unittest.main(exit=False).result
    assert result.testsRun == 2
    print(json.dumps({"expected_sink_cases": 2, "observed_sink_cases": result.testsRun}))
    raise SystemExit(0 if result.wasSuccessful() else 1)
