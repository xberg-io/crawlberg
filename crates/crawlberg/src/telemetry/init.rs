//! W3C TraceContext propagation bridges.
//!
//! ~keep `with_traceparent` and `current_traceparent` are always compiled for
//! ~keep Rust callers. They are not exposed by the generated language bindings
//! ~keep because `with_traceparent` requires a Rust callback.
//!
//! The library is emit-only: it installs no subscriber or exporter. OTLP export
//! wiring lives in the CLI (`crawlberg-cli`) and in downstream services, which own
//! their own `TracerProvider` / `MeterProvider`.

use std::collections::HashMap;

use opentelemetry::propagation::{Extractor, Injector};

struct SingleHeaderMap(HashMap<String, String>);

impl Extractor for SingleHeaderMap {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(String::as_str).collect()
    }
}

impl Injector for SingleHeaderMap {
    fn set(&mut self, key: &str, value: String) {
        self.0.insert(key.to_owned(), value);
    }
}

/// Extract a W3C TraceContext from a `traceparent` header string and execute
/// `f` with that context as the active OpenTelemetry context.
///
/// This lets Rust applications pass a parent span context into crawlberg
/// calls. The Rust span created inside `f` will be a child of the supplied
/// parent span in the collector.
///
/// If `traceparent` is invalid or empty, or if no propagator has been
/// registered, the call behaves identically to calling `f()` directly —
/// no panic, no error.
///
/// ~keep The registered propagator is whatever the consuming application
/// ~keep installed via `opentelemetry::global::set_text_map_propagator` (the CLI's
/// ~keep `otel` path registers a W3C `TraceContextPropagator`).
pub fn with_traceparent<F, R>(traceparent: &str, f: F) -> R
where
    F: FnOnce() -> R,
{
    let mut carrier = SingleHeaderMap(HashMap::new());
    carrier.0.insert("traceparent".to_owned(), traceparent.to_owned());
    let parent_cx = opentelemetry::global::get_text_map_propagator(|p| p.extract(&carrier));
    let _guard = opentelemetry::Context::attach(parent_cx);
    f()
}

/// Encode the active OpenTelemetry context as a W3C `traceparent` header value.
///
/// Returns `None` when there is no active remote span context (i.e. no span
/// is in-flight or the span is not sampled), or when no propagator has been
/// registered.
///
/// Use this in Rust code to hand the current crawlberg trace context to
/// downstream services.
pub fn current_traceparent() -> Option<String> {
    let cx = opentelemetry::Context::current();
    let mut carrier = SingleHeaderMap(HashMap::new());
    opentelemetry::global::get_text_map_propagator(|p| p.inject_context(&cx, &mut carrier));
    carrier.0.remove("traceparent")
}
