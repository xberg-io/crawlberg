//! Pluggable `tracing`-based console logging.
//!
//! This module wires a [`tracing_subscriber::fmt`] layer to **stderr** so
//! diagnostics never collide with a program's data output on stdout. It is
//! deliberately composable: [`layer`] returns a boxed [`tracing_subscriber::Layer`]
//! that a host application can add to its own `registry()` alongside other
//! layers (e.g. an OTel bridge), and [`try_init`] is a convenience wrapper for
//! the common case of "just install a global subscriber for stderr logging".
//!
//! # Level semantics
//!
//! ~keep crawlberg and its consumers follow these conventions when choosing a
//! ~keep `tracing` level:
//!
//! - `error`: an operation failed and the user needs to know (a scrape/crawl
//!   errored, invalid config, server crash).
//! - `warn`: degraded but recoverable (retry, soft-block fallback, tier
//!   escalation, missing-but-optional data).
//! - `info`: high-level lifecycle/progress (server starting, crawl
//!   started/finished).
//! - `debug`: per-request / per-page detail.
//! - `trace`: verbose wire-level.

use std::io::IsTerminal;

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

/// Output format for the console logging layer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LogFormat {
    /// Multi-line, human-friendly output (`tracing_subscriber::fmt::layer().pretty()`).
    #[default]
    Pretty,
    /// Single-line, human-friendly output (`tracing_subscriber::fmt::layer().compact()`).
    Compact,
    /// Newline-delimited JSON output (`tracing_subscriber::fmt::layer().json()`).
    Json,
}

/// Configuration for the console logging layer built by [`layer`] / [`try_init`].
#[derive(Clone, Debug)]
pub struct LogConfig {
    /// Default `EnvFilter` directive string applied when no env var overrides it,
    /// e.g. "warn" or "warn,crawlberg_cli=info".
    pub directives: String,
    /// Output format.
    pub format: LogFormat,
    /// None = auto-detect (ANSI only when stderr is a terminal).
    pub ansi: Option<bool>,
    /// Honor `CRAWLBERG_LOG` (preferred) then `RUST_LOG` when set. Default true.
    pub env_override: bool,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            directives: "warn".to_owned(),
            format: LogFormat::Pretty,
            ansi: None,
            env_override: true,
        }
    }
}

/// Build an [`EnvFilter`] for `config`.
///
/// When `config.env_override` is set, `CRAWLBERG_LOG` is preferred over
/// `RUST_LOG`; a malformed value in either falls back to `config.directives`
/// rather than panicking.
pub fn build_env_filter(config: &LogConfig) -> EnvFilter {
    if config.env_override {
        if let Ok(value) = std::env::var("CRAWLBERG_LOG")
            && let Ok(filter) = EnvFilter::try_new(&value)
        {
            return filter;
        }
        if let Ok(value) = std::env::var("RUST_LOG")
            && let Ok(filter) = EnvFilter::try_new(&value)
        {
            return filter;
        }
    }
    EnvFilter::new(&config.directives)
}

/// Build a boxed console logging [`Layer`] writing to stderr, formatted and
/// filtered per `config`.
///
/// This is the composable/pluggable path: consumers add the returned layer to
/// their own `tracing_subscriber::registry()` rather than calling [`try_init`]
/// directly, letting them combine it with other layers (e.g. an OTel bridge).
pub fn layer<S>(config: &LogConfig) -> Box<dyn Layer<S> + Send + Sync>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    let ansi = config.ansi.unwrap_or_else(|| std::io::stderr().is_terminal());

    match config.format {
        LogFormat::Json => Box::new(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_ansi(ansi)
                .json()
                .with_filter(build_env_filter(config)),
        ),
        LogFormat::Compact => Box::new(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_ansi(ansi)
                .compact()
                .with_filter(build_env_filter(config)),
        ),
        LogFormat::Pretty => Box::new(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_ansi(ansi)
                .pretty()
                .with_filter(build_env_filter(config)),
        ),
    }
}

/// Install a global `tracing` subscriber built from [`layer`].
///
/// Returns `Err` when a global subscriber is already installed (e.g. the `otel`
/// OTLP path already ran, or a binding embedder installed its own subscriber).
/// That is expected and non-fatal: the existing subscriber wins and callers
/// should ignore the error.
///
/// # Errors
///
/// Returns [`tracing_subscriber::util::TryInitError`] if a global subscriber
/// is already installed.
pub fn try_init(config: &LogConfig) -> Result<(), tracing_subscriber::util::TryInitError> {
    tracing_subscriber::registry().with(layer(config)).try_init()
}
