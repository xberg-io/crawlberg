//! OpenTelemetry foundations for crawlberg.
//!
//! Three sub-modules:
//!
//! - [`attributes`] — re-exports upstream semconv constants plus `crawl.*`
//!   extension keys.
//! - [`metrics`] — process-wide [`MetricRegistry`] singleton (Cluster 2
//!   populates it with instrument handles).
//! - [`init`] — `with_traceparent` / `current_traceparent` (always compiled)
//!   and the optional `init_otlp` helper (requires `telemetry-init` feature).
//! - [`logging`] — pluggable `tracing`-based console logging to stderr
//!   (requires `logging` feature).

pub mod attributes;
pub mod init;
#[cfg(feature = "logging")]
pub mod logging;
pub mod metrics;

pub use init::{current_traceparent, with_traceparent};
pub use metrics::{MetricRegistry, registry};

#[cfg(feature = "telemetry-init")]
pub use init::{InitError, TelemetryConfig, TelemetryGuard, init_otlp};

#[cfg(feature = "logging")]
pub use logging::{LogConfig, LogFormat, build_env_filter, layer, try_init};
