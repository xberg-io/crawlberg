import asyncio
import ctypes
import errno
import inspect
import os
import socket
import subprocess
import sys
import threading
import time
from collections.abc import AsyncGenerator, Iterator
from concurrent.futures import ThreadPoolExecutor
from contextlib import suppress
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

import pytest

import crawlberg

if sys.version_info >= (3, 12):
    from typing import override
else:
    from typing_extensions import override


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


def _workqueue_threads() -> int:
    if sys.platform != "darwin":
        return 0
    from urllib import request

    system_proxy_query = getattr(request, "getproxies_macosx_sysconf", None)
    if not callable(system_proxy_query):
        raise RuntimeError("macOS system proxy query is unavailable")
    # ~keep A missing queue and a contended queue both report ESRCH; initialize before measuring either.
    system_proxy_query()
    proc_pidworkqueueinfo = 12
    workqueue_info = (ctypes.c_uint32 * 4)()
    libproc = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
    libproc.proc_pidinfo.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_uint64, ctypes.c_void_p, ctypes.c_int]
    libproc.proc_pidinfo.restype = ctypes.c_int
    size = ctypes.sizeof(workqueue_info)
    deadline = time.monotonic() + 0.1
    while True:
        ctypes.set_errno(0)
        observed = libproc.proc_pidinfo(os.getpid(), proc_pidworkqueueinfo, 0, ctypes.byref(workqueue_info), size)
        error = ctypes.get_errno()
        if observed == size:
            return int(workqueue_info[0])
        if observed != 0 or error != errno.ESRCH or time.monotonic() >= deadline:
            raise OSError(error, f"reading macOS workqueue threads returned {observed} bytes, expected {size}")
        time.sleep(0.001)


def _threads() -> int:
    if sys.platform == "linux":
        return len(list(Path("/proc/self/task").iterdir()))
    for _ in range(10):
        before = _workqueue_threads()
        total = len(subprocess.check_output(["/bin/ps", "-M", "-p", str(os.getpid())]).splitlines()) - 1
        after = _workqueue_threads()
        # ~keep SystemConfiguration queues outlive Tokio and can exit while ps samples the total.
        if before == after:
            return total - after
    raise RuntimeError("could not obtain a stable application thread count after 10 attempts")


