"""Any thread can release the last reference to an engine.

An async result is delivered on a runtime thread. When that thread releases the last reference to
the asyncio future, Python frees the objects the future kept alive, and the engine can be one of
them. A thread-bound engine class then prints "is unsendable, but is being dropped on another
thread" and the engine is never freed.

The test needs no race. A child process creates an engine in one thread and drops the last
reference in another thread. No line about an unsendable class may reach stderr.
"""

from __future__ import annotations

import subprocess
import sys

CHILD = r"""
import gc, threading

import crawlberg

config = crawlberg.CrawlConfig(browser=crawlberg.BrowserConfig(mode="never"))
holder = []


def create():
    holder.append(crawlberg.create_engine(config))


def release():
    holder.clear()
    gc.collect()


for step in (create, release):
    thread = threading.Thread(target=step)
    thread.start()
    thread.join()

print("released", flush=True)
"""


def test_an_engine_created_in_one_thread_is_released_in_another_without_an_error() -> None:
    """The child releases the engine in a second thread and prints no line about an unsendable class."""
    proc = subprocess.run(
        [sys.executable, "-c", CHILD],
        capture_output=True,
        text=True,
        timeout=120,
        check=False,
    )

    # The child ran both steps: a child that fails early cannot show the defect.
    assert proc.stdout.strip() == "released", proc.stderr
    assert "unsendable" not in proc.stderr, proc.stderr
