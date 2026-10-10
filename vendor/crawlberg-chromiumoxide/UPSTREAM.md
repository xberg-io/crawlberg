# Upstream provenance

This crate is forked from `chromiumoxide` 0.9.1, downloaded from crates.io.

- Upstream repository: <https://github.com/mattsse/chromiumoxide>
- crates.io package checksum: `26ed067eb6c1f660bdb87c05efb964421d2ca262bae0296cdfe38cf0cd949a3e`
- Upstream licenses: MIT OR Apache-2.0; the unmodified license texts are included.

The fork keeps the public API compatible and adds an opt-out for chromiumoxide's
automatic management and resumption of child targets. Crawlberg uses that opt-out
so its security controller is the sole owner of paused child-target sessions.

The fork also reads a protocol message that holds one half of a surrogate pair. Chrome
writes that half as a lone `\ud83d` escape, which `serde_json` refuses, so upstream loses
the whole reply or event. `conn::parse_message` replaces each unpaired half with U+FFFD.
