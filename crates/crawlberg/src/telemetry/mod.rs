//! OpenTelemetry foundations for crawlberg (emit-only).
//!
//! Sub-modules:
//!
//! - [`attributes`] — re-exports upstream semconv constants plus `crawl.*`
//!   extension keys.
//! - [`metrics`] — process-wide [`MetricRegistry`] singleton (Cluster 2
//!   populates it with instrument handles).
//! - [`init`] — `with_traceparent` / `current_traceparent` W3C propagation
//!   bridges (always compiled).
//!
//! ~keep The library never installs a global subscriber or OTLP exporter — that
//! ~keep belongs to the CLI (`crawlberg-cli`) and downstream services. Spans,
//! ~keep semconv attributes, and metric instruments are emitted always-on and
//! ~keep flow into whatever exporter the consumer installs.

pub mod attributes;
pub mod init;
pub mod metrics;

pub use init::{current_traceparent, with_traceparent};
pub use metrics::{MetricRegistry, registry};