@pytest.mark.parametrize("child", [False, True], ids=["parent", "exit-child"])
@pytest.mark.parametrize(
    "workqueues",
    [(2, 1, 1, 1), (1, 2, 2, 2), (0, 0), (2, 2)],
    ids=["decreasing", "increasing", "no-workqueue", "stable-workqueue"],
)
def test_should_retry_until_thread_count_has_a_stable_bracket(
    monkeypatch: pytest.MonkeyPatch, child: bool, workqueues: tuple[int, ...]
) -> None:
    namespace: dict[str, object] = globals()
    if child:
        namespace = {"os": os}
        exec(_EXIT.split("def settled_threads():")[0], namespace)  # noqa: S102 ~keep Evaluate our own child sampler.
    queue_reads = iter(workqueues)
    totals = iter((workqueues[0] + 2,) if len(workqueues) == 2 else (3, workqueues[-1] + 2))
    reads: list[str] = []

    def queue_count() -> int:
        reads.append("workqueue")
        return next(queue_reads)

    def total_count(command: list[str]) -> bytes:
        assert command == ["/bin/ps", "-M", "-p", str(os.getpid())]
        reads.append("total")
        return b"thread\n" * (next(totals) + 1)

    monkeypatch.setattr(sys, "platform", "darwin")
    monkeypatch.setitem(namespace, "_workqueue_threads", queue_count)
    monkeypatch.setattr(subprocess, "check_output", total_count)
    counter = namespace["threads" if child else "_threads"]
    assert callable(counter)
    assert counter() == 2
    assert reads == ["workqueue", "total", "workqueue"] * (len(workqueues) // 2)


@pytest.mark.parametrize("child", [False, True], ids=["parent", "exit-child"])
def test_should_fail_visibly_when_workqueue_count_never_stabilizes(
    monkeypatch: pytest.MonkeyPatch, child: bool
) -> None:
    namespace: dict[str, object] = globals()
    if child:
        namespace = {"os": os}
        exec(_EXIT.split("def settled_threads():")[0], namespace)  # noqa: S102 ~keep Evaluate our own child sampler.
    reads: list[str] = []

    def queue_count() -> int:
        reads.append("workqueue")
        return len(reads) % 3

    def total_count(command: list[str]) -> bytes:
        assert command == ["/bin/ps", "-M", "-p", str(os.getpid())]
        reads.append("total")
        return b"thread\n" * 4

    monkeypatch.setattr(sys, "platform", "darwin")
    monkeypatch.setitem(namespace, "_workqueue_threads", queue_count)
    monkeypatch.setattr(subprocess, "check_output", total_count)
    counter = namespace["threads" if child else "_threads"]
    assert callable(counter)
    with pytest.raises(RuntimeError, match=r"stable.*thread count"):
        counter()
    assert reads == ["workqueue", "total", "workqueue"] * 10


def _settled_threads(expected: int) -> int:
    """Count the threads, and give a joined thread up to five seconds to leave the task list of the OS."""
    deadline = time.monotonic() + 5
    while (count := _threads()) != expected and time.monotonic() < deadline:
        time.sleep(0.01)
    return count


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
        assert _settled_threads(threads) == threads
        # ~keep Tokio's process-global signal registry retains two descriptors on first use.
        assert _fds() <= fds + 2
        fds = _fds()
        for _ in range(3):
            result = await crawlberg.scrape(engine, local_server.url + "/ok")
            assert result.status_code == 200
            crawlberg.shutdown_async_runtime()
            assert _settled_threads(threads) == threads
            assert _fds() == fds
        events = [event async for event in crawlberg.crawl_stream(engine, local_server.url + "/stream")]
        assert [event.type for event in events] == ["page", "complete"]
        crawlberg.shutdown_async_runtime()
        assert _settled_threads(threads) == threads
        assert _fds() == fds
        pending = asyncio.create_task(crawlberg.scrape(engine, local_server.url + "/slow"))
        await asyncio.sleep(0.05)
        with pytest.raises(RuntimeError, match="work is active"):
            crawlberg.shutdown_async_runtime()
        assert (await pending).status_code == 200
        crawlberg.shutdown_async_runtime()
        assert _settled_threads(threads) == threads
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
        assert _settled_threads(threads) == threads
        assert _fds() == fds
        result = await crawlberg.scrape(engine, local_server.url + "/after-cancellation")
        assert result.status_code == 200
        crawlberg.shutdown_async_runtime()
        assert _settled_threads(threads) == threads
        assert _fds() == fds

    asyncio.run(verify())


_SETUP = """
import asyncio
import os
import crawlberg

def engine():
    return crawlberg.create_engine(crawlberg.CrawlConfig(
        browser=crawlberg.BrowserConfig(mode="never"),
        ssrf=crawlberg.SsrfPolicy(deny_private=False),
        respect_robots_txt=False, retry_count=0, rate_limit_ms=0,
    ))

async def scrape():
    assert (await crawlberg.scrape(engine(), sys.argv[1])).status_code == 200
"""

_FORK = (
    """
import signal
import sys
"""
    + _SETUP
    + """
warm = sys.argv[2] == "warm"
async def child_work():
    if sys.argv[3] == "stream":
        stream = crawlberg.crawl_stream(engine(), sys.argv[1])
        try:
            events = [event.type async for event in stream]
            assert events == ["page", "complete"]
        finally:
            await stream.aclose()
    else:
        await scrape()
if warm:
    asyncio.run(scrape())
pid = os.fork()
if pid == 0:
    signal.alarm(10)
    try:
        asyncio.run(child_work())
    except RuntimeError as error:
        if not warm or "fork" not in str(error).lower():
            raise
        print("child rejected inherited runtime", flush=True)
    else:
        print("child scraped", flush=True)
    sys.exit(0)
_, status = os.waitpid(pid, 0)
assert os.waitstatus_to_exitcode(status) == 0, status
asyncio.run(scrape())
print("parent scraped", flush=True)
"""
)

_EXIT = (
    """
import atexit
import ctypes
import errno
import subprocess
import sys
import time
from pathlib import Path
"""
    + inspect.getsource(_workqueue_threads)
    + inspect.getsource(_threads)
    + """
threads = _threads

def settled_threads():
    # ~keep: a joined thread can stay in the task list of the OS for a moment.
    deadline = time.monotonic() + 5
    while (count := threads()) != 1 and time.monotonic() < deadline:
        time.sleep(0.01)
    return count

def after_runtime_close():
    assert settled_threads() == 1, threads()
    async def rejected():
        try:
            await crawlberg.scrape(engine(), sys.argv[1])
        except RuntimeError as error:
            assert "interpreter exit" in str(error), str(error)
        else:
            raise AssertionError("runtime restarted during interpreter exit")
    asyncio.run(rejected())
    assert settled_threads() == 1, threads()
    if "exit_started" in globals():
        assert time.monotonic() - exit_started[0] < 6
    print("exit checked", flush=True)

atexit.register(after_runtime_close)
"""
    + _SETUP
    + """
async def work():
    for _ in range(20):
        await scrape()
asyncio.run(work())
print("work done", flush=True)
"""
)

_UNREAD = (
    _EXIT.split("async def work():")[0]
    + """
import time
from urllib.request import urlopen

exit_started = []
atexit.register(lambda: exit_started.append(time.monotonic()))
loop = asyncio.new_event_loop()
stream = crawlberg.crawl_stream(engine(), sys.argv[1])
assert loop.run_until_complete(anext(stream)).type == "page"
deadline = time.monotonic() + 5
while True:
    with urlopen(sys.argv[1] + "progress", timeout=2) as response:
        if int(response.read()) >= 33:
            break
    assert time.monotonic() < deadline, "producer did not advance the unread stream"
    time.sleep(0.01)
print("work done", flush=True)
"""
)


def _run(script: str, url: str, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, "-X", "faulthandler", "-c", script, url, *args],
        capture_output=True,
        text=True,
        timeout=15,
        check=False,
    )


