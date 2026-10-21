import asyncio
import sys
import threading
from collections.abc import Iterator
from http.server import BaseHTTPRequestHandler, HTTPServer

if sys.version_info >= (3, 12):
    from typing import override
else:
    from typing_extensions import override

import pytest

import crawlberg


_BODY = b"<html><body><h1>Checking your browser</h1></body></html>"


class _Handler(BaseHTTPRequestHandler):
    def do_GET(self) -> None:
        linked_seed = self.path == "/links"
        self.send_response(200 if linked_seed else 403)
        self.send_header("Content-Type", "text/html")
        if not linked_seed:
            self.send_header("cf-mitigated", "challenge")
            self.send_header("Server", "cloudflare")
        self.end_headers()
        index = (
            b"<html><body><h1>Local index</h1>"
            b"<p>This ordinary page links to a protected document and remains readable when that request fails.</p>"
            b'<a href="/blocked">Blocked link</a></body></html>'
        )
        self.wfile.write(index if linked_seed else _BODY)

    @override
    def log_message(self, format: str, *args: object) -> None:  # noqa: A002 ~keep Preserve the stdlib signature.
        del format, args


@pytest.fixture
def blocked_url() -> Iterator[str]:
    server = HTTPServer(("127.0.0.1", 0), _Handler)
    worker = threading.Thread(target=server.serve_forever)
    worker.start()
    try:
        yield f"http://127.0.0.1:{server.server_port}/"
    finally:
        server.shutdown()
        server.server_close()
        worker.join()


def test_should_preserve_waf_details_in_scrape_exception_and_crawl_result(blocked_url: str) -> None:
    async def verify() -> None:
        config = crawlberg.CrawlConfig(
            browser=crawlberg.BrowserConfig(mode=crawlberg.BrowserMode.NEVER),
            ssrf=crawlberg.SsrfPolicy(deny_private=False),
            respect_robots_txt=False,
            retry_count=0,
            rate_limit_ms=0,
        )
        engine = crawlberg.create_engine(config)
        with pytest.raises(crawlberg.WafBlockedError) as caught:
            await crawlberg.scrape(engine, blocked_url)
        assert caught.value.vendor == "cloudflare"
        assert caught.value.status == 403
        assert caught.value.body == _BODY.decode()
        assert caught.value.args == (str(caught.value),)
        result = await crawlberg.crawl(engine, blocked_url)
        assert result.pages == []
        assert result.error == str(caught.value)
        assert result.waf_block is not None
        assert result.waf_block.vendor == "cloudflare"
        assert result.waf_block.status == 403
        assert result.waf_block.body_prefix == _BODY.decode()
        linked = await crawlberg.crawl(engine, blocked_url + "links")
        assert len(linked.pages) == 1
        assert linked.error is None
        assert linked.waf_block is not None
        assert linked.waf_block.vendor == "cloudflare"
        assert linked.waf_block.status == 403
        assert linked.waf_block.body_prefix == _BODY.decode()

    asyncio.run(verify())
