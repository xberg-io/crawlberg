# Upstream provenance

Source: chromiumoxide_fetcher 0.9.1 from crates.io.
Archive SHA-256: `edd2cc8c9431726edce9f5ceb3ca1b711dde0f818cd6fb34808a00556ce73135` (verified against the registry index).
Upstream: <https://github.com/mattsse/chromiumoxide>.
Licenses: MIT OR Apache-2.0; both original texts are included unchanged.

The candidate keeps the downloader API and TLS behavior. Both historical ZIP
feature names select one ZIP 9 backend. Extraction delegates to its default
recursive in-folder symlink policy instead of upstream's unchecked custom walker.
ZIP 9 requires Rust 1.88; this fork declares that minimum explicitly.

The independently versioned package is `crawlberg-chromiumoxide-fetcher` 0.9.2.
The driver's 0.10 release consumes it through the existing `chromiumoxide_fetcher`
dependency alias. That changes optional fetcher type identity and raises the
standalone driver minimum to Rust 1.88. Core retains registry driver 0.9.5 with
default features disabled; it exposes no optional browser download behavior.

Publication must make this package available before the 0.10 driver. This source
candidate is not a claim that either package has been published.

The `windows-version` 0.1 constraint is intentional: its current 0.100 series
requires Rust 1.95, beyond this package's declared minimum. The driver likewise
retains `windows-registry` 0.6 for its Rust 1.88 minimum. Both manifests have been
checked with `cargo upgrade --incompatible`; the local renamed fetcher dependency
keeps its explicit version so Cargo can replace its path when publishing.