@pytest.mark.skipif(sys.platform not in {"linux", "darwin"}, reason="requires os.fork")
@pytest.mark.parametrize("warm", [False, True], ids=["before-runtime", "after-runtime"])
@pytest.mark.parametrize("operation", ["scrape", "stream"])
def test_should_complete_or_reject_forked_work_and_keep_parent_usable(
    local_server: _Server, warm: bool, operation: str
) -> None:
    result = _run(_FORK, local_server.url, "warm" if warm else "cold", operation)
    assert result.returncode == 0, result.stderr
    assert "Exception ignored in atexit callback" not in result.stderr
    assert "parent scraped" in result.stdout
    expected = "child rejected inherited runtime" if warm else "child scraped"
    assert expected in result.stdout


@pytest.mark.skipif(sys.platform not in {"linux", "darwin"}, reason="requires OS thread counters")
def test_should_close_runtime_before_interpreter_exit_and_reject_restart(local_server: _Server) -> None:
    def trial(_: int) -> subprocess.CompletedProcess[str]:
        return _run(_EXIT, local_server.url)

    with ThreadPoolExecutor(max_workers=4) as workers:
        results = list(workers.map(trial, range(20)))
    assert len(results) == 20
    for result in results:
        assert result.returncode == 0, result.stderr
        assert "work done" in result.stdout
        assert "exit checked" in result.stdout
        assert result.stderr == ""


@pytest.mark.skipif(sys.platform not in {"linux", "darwin"}, reason="requires OS thread counters")
def test_should_close_an_unread_crawl_stream_within_the_exit_wait_bound() -> None:
    page_paths: list[str] = []

    class Handler(_Handler):
        @override
        def do_GET(self) -> None:
            if self.path not in {"/", "/progress"}:
                page_paths.append(self.path)
                super().do_GET()
                return
            body = (
                "".join(f'<a href="/item/{index}">item</a>' for index in range(64)).encode()
                if self.path == "/"
                else str(len(page_paths)).encode()
            )
            self.send_response(200)
            self.send_header("Content-Type", "text/html")
            self.end_headers()
            self.wfile.write(body)

    server = _Server(("127.0.0.1", 0), Handler)
    worker = threading.Thread(target=server.serve_forever)
    worker.start()
    try:
        result = _run(_UNREAD, server.url + "/")
        assert len(page_paths) >= 33
        assert result.returncode == 0, result.stderr
        assert "exit checked" in result.stdout
        assert result.stderr == ""
    finally:
        server.shutdown()
        server.server_close()
        worker.join()


@pytest.mark.skipif(sys.platform != "darwin", reason="requires macOS framework workqueues")
def test_should_count_application_threads_without_counting_framework_workqueues() -> None:
    """A framework queue is excluded, while a live application thread still counts."""
    script = """
import runpy
import sys
import threading
import _scproxy

counters = runpy.run_path(sys.argv[1])
threads = counters["_threads"]
assert threads() == 1
_scproxy._get_proxy_settings()
_scproxy._get_proxies()
assert threads() == 1
assert counters["_workqueue_threads"]() > 0
stop = threading.Event()
worker = threading.Thread(target=stop.wait)
worker.start()
try:
    assert threads() == 2
finally:
    stop.set()
    worker.join()
assert counters["_settled_threads"](1) == 1
print("application thread counted; framework workqueues excluded", flush=True)
"""
    result = _run(script, str(Path(__file__).resolve()))
    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == "application thread counted; framework workqueues excluded"
    assert result.stderr == ""
