//! Credential redaction for values that may reach `tracing` fields or error `Display`
//! output.
//!
//! Secrets this crate handles — proxy credentials embedded in a URL's userinfo, API
//! keys, auth tokens — must never appear verbatim in a span field or an error message,
//! since both are routinely shipped to logs, OTLP collectors, and issue trackers. This
//! module centralizes that redaction so every call site applies the same rule.

const REDACTED_PLACEHOLDER: &str = "***";

/// Redact `user[:password]@` userinfo from a URL-like string.
///
/// Parses `input` as an absolute URL; if it carries a username and/or password, both are
/// replaced with a fixed placeholder before re-serializing, so the scheme/host/path
/// remain useful for debugging while the credential bytes never reach the output.
///
/// `input` is returned unchanged (not an error) when it does not parse as an absolute
/// URL, or parses but carries no credentials — either way there is nothing to redact.
/// This makes the function safe to call unconditionally on any string that *might* be a
/// URL, such as an error message being assembled for display.
#[must_use]
pub fn redact_url_credentials(input: &str) -> String {
    let Ok(mut url) = url::Url::parse(input) else {
        return input.to_owned();
    };
    if url.username().is_empty() && url.password().is_none() {
        return input.to_owned();
    }
    // ~keep `set_username`/`set_password` fail only for schemes without an authority
    // (data:, mailto:, ...), which by construction cannot have parsed a non-empty
    // username/password in the first place; the `Err` case is unreachable here but is
    // not worth `.expect()`-ing over, so both are ignored. Only redact a component that
    // was actually present, so a username-only URL does not grow a fabricated password.
    if !url.username().is_empty() {
        let _ = url.set_username(REDACTED_PLACEHOLDER);
    }
    if url.password().is_some() {
        let _ = url.set_password(Some(REDACTED_PLACEHOLDER));
    }
    url.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_username_and_password() {
        let redacted = redact_url_credentials("http://user:hunter2@evil.example/path");
        assert!(
            !redacted.contains("hunter2"),
            "password must not survive redaction, got '{redacted}'"
        );
        assert!(
            !redacted.contains("user:hunter2"),
            "raw userinfo must not survive redaction, got '{redacted}'"
        );
        assert_eq!(
            redacted, "http://***:***@evil.example/path",
            "unexpected redacted form: '{redacted}'"
        );
    }

    #[test]
    fn redacts_username_only() {
        let redacted = redact_url_credentials("http://apikey@example.com/");
        assert!(
            !redacted.contains("apikey"),
            "username must not survive redaction, got '{redacted}'"
        );
        assert_eq!(
            redacted, "http://***@example.com/",
            "unexpected redacted form: '{redacted}'"
        );
    }

    #[test]
    fn leaves_credential_free_url_unchanged() {
        assert_eq!(
            redact_url_credentials("https://example.com/path?query=1"),
            "https://example.com/path?query=1"
        );
    }

    #[test]
    fn leaves_non_url_input_unchanged() {
        assert_eq!(redact_url_credentials("not a url at all"), "not a url at all");
    }

    #[test]
    fn leaves_dns_failure_message_unchanged() {
        // ~keep A bare hostname (no scheme) is not parseable as an absolute URL, so it
        // passes through untouched — there is no userinfo syntax to strip here.
        assert_eq!(
            redact_url_credentials("evil.example: dns error"),
            "evil.example: dns error"
        );
    }
}
