"""Exercise supported server and CLI configuration in the built Docker image."""

from __future__ import annotations

import argparse
import json
import subprocess
import tempfile
import threading
import time
import uuid
from contextlib import closing, contextmanager
from http.client import HTTPConnection
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import TYPE_CHECKING
from urllib.parse import urlsplit

if TYPE_CHECKING:
    from collections.abc import Iterator


def docker(*args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(["docker", *args], capture_output=True, text=True, timeout=60, check=False)


def successful(*args: str) -> str:
    result = docker(*args)
    assert result.returncode == 0, result.stderr
    return result.stdout.strip()


def status(url: str, token: str | None = None) -> int:
    headers = {"Authorization": f"Bearer {token}"} if token else {}
    target = urlsplit(url)
    assert target.scheme == "http" and target.hostname == "127.0.0.1", url
    with closing(HTTPConnection(target.hostname, target.port, timeout=2)) as connection:
        connection.request("GET", target.path, headers=headers)
        return connection.getresponse().status


@contextmanager
def server(image: str, port: int = 3000, *options: str, default_command: bool = False) -> Iterator[str]:
    name = f"crawlberg-config-test-{uuid.uuid4().hex}"
    command = [] if default_command else ["serve", "--host", "0.0.0.0", "--port", str(port)]
    successful("run", "-d", "--name", name, "-p", f"127.0.0.1::{port}", *options, image, *command)
    try:
        address = successful("port", name, f"{port}/tcp")
        url = f"http://{address}"
        for _ in range(30):
            try:
                if status(f"{url}/health") == 200:
                    yield url
                    return
            except (ConnectionError, TimeoutError):
                pass
            assert successful("inspect", name, "--format", "{{.State.Running}}") == "true", successful("logs", name)
            time.sleep(1)
        raise AssertionError(f"Server did not become healthy: {successful('logs', name)}")
    finally:
        successful("rm", "-f", name)


def test_server_configuration(image: str) -> int:
    token = uuid.uuid4().hex
    checks = 0
    with server(image, 3000, "-e", f"CRAWLBERG_API_TOKEN={token}", default_command=True) as url:
        expected = (("/health", None, 200), ("/version", None, 401), ("/version", uuid.uuid4().hex, 401))
        for path, bearer, code in (*expected, ("/version", token, 200)):
            assert status(f"{url}{path}", bearer) == code, (path, code)
            checks += 1
    print("PASS: default port health, missing/wrong/correct bearer token (4 checks)")
    with server(image, 3107, "-e", f"CRAWLBERG_API_TOKEN={token}") as url:
        assert status(f"{url}/version", token) == 200
        checks += 1
    print("PASS: explicit server port")
    with server(image, 3000, "-e", "CRAWLBERG_API_ALLOW_INSECURE=1") as url:
        assert status(f"{url}/version") == 200
        checks += 1
    print("PASS: explicit insecure bind opt-in")
    name = f"crawlberg-config-test-{uuid.uuid4().hex}"
    try:
        result = docker("run", "--rm", "--name", name, image)
    except subprocess.TimeoutExpired as error:
        raise AssertionError("The server started on the default bind without authentication") from error
    finally:
        docker("rm", "-f", name)
    assert result.returncode != 0 and "without authentication" in result.stderr, result.stderr
    checks += 1
    print("PASS: unauthenticated default bind is refused")
    return checks


class FixtureHandler(BaseHTTPRequestHandler):
    def do_GET(self) -> None:
        agent = self.headers.get("User-Agent", "missing")
        body = f"<html><head><title>{agent}</title></head><body>Configuration fixture.</body></html>".encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/html")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, message: str, *args: object) -> None:
        pass


@contextmanager
def fixture() -> Iterator[str]:
    http = ThreadingHTTPServer(("0.0.0.0", 0), FixtureHandler)
    worker = threading.Thread(target=http.serve_forever, daemon=True)
    worker.start()
    try:
        yield f"http://host.docker.internal:{http.server_port}/"
    finally:
        http.shutdown()
        worker.join()
        http.server_close()


def test_cli_configuration(image: str) -> int:
    inline, mounted = (
        json.dumps({"user_agent": agent, "ssrf": {"deny_private": False}})
        for agent in ("crawlberg-inline-config", "crawlberg-mounted-config")
    )
    checks = 0
    with tempfile.TemporaryDirectory(prefix="crawlberg-config-") as directory, fixture() as url:
        path = Path(directory) / "config.json"
        path.write_text(mounted)
        path.chmod(0o644)
        command = ["scrape", url, "--browser-mode", "never", "--config"]
        options = ["run", "--rm", "--add-host", "host.docker.internal:host-gateway"]
        for overlay, agent in ((inline, "crawlberg-inline-config"), ("@/app/config.json", "crawlberg-mounted-config")):
            output = successful(*options, "-v", f"{path}:/app/config.json:ro", image, *command, overlay)
            result = json.loads(output)
            assert result["metadata"]["title"] == agent, output
            checks += 1
        print("PASS: inline and read-only mounted JSON overlays change fetched User-Agent (2 checks)")
        for overlay, detail in (("{invalid", "key must be a string"), ("@/missing-config.json", "No such file")):
            result = docker("run", "--rm", image, "scrape", url, "--config", overlay)
            assert result.returncode != 0, result.stderr
            assert "invalid config" in result.stderr and detail in result.stderr, result.stderr
            checks += 1
        print("PASS: malformed and missing JSON config are rejected (2 checks)")
    return checks


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", required=True)
    args = parser.parse_args()
    successful("image", "inspect", args.image)
    count = test_server_configuration(args.image) + test_cli_configuration(args.image)
    assert count == 11, f"Expected 11 configuration checks, ran {count}"
    print(f"All {count} configuration checks passed")


if __name__ == "__main__":
    main()
