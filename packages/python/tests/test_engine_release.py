"""A runtime thread can release the last reference to an engine.

An async result is delivered on a runtime thread. When that thread releases the last reference to
the asyncio future, Python frees the objects the future kept alive, and the engine can be one of
them. A thread-bound engine class then prints "is unsendable, but is being dropped on another
thread" and the engine is never freed.

Each child process crawls a local server with two engines and exits. No child may print that line.
The line needs a race, so the children run 16 at a time on 2 cores. Measured with a thread-bound
class: 70 of 400 children print it. With 160 children the chance that none prints it is below 1 in
10,000, also at a third of that rate.
"""

from __future__ import annotations

import os
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor

CHILDREN = 160
AT_A_TIME = 16
CORES = 2

CHILD = r"""
import asyncio, json, os, sys, threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

if sys.argv[1]:
    os.sched_setaffinity(0, {int(cpu) for cpu in sys.argv[1].split(",")})

import crawlberg

PAGES = {
    "/": '<h1>Home</h1><p><a href="/docs">the docs</a></p><p><a href="/moved">a moved page</a></p>',
    "/new-place": "<h1>New place</h1>",
    "/docs/": '<h1>Docs index</h1><p><a href="guide">the guide</a></p>',
    "/docs/guide": "<h1>Guide</h1>",
}
REDIRECTS = {"/docs": "/docs/", "/moved": "/new-place"}


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        if self.path in REDIRECTS:
            self.send_response(301)
            self.send_header("Location", REDIRECTS[self.path])
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        text = PAGES.get(self.path, "<h1>Not found</h1>")
        body = f"<!doctype html><html><head><title>t</title></head><body>{text}</body></html>".encode()
        self.send_response(200 if self.path in PAGES else 404)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
threading.Thread(target=server.serve_forever, daemon=True).start()
base = f"http://127.0.0.1:{server.server_address[1]}"
config = crawlberg.CrawlConfig(
    ssrf=crawlberg.SsrfPolicy(deny_private=True, allowlist=[crawlberg.HostMatcher.cidr("127.0.0.0/8")]),
    browser=crawlberg.BrowserConfig(mode="never"),
    content=crawlberg.ContentConfig(extract_metadata=False),
    max_depth=2,
    stay_on_domain=True,
)


async def work():
    pages = 0
    for seed in ("/", "/docs"):
        engine = crawlberg.create_engine(config)
        async for raw in crawlberg.crawl_stream(engine, base + seed):
            if json.loads(str(raw))["type"] == "page":
                pages += 1
    return pages


print("pages", asyncio.run(work()), flush=True)
"""


def two_cores() -> str:
    """The cores the children share, or an empty string where the platform cannot pin a process."""
    if not hasattr(os, "sched_getaffinity"):
        return ""
    allowed = sorted(os.sched_getaffinity(0))
    return ",".join(str(cpu) for cpu in allowed[-CORES:])


def run_child(cores: str) -> tuple[str, str]:
    """Run one child process and return what it wrote to stdout and to stderr."""
    proc = subprocess.run(
        [sys.executable, "-c", CHILD, cores],
        capture_output=True,
        text=True,
        timeout=120,
        check=False,
    )
    return proc.stdout, proc.stderr


def test_an_engine_released_on_a_runtime_thread_is_freed_without_an_error() -> None:
    """No child prints the line that a thread-bound engine class gives."""
    cores = two_cores()
    with ThreadPoolExecutor(max_workers=AT_A_TIME) as pool:
        results = list(pool.map(run_child, [cores] * CHILDREN))

    # Every child did its crawls: a child that fails early cannot show the defect.
    crawled = [out for out, _ in results if out.startswith("pages ") and int(out.split()[1]) > 0]
    assert len(crawled) == CHILDREN, results[0]

    refused = [err for _, err in results if "unsendable" in err]
    assert len(refused) == 0, f"{len(refused)} of {CHILDREN} children: {refused[:1]}"
