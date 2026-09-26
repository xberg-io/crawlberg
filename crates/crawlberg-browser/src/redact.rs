//! `Debug` redaction for header maps and other caller secrets.
//!
//! ~keep crawlberg keeps the same list in `crawlberg::net::redact`, because this crate
//! cannot depend on crawlberg. A crawlberg test pins the two lists and placeholders equal.

use std::collections::HashMap;
use std::fmt;

/// Placeholder `Debug` prints in place of a secret.
pub const REDACTED: &str = "***";

/// Request and response headers whose values are credentials: the caller's own
/// `Authorization`, a proxy's, and session cookies in either direction. Names are
/// lowercase and matched without case.
pub const SENSITIVE_HEADERS: [&str; 4] = ["authorization", "proxy-authorization", "cookie", "set-cookie"];

/// Whether `name` is one of [`SENSITIVE_HEADERS`], in any case.
pub(crate) fn is_sensitive_header(name: &str) -> bool {
    SENSITIVE_HEADERS
        .iter()
        .any(|sensitive| name.eq_ignore_ascii_case(sensitive))
}

/// `Debug` view of a header map that shows each name and hides each value.
pub(crate) struct RedactedValues<'a>(pub(crate) &'a HashMap<String, String>);

impl fmt::Debug for RedactedValues<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.0.keys().map(|key| (key, REDACTED))).finish()
    }
}

/// `Debug` view of a header map that shows every name and every value, except the value
/// of a [`SENSITIVE_HEADERS`] entry, which prints as [`REDACTED`].
pub(crate) struct RedactedHeaders<'a>(pub(crate) &'a HashMap<String, String>);

impl fmt::Debug for RedactedHeaders<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map()
            .entries(self.0.iter().map(|(name, value)| {
                let value = if is_sensitive_header(name) {
                    REDACTED
                } else {
                    value.as_str()
                };
                (name, value)
            }))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacted_headers_hide_only_sensitive_values_in_any_case() {
        let map = HashMap::from([
            ("Cookie".to_owned(), "sid=s3cr3t".to_owned()),
            ("PROXY-AUTHORIZATION".to_owned(), "Basic dXNlcjpwdw==".to_owned()),
            ("accept".to_owned(), "text/html".to_owned()),
        ]);
        let debug = format!("{:?}", RedactedHeaders(&map));
        assert!(
            !debug.contains("s3cr3t") && !debug.contains("dXNlcjpwdw"),
            "got {debug}"
        );
        assert!(debug.contains(r#""Cookie": "***""#), "got {debug}");
        assert!(debug.contains(r#""PROXY-AUTHORIZATION": "***""#), "got {debug}");
        assert!(debug.contains(r#""accept": "text/html""#), "got {debug}");
    }
}
