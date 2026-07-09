"""Shared fixtures for schemathesis API contract tests.

Starts a crawlberg API server before the test session and tears it down after.
The server is built with `--features api` and runs on a random available port.
"""

from __future__ import annotations

import socket
import subprocess
import time
from pathlib import Path

import pytest
import requests
import schemathesis

PROJECT_ROOT = Path(__file__).resolve().parent.parent.parent
STARTUP_TIMEOUT = 60


def _find_free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _wait_for_server(url: str, timeout: float = STARTUP_TIMEOUT) -> None:
    """Block until the server responds to /health or timeout expires."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            resp = requests.get(f"{url}/health", timeout=2)
            if resp.status_code == 200:
                return
        except requests.ConnectionError:
            pass
        time.sleep(0.5)
    msg = f"Server at {url} did not become ready within {timeout}s"
    raise TimeoutError(msg)


@pytest.fixture(scope="session")
def api_url() -> str:
    """Start the API server and yield its base URL."""
    port = _find_free_port()
    base_url = f"http://127.0.0.1:{port}"

    proc = subprocess.Popen(
        [
            "cargo",
            "run",
            "--release",
            "-p",
            "crawlberg-cli",
            "--features",
            "api",
            "--",
            "serve",
            "--host",
            "127.0.0.1",
            "--port",
            str(port),
        ],
        cwd=PROJECT_ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    try:
        _wait_for_server(base_url)
        yield base_url
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()


@pytest.fixture(scope="session")
def openapi_url(api_url: str) -> str:
    """Return the OpenAPI spec URL."""
    return f"{api_url}/openapi.json"


@pytest.fixture(scope="session")
def api_schema(openapi_url: str) -> schemathesis.schemas.BaseSchema:
    """Load the OpenAPI schema from the running server."""
    return schemathesis.openapi.from_url(openapi_url)
