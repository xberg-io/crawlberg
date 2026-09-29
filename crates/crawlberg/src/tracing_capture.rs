//! Test-only capture of `tracing` event fields, for asserting what a log line carries
//! without adding a `tracing-subscriber` dev-dependency.

use std::sync::{Arc, Mutex};

/// Records every field name/value pair, formatted with `Debug` (which is how tracing
/// dispatches both `%value` and plain `Display`/`Debug` fields).
struct FieldVisitor<'a>(&'a mut Vec<(String, String)>);

impl tracing::field::Visit for FieldVisitor<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.push((field.name().to_owned(), format!("{value:?}")));
    }
}

/// Captures every event's fields into `sink`.
struct CapturingSubscriber {
    sink: Arc<Mutex<Vec<(String, String)>>>,
}

impl tracing::Subscriber for CapturingSubscriber {
    fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _attrs: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut fields = self.sink.lock().expect("sink mutex must not be poisoned");
        event.record(&mut FieldVisitor(&mut fields));
    }
    fn enter(&self, _span: &tracing::span::Id) {}
    fn exit(&self, _span: &tracing::span::Id) {}
}

/// Run `f` with a thread-local capturing subscriber and return its result together with
/// every event field it logged.
// ~keep Serialize the caller with every test that reaches the same callsite without a
// ~keep subscriber. While this subscriber is the only live dispatcher, a first hit on
// ~keep another thread caches that callsite's interest from that thread's default (none),
// ~keep so the event here is filtered out and the capture comes back empty.
pub(crate) fn capture_events<R>(f: impl FnOnce() -> R) -> (R, Vec<(String, String)>) {
    let sink = Arc::new(Mutex::new(Vec::new()));
    let result = {
        let _guard = tracing::subscriber::set_default(CapturingSubscriber { sink: sink.clone() });
        f()
    };
    let fields = std::mem::take(&mut *sink.lock().expect("sink mutex must not be poisoned"));
    (result, fields)
}

/// Assert that `fields` holds at least one event, that no field contains `secret`, and
/// that some field contains `expected`. The last check keeps the absence check from
/// passing on an empty or unrelated capture.
pub(crate) fn assert_logged_without_secret(fields: &[(String, String)], secret: &str, expected: &str) {
    assert!(
        !fields.is_empty(),
        "expected the debug log to fire, got no recorded events"
    );
    let leaking: Vec<&(String, String)> = fields.iter().filter(|(_, v)| v.contains(secret)).collect();
    assert!(
        leaking.is_empty(),
        "no log field may contain the raw secret '{secret}', but found: {leaking:?}"
    );
    assert!(
        fields.iter().any(|(_, v)| v.contains(expected)),
        "expected a log field to contain '{expected}', got {fields:?}"
    );
}
