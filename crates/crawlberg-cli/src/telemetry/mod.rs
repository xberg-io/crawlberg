//! CLI-owned tracing subscriber installation.
//!
//! ~keep Libraries in the crawlberg workspace emit only; installing the global
//! ~keep subscriber is the application's job, and for the CLI that is here.
//! Console diagnostics go to stderr so they never collide with data on stdout.
//! With the `otel` feature compiled and `OTEL_EXPORTER_OTLP_ENDPOINT` set, spans
//! and metrics also export via OTLP.

pub mod logging;
#[cfg(feature = "otel")]
pub mod otlp;

pub use logging::{LogConfig, LogFormat};

/// RAII guard for the installed telemetry pipeline.
///
/// ~keep Holds the OTLP tracer/meter providers (when active) so they flush and
/// ~keep shut down on drop; keep it alive for the whole process.
#[derive(Default)]
pub struct TelemetryGuard {
    #[cfg(feature = "otel")]
    _otel: Option<otlp::OtelGuard>,
}

/// Install the process-wide `tracing` subscriber.
///
/// With the `otel` feature compiled and `OTEL_EXPORTER_OTLP_ENDPOINT` set to a
/// non-empty value, spans/metrics export to that OTLP collector; otherwise
/// diagnostics are written to stderr per `config`. A failed OTLP init falls back
/// to console logging rather than aborting.
pub fn init(config: &LogConfig) -> TelemetryGuard {
    #[cfg(feature = "otel")]
    {
        if let Ok(endpoint) = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
            && !endpoint.is_empty()
        {
            match otlp::init_otlp(otlp::TelemetryConfig {
                service_name: "crawlberg".to_owned(),
                service_version: Some(env!("CARGO_PKG_VERSION").to_owned()),
                otlp_endpoint: endpoint,
                resource_attrs: Vec::new(),
            }) {
                Ok(guard) => return TelemetryGuard { _otel: Some(guard) },
                Err(error) => {
                    let _ = logging::try_init(config);
                    tracing::warn!(%error, "OTLP telemetry init failed; falling back to console logging");
                    return TelemetryGuard::default();
                }
            }
        }
    }
    let _ = logging::try_init(config);
    TelemetryGuard::default()
}
