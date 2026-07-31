//! OTLP export wiring for the crawlberg CLI (compiled under the `otel` feature).
//!
//! ~keep This lives in the CLI, not the library: libraries emit only and never
//! ~keep install a global subscriber or exporter. `init_otlp` builds a
//! ~keep TracerProvider + MeterProvider wired to an OTLP collector, registers the
//! ~keep W3C TraceContext propagator, and bridges `tracing` spans to OpenTelemetry.

use opentelemetry::KeyValue;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::{SpanExporter, WithExportConfig};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::SdkTracerProvider;
use thiserror::Error;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Configuration for [`init_otlp`].
pub struct TelemetryConfig {
    /// `service.name` resource attribute (required).
    pub service_name: String,
    /// `service.version` resource attribute (optional).
    pub service_version: Option<String>,
    /// OTLP gRPC endpoint, e.g. `"http://localhost:4317"`.
    pub otlp_endpoint: String,
    /// Additional resource attributes as `(key, value)` pairs.
    pub resource_attrs: Vec<(String, String)>,
}

/// Errors returned by [`init_otlp`].
#[derive(Debug, Error)]
pub enum InitError {
    /// Failed to build the OTLP span exporter.
    #[error("failed to build OTLP span exporter: {0}")]
    SpanExporterBuild(#[from] opentelemetry_otlp::ExporterBuildError),
    /// Failed to build the OTLP metric exporter.
    #[error("failed to build OTLP metric exporter: {0}")]
    MetricExporterBuild(opentelemetry_otlp::ExporterBuildError),
    /// Failed to initialise the `tracing` subscriber.
    #[error("failed to initialise tracing subscriber: {0}")]
    SubscriberInit(#[from] tracing_subscriber::util::TryInitError),
}

/// Returned by [`init_otlp`]; shuts down the tracer and meter providers on drop.
pub struct OtelGuard {
    tracer_provider: SdkTracerProvider,
    meter_provider: SdkMeterProvider,
}

impl Drop for OtelGuard {
    fn drop(&mut self) {
        if let Err(e) = self.tracer_provider.shutdown() {
            tracing::warn!(error = %e, "error shutting down tracer provider");
        }
        if let Err(e) = self.meter_provider.shutdown() {
            tracing::warn!(error = %e, "error shutting down meter provider");
        }
    }
}

/// Initialise a TracerProvider + MeterProvider wired to an OTLP collector,
/// register the W3C TraceContext propagator, and bridge `tracing` spans to OTel.
///
/// # Errors
///
/// Returns [`InitError`] if the OTLP exporter cannot be built or if a
/// `tracing` subscriber is already registered.
pub fn init_otlp(config: TelemetryConfig) -> Result<OtelGuard, InitError> {
    let mut resource_builder = Resource::builder().with_service_name(config.service_name);
    if let Some(version) = config.service_version {
        resource_builder = resource_builder.with_attribute(KeyValue::new("service.version", version));
    }
    for (key, value) in config.resource_attrs {
        resource_builder = resource_builder.with_attribute(KeyValue::new(key, value));
    }
    let resource = resource_builder.build();

    let span_exporter = SpanExporter::builder()
        .with_tonic()
        .with_endpoint(&config.otlp_endpoint)
        .build()?;
    let tracer_provider = SdkTracerProvider::builder()
        .with_batch_exporter(span_exporter)
        .with_resource(resource.clone())
        .build();
    let tracer = tracer_provider.tracer("crawlberg");
    opentelemetry::global::set_tracer_provider(tracer_provider.clone());

    let metric_exporter = opentelemetry_otlp::MetricExporter::builder()
        .with_tonic()
        .with_endpoint(&config.otlp_endpoint)
        .build()
        .map_err(InitError::MetricExporterBuild)?;
    let reader = PeriodicReader::builder(metric_exporter)
        .with_interval(std::time::Duration::from_secs(15))
        .build();
    let meter_provider = SdkMeterProvider::builder()
        .with_reader(reader)
        .with_resource(resource)
        .build();
    opentelemetry::global::set_meter_provider(meter_provider.clone());

    opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());

    let fmt_layer = tracing_subscriber::fmt::layer().json();
    let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);
    tracing_subscriber::registry()
        .with(EnvFilter::from_default_env())
        .with(fmt_layer)
        .with(otel_layer)
        .try_init()?;

    Ok(OtelGuard {
        tracer_provider,
        meter_provider,
    })
}
