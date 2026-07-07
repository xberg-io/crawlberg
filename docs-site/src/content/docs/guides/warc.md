---
title: "WARC Output"
---

Crawlberg can write crawled content to [WARC 1.1](https://iipc.github.io/warc-specifications/specifications/warc-format/warc-1.1/)
(Web ARChive) files for long-term archival and reproducibility. The WARC feature is gated
behind the `warc` Cargo feature.

## WARC 1.1 format overview

WARC is the standard format used by the Internet Archive and web preservation institutions.
Each WARC file contains a sequence of records, where each record has:

1. A version line (`WARC/1.1`)
2. Named-field headers (record type, date, record ID, content type, content length)
3. A payload block (the actual HTTP response)
4. A double-CRLF terminator

Crawlberg writes two record types:

- **warcinfo** -- A single metadata record at the start of the file, identifying the
  software and hostname that produced the archive.
- **response** -- One record per crawled page, containing the full HTTP response
  (status line, headers, and body) as the payload.

Record IDs use the `<urn:uuid:...>` format. Timestamps follow ISO 8601 with second
precision and a `Z` suffix.

## Enabling WARC output

Set the `warc_output` field on `CrawlConfig` to write WARC during a crawl:

```rust
use std::path::PathBuf;
use crawlberg::CrawlConfig;

let config = CrawlConfig {
    warc_output: Some(PathBuf::from("crawl-2026-04-09.warc")),
    ..Default::default()
};
```

When `warc_output` is set, the crawl writes a single `warcinfo` record at the start of the file (identifying `crawlberg/<version>` and the host) followed by one `response` record per successfully fetched page, containing the full HTTP response — status line, headers, and body — as the payload. The file is flushed and closed when the crawl completes. Header names and values are validated against CR/LF injection before being written.

## CLI flag

From the command line, pass `--warc-output` to any crawl command:

```bash
crawlberg crawl https://example.com --depth 2 --warc-output archive.warc
```

The resulting file is a valid WARC 1.1 archive that can be processed by standard tools
such as [warcio](https://github.com/webrecorder/warcio), [pywb](https://github.com/webrecorder/pywb),
and the [Wayback Machine](https://web.archive.org/).

## HTTP response encoding

Each WARC response record wraps the full HTTP response block:

```text
HTTP/1.1 200 OK\r\n
Content-Type: text/html\r\n
\r\n
<html>...</html>
```

Crawlberg maps common status codes to their standard reason phrases (200 OK, 404 Not Found,
etc.) and uses `"Unknown"` for unrecognized codes. The response block is stored as raw bytes
in the WARC payload, preserving the original content encoding.

:::tip[File size]
WARC files can grow large for deep crawls. Consider post-processing with gzip
compression (`.warc.gz`) using external tools. Crawlberg does not compress WARC
output natively.
:::
