//! Regression tests proving the B3 fix: `AttemptOutcome` is owned and `Send`.

use std::sync::Arc;

use crawlberg::{AttemptOutcome, Tier};

/// Verify `AttemptOutcome` can be moved into a spawned task.
///
/// Under the old `<'a>` shape this would not compile because `tokio::spawn`
/// requires `'static` bounds and the struct borrowed external data.
#[tokio::test]
async fn attempt_outcome_is_send_for_spawning() {
    let outcome = AttemptOutcome {
        attempt: 0,
        url: Arc::from("https://example.com/"),
        status: Some(200),
        error: None,
        waf_signal: None,
        body_size: 100,
        content_density: 0.5,
        bytes_transferred: Some(100),
        previous_tier: Tier::Http,
    };

    let handle = tokio::spawn(async move {
        assert_eq!(outcome.attempt, 0);
        outcome
    });
    let back = handle.await.unwrap();
    assert_eq!(back.attempt, 0);
}

/// Verify `AttemptOutcome` is `Clone` — required for policies that fan out
/// to multiple state backends.
#[test]
fn attempt_outcome_is_clone() {
    let outcome = AttemptOutcome {
        attempt: 1,
        url: Arc::from("https://clone.example.com/"),
        status: Some(404),
        error: None,
        waf_signal: None,
        body_size: 0,
        content_density: 0.0,
        bytes_transferred: None,
        previous_tier: Tier::Bypass,
    };
    let cloned = outcome.clone();
    assert_eq!(cloned.attempt, 1);
    assert_eq!(&*cloned.url, "https://clone.example.com/");
    assert_eq!(cloned.previous_tier, Tier::Bypass);
}
