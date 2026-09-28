//! `Debug` redaction for header maps and other caller secrets.
//!
//! ~keep crawlberg keeps the same list in `crawlberg::net::redact`, because this crate
//! cannot depend on crawlberg. A crawlberg test renders one header map through both and
//! requires the same text. The two constants are public only for that test, so they are
//! hidden from the docs.

use std::collections::HashMap;
use std::fmt;

/// Placeholder `Debug` prints in place of a secret.
#[doc(hidden)]
pub const REDACTED: &str = "***";

/// A denylist of response header names whose values are credentials: an `Authorization` or
/// `Proxy-Authorization` a server echoes, session cookies in either direction, the
/// `Authentication-Info` a server returns after a login, and the vendor tokens a server echoes
/// back (`X-Api-Key`, `X-Amz-Security-Token`). Names are lowercase and matched without case.
///
/// A response header outside this list prints in full. The list leaves out the challenge
/// headers `WWW-Authenticate` and `Proxy-Authenticate`, which carry no secret, the obsolete
/// `Set-Cookie2`, and any vendor token header it does not name. Request header maps do not use
/// it: they hide every value.
#[doc(hidden)]
pub const SENSITIVE_HEADERS: [&str; 7] = [
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
    "x-api-key",
    "x-amz-security-token",
    "authentication-info",
];

/// Whether `name` is one of [`SENSITIVE_HEADERS`], in any case.
pub(crate) fn is_sensitive_header(name: &str) -> bool {
    SENSITIVE_HEADERS
        .iter()
        .any(|sensitive| name.eq_ignore_ascii_case(sensitive))
}

/// `Debug` view of a **request** header map: every name stays visible, every value is hidden.
///
/// ~keep Request headers are populated from caller configuration (`CrawlConfig.custom_headers`,
/// ~keep `NativeBrowserConfig.extra_headers`), whose values are hidden wholesale at the config
/// ~keep type. A caller sends an API key under whatever name the vendor asks for (`X-Api-Key`,
/// ~keep `apikey`, ...), so a name denylist cannot cover it and would contradict the config
/// ~keep type on the same data. [`RedactedHeaders`] stays for *response* headers, where
/// ~keep `content-type` and `server` are the debugging value.
pub(crate) struct RedactedValues<'a>(pub(crate) &'a HashMap<String, String>);

impl fmt::Debug for RedactedValues<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.0.keys().map(|key| (key, REDACTED))).finish()
    }
}

/// `Debug` view of a **response** header map: every name and every value print, except the
/// value of a [`SENSITIVE_HEADERS`] entry, which prints as [`REDACTED`].
///
/// ~keep Response headers come from the server, not from caller configuration, so
/// ~keep `content-type`, `server` and the rest are worth keeping. Use [`RedactedValues`] for
/// ~keep a request header map.
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
