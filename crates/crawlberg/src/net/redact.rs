//! Credential redaction for values that may reach `tracing` fields or error `Display`
//! output.
//!
//! Secrets this crate handles — proxy credentials embedded in a URL's userinfo, API
//! keys, auth tokens — must never appear verbatim in a span field or an error message,
//! since both are routinely shipped to logs, OTLP collectors, and issue trackers. This
//! module centralizes that redaction so every call site applies the same rule.

pub(crate) const REDACTED_PLACEHOLDER: &str = "***";

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

/// Redact userinfo and the whole query string from a URL-like string.
///
/// For an endpoint that authenticates through its query, such as a CDP WebSocket URL with
/// a `?token=` parameter or a bypass vendor endpoint. Returns `input` unchanged when it does
/// not parse as an absolute URL or carries neither.
#[must_use]
pub fn redact_url_secrets(input: &str) -> String {
    let redacted = redact_url_credentials(input);
    let Ok(mut url) = url::Url::parse(&redacted) else {
        return redacted;
    };
    if url.query().is_none() {
        return redacted;
    }
    url.set_query(Some(REDACTED_PLACEHOLDER));
    url.to_string()
}

/// Redact `user[:password]@` userinfo from a URL-like string **without parsing it**.
///
/// [`redact_url_credentials`] returns its input unchanged when `url::Url::parse` fails, so it
/// is a no-op in exactly the place an "invalid URL" error message is assembled — the value is
/// unparseable, which is why the error exists. This locates the authority's userinfo span
/// textually instead, so a malformed URL still has its credentials removed.
///
/// The span examined is bounded by the authority: the text after `//` (or the start of the
/// input when there is no `//`) up to the first `/`, `?` or `#`. Only the last `@` inside that
/// span terminates the userinfo, matching how a URL parser reads an authority, so an `@` in a
/// path, query or fragment is never mistaken for one.
#[must_use]
pub(crate) fn redact_userinfo_textually(input: &str) -> String {
    let authority_start = input.find("//").map_or(0, |index| index + 2);
    let authority = &input[authority_start..];
    let authority_end = authority.find(['/', '?', '#']).unwrap_or(authority.len());
    let Some(at) = authority[..authority_end].rfind('@') else {
        return input.to_owned();
    };
    format!(
        "{}{REDACTED_PLACEHOLDER}{}",
        &input[..authority_start],
        &input[authority_start + at..]
    )
}

/// `Debug` view of a string map that shows each key and hides each value, for maps of
/// header values or cookie values set by the caller.
pub(crate) struct RedactedValues<'a>(pub(crate) &'a std::collections::HashMap<String, String>);

impl std::fmt::Debug for RedactedValues<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map()
            .entries(self.0.keys().map(|key| (key, REDACTED_PLACEHOLDER)))
            .finish()
    }
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
    fn redact_url_secrets_hides_userinfo_and_query() {
        assert_eq!(
            redact_url_secrets("wss://user:pw@chrome.example:3000/devtools?token=abc123&x=1"),
            "wss://***:***@chrome.example:3000/devtools?***"
        );
        assert_eq!(
            redact_url_secrets("ws://127.0.0.1:9222/devtools/browser/42"),
            "ws://127.0.0.1:9222/devtools/browser/42"
        );
    }

    #[test]
    fn redacted_values_shows_keys_only() {
        let map = std::collections::HashMap::from([("Authorization".to_owned(), "Bearer abc123".to_owned())]);
        assert_eq!(format!("{:?}", RedactedValues(&map)), r#"{"Authorization": "***"}"#);
    }

    #[test]
    fn redacts_userinfo_of_a_url_that_does_not_parse() {
        // ~keep A space in the host makes this unparseable, so `redact_url_credentials` is a
        // ~keep no-op on it; the textual strip is the only thing that removes the password.
        let malformed = "http://user:hunter2@proxy internal:8080";
        assert_eq!(
            redact_url_credentials(malformed),
            malformed,
            "the parsing helper is expected to pass an unparseable URL through unchanged"
        );
        assert_eq!(redact_userinfo_textually(malformed), "http://***@proxy internal:8080");
    }

    #[test]
    fn textual_userinfo_strip_ignores_an_at_sign_outside_the_authority() {
        assert_eq!(
            redact_userinfo_textually("http://example.com/mail/user@host?to=a@b#f@g"),
            "http://example.com/mail/user@host?to=a@b#f@g"
        );
    }

    #[test]
    fn textual_userinfo_strip_handles_a_missing_scheme_and_a_plain_string() {
        assert_eq!(redact_userinfo_textually("user:hunter2@proxy:8080"), "***@proxy:8080");
        assert_eq!(redact_userinfo_textually("not a url at all"), "not a url at all");
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
