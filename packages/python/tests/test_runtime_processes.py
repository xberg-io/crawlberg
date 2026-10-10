import subprocess
import sys
import threading
from concurrent.futures import ThreadPoolExecutor

import pytest
from typing_extensions import override

from tests.test_async_runtime import _Handler, _Server
from tests.test_async_runtime import local_server as _local_server_fixture

local_server = _local_server_fixture

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
import subprocess
import sys
from pathlib import Path

def threads():
    if sys.platform == "linux":
        return len(list(Path("/proc/self/task").iterdir()))
    total = len(subprocess.check_output(["/bin/ps", "-M", "-p", str(os.getpid())]).splitlines()) - 1
    # ~keep: macOS framework workqueues outlive Tokio; exclude only the OS-reported workqueue threads.
    proc_pidworkqueueinfo = 12
    workqueue_info = (ctypes.c_uint32 * 4)()
    libproc = ctypes.CDLL("/usr/lib/libproc.dylib")
    libproc.proc_pidinfo.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_uint64, ctypes.c_void_p, ctypes.c_int]
    libproc.proc_pidinfo.restype = ctypes.c_int
    size = ctypes.sizeof(workqueue_info)
    assert libproc.proc_pidinfo(os.getpid(), proc_pidworkqueueinfo, 0, ctypes.byref(workqueue_info), size) == size
    return total - workqueue_info[0]

def after_runtime_close():
    assert threads() == 1
    async def rejected():
        try:
            await crawlberg.scrape(engine(), sys.argv[1])
        except RuntimeError as error:
            assert "interpreter exit" in str(error), str(error)
        else:
            raise AssertionError("runtime restarted during interpreter exit")
    asyncio.run(rejected())
    assert threads() == 1
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
