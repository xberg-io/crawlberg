from __future__ import annotations

import os
import signal
import subprocess
import sys
import unittest

CHILD = r"""
import asyncio
import os
import sys
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer

import crawlberg
from crawlberg import _crawlberg

class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        payload = b'<html><body><p>Runtime lifecycle text.</p></body></html>'
        self.send_response(200)
        self.send_header('Content-Type', 'text/html')
        self.send_header('Content-Length', str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, *args):
        pass

server = HTTPServer(('127.0.0.1', 0), Handler)
threading.Thread(target=server.serve_forever, daemon=True).start()
url = f'http://127.0.0.1:{server.server_port}/'
engine = crawlberg.create_engine(crawlberg.CrawlConfig(
    browser=crawlberg.BrowserConfig(mode='never'),
    ssrf=crawlberg.SsrfPolicy(deny_private=False),
))
result = asyncio.run(crawlberg.scrape(engine, url))
assert result.status_code == 200, result.status_code
assert result.markdown is not None
assert result.markdown.content == 'Runtime lifecycle text.\n', result.markdown.content
assert hasattr(_crawlberg, 'shutdown_async_runtime'), 'missing managed runtime shutdown boundary'
print('real_async_scrape', flush=True)
if sys.argv[1] == 'fork':
    pid = os.fork()
    if pid == 0:
        try:
            asyncio.run(crawlberg.scrape(engine, url))
        except RuntimeError as error:
            if 'async runtime cannot be used after fork once initialized in the parent' in str(error):
                os.write(1, b'fork_guard\n')
                os._exit(0)
            os.write(2, str(error).encode())
            os._exit(2)
        os._exit(3)
    _, status = os.waitpid(pid, 0)
    assert os.waitstatus_to_exitcode(status) == 0, status
print('parent_exit', flush=True)
"""


def _run_native_runtime_child(mode: str, expected: str) -> None:
    process = subprocess.Popen(
        [sys.executable, "-c", CHILD, mode],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=os.name == "posix",
    )
    try:
        stdout, stderr = process.communicate(timeout=30)
    except subprocess.TimeoutExpired as error:
        if os.name == "posix":
            os.killpg(process.pid, signal.SIGKILL)
        else:
            process.kill()
        stdout, stderr = process.communicate()
        raise AssertionError(f"{mode} did not complete within30s: {stdout!r} {stderr!r}") from error
    assert process.returncode == 0, (mode, stdout, stderr)
    assert stdout == expected, (mode, stdout, stderr)
    assert "unsendable" not in stderr, stderr


def test_real_async_scrape_exits_with_the_managed_runtime() -> None:
    _run_native_runtime_child("exit", "real_async_scrape\nparent_exit\n")


@unittest.skipUnless(hasattr(os, "fork"), "requires POSIX fork")
def test_initialized_native_runtime_rejects_a_forked_child() -> None:
    _run_native_runtime_child("fork", "real_async_scrape\nfork_guard\nparent_exit\n")
