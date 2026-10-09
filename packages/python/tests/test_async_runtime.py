import asyncio
import os
import socket
import subprocess
import sys
import threading
import time
from collections.abc import AsyncGenerator, Iterator
from contextlib import suppress
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

import pytest
from typing_extensions import override

import crawlberg


class _Server(HTTPServer):
    @property
    def url(self) -> str:
        return f"http://127.0.0.1:{self.server_port}"


class _Handler(BaseHTTPRequestHandler):
    def do_GET(self) -> None:
        status = {"/missing": 404, "/forbidden": 403}.get(self.path, 200)
        if self.path.startswith("/slow"):
            time.sleep(0.4)
        self.send_response(status)
        self.send_header("Content-Type", "text/html")
        self.end_headers()
        with suppress(BrokenPipeError):
            self.wfile.write(b"<html><title>Runtime test</title><p>Content</p></html>")

    @override
    def log_message(self, format: str, *args: object) -> None:  # noqa: A002 ~keep Preserve the stdlib override signature.
        del format, args


@pytest.fixture
def local_server() -> Iterator[_Server]:
    """Serve deterministic responses without external network dependencies."""
    server = _Server(("127.0.0.1", 0), _Handler)
    thread = threading.Thread(target=server.serve_forever)
    thread.start()
    try:
        yield server
    finally:
        server.shutdown()
        server.server_close()
        thread.join()


def _engine(timeout: int = 30000) -> crawlberg.CrawlEngineHandle:
    return crawlberg.create_engine(
        crawlberg.CrawlConfig(
            browser=crawlberg.BrowserConfig(mode=crawlberg.BrowserMode.NEVER),
            ssrf=crawlberg.SsrfPolicy(deny_private=False),
            respect_robots_txt=False,
            retry_count=0,
            rate_limit_ms=0,
            request_timeout=timeout,
        )
    )


def test_should_raise_typed_http_and_connection_errors(local_server: _Server) -> None:
    """Async failures retain the declared CrawlError hierarchy and exact category."""

    async def verify() -> None:
        engine = _engine()
        for path, category in [
            ("/missing", crawlberg.NotFoundError),
            ("/forbidden", crawlberg.ForbiddenError),
        ]:
            with pytest.raises(category) as caught:
                await crawlberg.scrape(engine, local_server.url + path)
            assert type(caught.value) is category
            assert isinstance(caught.value, crawlberg.CrawlError)
        with socket.socket() as reserved:
            reserved.bind(("127.0.0.1", 0))
            port = reserved.getsockname()[1]
        with pytest.raises(crawlberg.CrawlConnectionError) as caught:
            await crawlberg.scrape(engine, f"http://127.0.0.1:{port}/")
        assert type(caught.value) is crawlberg.CrawlConnectionError
        assert isinstance(caught.value, crawlberg.CrawlError)
        with pytest.raises(crawlberg.CrawlTimeoutError) as caught:
            await crawlberg.scrape(_engine(50), local_server.url + "/slow")
        assert type(caught.value) is crawlberg.CrawlTimeoutError
        assert isinstance(caught.value, crawlberg.CrawlError)
        crawlberg.shutdown_async_runtime()

    asyncio.run(verify())


def _threads() -> int:
    if sys.platform == "linux":
        return len(list(Path("/proc/self/task").iterdir()))
    return len(subprocess.check_output(["/bin/ps", "-M", "-p", str(os.getpid())]).splitlines()) - 1


def _fds() -> int:
    return len(list(Path("/proc/self/fd" if sys.platform == "linux" else "/dev/fd").iterdir()))


async def _shutdown_when_idle() -> None:
    while True:
        with suppress(RuntimeError):
            crawlberg.shutdown_async_runtime()
            return
        await asyncio.sleep(0.01)


async def _wait_for_resources(threads: int, fds: int) -> None:
    while _threads() != threads or _fds() != fds:
        await asyncio.sleep(0.01)


@pytest.mark.skipif(sys.platform not in {"linux", "darwin"}, reason="requires OS resource counters")
def test_should_release_native_runtime_resources_and_restart(local_server: _Server) -> None:
    """Shutdown frees runtime threads and descriptors, and the same engine can restart."""

    async def verify() -> None:
        crawlberg.shutdown_async_runtime()
        engine = _engine()
        threads, fds = _threads(), _fds()
        result = await crawlberg.scrape(engine, local_server.url + "/ok")
        assert result.status_code == 200
        assert _threads() > threads
        crawlberg.shutdown_async_runtime()
        assert _threads() == threads
        # ~keep Tokio's process-global signal registry retains two descriptors on first use.
        assert _fds() <= fds + 2
        fds = _fds()
        for _ in range(3):
            result = await crawlberg.scrape(engine, local_server.url + "/ok")
            assert result.status_code == 200
            crawlberg.shutdown_async_runtime()
            assert _threads() == threads
            assert _fds() == fds
        events = [event async for event in crawlberg.crawl_stream(engine, local_server.url + "/stream")]
        assert [event.type for event in events] == ["page", "complete"]
        crawlberg.shutdown_async_runtime()
        assert _threads() == threads
        assert _fds() == fds
        pending = asyncio.create_task(crawlberg.scrape(engine, local_server.url + "/slow"))
        await asyncio.sleep(0.05)
        with pytest.raises(RuntimeError, match="work is active"):
            crawlberg.shutdown_async_runtime()
        assert (await pending).status_code == 200
        crawlberg.shutdown_async_runtime()
        assert _threads() == threads
        assert _fds() == fds
        stream = crawlberg.crawl_stream(engine, local_server.url + "/slow-stream")
        assert isinstance(stream, AsyncGenerator)
        first_event = asyncio.ensure_future(anext(stream))
        await asyncio.sleep(0.05)
        with pytest.raises(RuntimeError, match="work is active"):
            crawlberg.shutdown_async_runtime()
        first_event.cancel()
        with suppress(asyncio.CancelledError):
            await first_event
        await stream.aclose()
        await asyncio.wait_for(_shutdown_when_idle(), timeout=5)

        await asyncio.wait_for(_wait_for_resources(threads, fds), timeout=5)
        assert _threads() == threads
        assert _fds() == fds
        result = await crawlberg.scrape(engine, local_server.url + "/after-cancellation")
        assert result.status_code == 200
        crawlberg.shutdown_async_runtime()
        assert _threads() == threads
        assert _fds() == fds

    asyncio.run(verify())
