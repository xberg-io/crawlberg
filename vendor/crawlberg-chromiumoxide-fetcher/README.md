# crawlberg-chromiumoxide-fetcher

Browser download and installation support for Crawlberg's independently published
Chromium driver. This package forks `chromiumoxide_fetcher` 0.9.1; its source archive
checksum and original licenses are recorded in [UPSTREAM.md](UPSTREAM.md).

ZIP archives use ZIP 9's protected extraction, including recursive symlink target
validation. The historical `zip0` and `zip8` features are aliases for that one backend
and can be enabled together. The default features are `rustls` and `zip8`;
`native-tls` remains available. The public library name is `chromiumoxide_fetcher`.

This package requires Rust 1.88 or later. Its first independent release is 0.9.2.
The `crawlberg-chromiumoxide` 0.10 driver uses this package instead of the upstream
fetcher, changing the identity of its reexported optional fetcher types. Core
Crawlberg continues to use the registry driver 0.9 series without fetcher features.

The original downloader API is retained. This change protects extraction; it does
not add browser downloading to Core Crawlberg or change its `Page` type.
