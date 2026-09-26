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
/// `input` is returned unchanged when it parses to a URL with a host but carries no
/// credentials, since there is nothing to redact. Otherwise [`strip_unparsed_userinfo`] removes
/// a `user[:pw]@` prefix from whatever looks like the authority part of `input` directly, so
/// a value never carries a credential through unchanged just because `url::Url` could not
/// give it a host: that covers a genuinely malformed address (a stray space in the host) as
/// well as one `url::Url` parses as an opaque, hostless value (a scheme-less `user:pw@host`
/// parses as scheme `user` with no host at all, not as an error). This makes the function
/// safe to call unconditionally on any string that *might* be a URL, such as an error
/// message being assembled for display.
#[must_use]
pub fn redact_url_credentials(input: &str) -> String {
    let mut url = match url::Url::parse(input) {
        Ok(url) if url.host().is_some() => url,
        _ => return strip_unparsed_userinfo(input),
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

/// Remove a `user[:password]@` prefix from a string that failed to parse as a URL.
///
/// The authority runs from right after the first `//` (or from the start of `input` when
/// there is no `//`, since a scheme-less `user:pw@host` has no double slash to anchor on)
/// up to the next `/`, or the end of `input` when there is none. When that span contains
/// an `@`, everything up to and including it is userinfo and is removed; an `@` that only
/// appears after the authority (in the path) is left alone.
fn strip_unparsed_userinfo(input: &str) -> String {
    let authority_start = input.find("//").map_or(0, |pos| pos + 2);
    let authority = &input[authority_start..];
    let authority_end = authority.find('/').unwrap_or(authority.len());
    let Some(at_pos) = authority[..authority_end].find('@') else {
        return input.to_owned();
    };
    let mut redacted = String::with_capacity(input.len());
    redacted.push_str(&input[..authority_start]);
    redacted.push_str(&authority[at_pos + 1..]);
    redacted
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

    #[test]
    fn strips_userinfo_from_an_unparseable_url() {
        // ~keep The space in the host makes this fail `url::Url::parse`; before the
        // fallback existed this returned the input unchanged, credentials included.
        let redacted = redact_url_credentials("https://user:pw@ex ample.com/x");
        assert!(
            !redacted.contains("user:pw"),
            "credentials must not survive, got '{redacted}'"
        );
        assert_eq!(redacted, "https://ex ample.com/x");
    }

    #[test]
    fn strips_userinfo_from_a_schemeless_authority() {
        // ~keep `url::Url::parse` treats this as `Ok`, scheme `user`, opaque path
        // `pw@host` -- not an error -- so routing only on `Err` would miss it. No `//`
        // is present either, so the fallback must anchor the authority at the start of
        // the string rather than after a scheme separator that is not there.
        let redacted = redact_url_credentials("user:pw@host");
        assert!(
            !redacted.contains("user:pw"),
            "credentials must not survive, got '{redacted}'"
        );
        assert_eq!(redacted, "host");
    }

    #[test]
    fn leaves_an_at_sign_in_the_path_alone() {
        // ~keep The `@` here is past the first `/` after the authority, i.e. in the
        // path, not in userinfo position. A fallback that scanned for the first `@`
        // anywhere in the string, instead of bounding the search at the authority,
        // would wrongly delete part of the path.
        let redacted = redact_url_credentials("https://ex ample.com/a@b");
        assert_eq!(redacted, "https://ex ample.com/a@b");
    }

    #[test]
    fn leaves_a_clean_unparseable_looking_url_unchanged() {
        // ~keep Same malformed-host family as the other fallback tests, but with no
        // userinfo at all: the fallback must not invent one.
        assert_eq!(
            redact_url_credentials("https://ex ample.com/clean"),
            "https://ex ample.com/clean"
        );
    }
}
