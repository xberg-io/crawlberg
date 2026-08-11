//! Monotonic-clock alias so callers never reach `std::time::Instant` directly.
//!
//! ~keep `std::time::Instant::now()` compiles on `wasm32-unknown-unknown` but its
//! `unsupported` backend traps with `unreachable` at runtime (no clock source on that
//! target). `web_time::Instant` is API-compatible and backed by `performance.now()` on
//! wasm, so every call site imports `Instant` from here instead of `std::time` and gets
//! a working clock on every target without a per-callsite `cfg`.

#[cfg(target_arch = "wasm32")]
pub(crate) use web_time::Instant;

#[cfg(not(target_arch = "wasm32"))]
pub(crate) use std::time::Instant;
