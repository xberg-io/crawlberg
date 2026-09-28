//! Credential redaction for values that may reach `tracing` fields or error `Display`
//! output.
//!
//! Secrets this crate handles — proxy credentials embedded in a URL's userinfo, API
//! keys, auth tokens — must never appear verbatim in a span field or an error message,
//! since both are routinely shipped to logs, OTLP collectors, and issue trackers. This
//! module centralizes that redaction so every call site applies the same rule.

/// The text every redacting `Debug` impl and helper here prints in place of a secret.
#[doc(hidden)]
pub const REDACTED_PLACEHOLDER: &str = "***";

/// What [`redact_url_credentials`] returns for a value it cannot read as one address.
///
/// Nothing of the value is kept. Without a parsed host there is no reliable way to tell where
/// a `user:password@` ends, so any part of a value that holds an `@` can be a credential.
const HIDDEN_ADDRESS: &str = "[address hidden: it may carry credentials]";

/// Redact `user[:password]@` userinfo from a URL-like string.
///
/// Parses `input` as an absolute URL; if it carries a username and/or password, both are
/// replaced with a fixed placeholder before re-serializing, so the scheme/host/path
/// remain useful for debugging while the credential bytes never reach the output.
///
/// Only a value that parses to a URL with a host, and has no whitespace, is read this way. A
/// value with whitespace is a message rather than one address, even when its first word parses.
/// Any other value that contains an `@` is replaced whole with [`HIDDEN_ADDRESS`]: a value that
/// fails to parse (a stray space in the host), a scheme-less `user:pw@host` (which parses as
/// scheme `user` with no host), a `mailto:` address or a message. A value without an `@` cannot
/// carry userinfo and is returned unchanged.
/// This makes the function safe to call unconditionally on any string that *might* be a URL,
/// such as an error message being assembled for display.
#[must_use]
pub fn redact_url_credentials(input: &str) -> String {
    let mut url = match url::Url::parse(input) {
        Ok(url) if url.host().is_some() && !input.contains(char::is_whitespace) => url,
        _ if input.contains('@') => return HIDDEN_ADDRESS.to_owned(),
        _ => return input.to_owned(),
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

/// Redact a URL down to its origin: scheme, host and non-default port.
///
/// For an endpoint whose *capability is the URL itself*. The canonical CDP endpoint is
/// `ws://host:9222/devtools/browser/<GUID>`: the GUID in the **path** is the bearer token
/// (anyone holding it drives the browser), and a proxied endpoint may instead carry a
/// `?token=`. Neither the path, the query, the fragment nor the userinfo may be printed, so
/// this keeps only the origin, which is the part an operator needs to tell one endpoint from
/// another.
///
/// The port is kept deliberately. It is the field that distinguishes a container-mapped CDP
/// port from the default 9222, which is what makes a connection failure diagnosable, and it
/// is no more secret than the host it belongs to. `url::Url::port` reports `None` for a
/// scheme's default port, so `wss://host:443` prints as `wss://host`.
///
/// Fails **closed**: returns the placeholder when `input` does not parse as an absolute URL
/// or carries no host, so an endpoint the parser rejects is never echoed.
#[must_use]
pub fn redact_url_to_origin(input: &str) -> String {
    let Ok(url) = url::Url::parse(input) else {
        return REDACTED_PLACEHOLDER.to_owned();
    };
    let Some(host) = url.host() else {
        return REDACTED_PLACEHOLDER.to_owned();
    };
    // ~keep `host` is formatted through `url::Host`, not `host_str`, so an IPv6 literal keeps
    // ~keep its brackets and the `:port` suffix below stays unambiguous.
    match url.port() {
        Some(port) => format!("{}://{host}:{port}", url.scheme()),
        None => format!("{}://{host}", url.scheme()),
    }
}

/// `Debug` text for a caller's script or template: the placeholder and the length, never
/// the text. A script is a common place to embed a token.
pub(crate) fn redacted_text(text: &str) -> String {
    format!("{REDACTED_PLACEHOLDER} ({} bytes)", text.len())
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

/// A denylist of response header names whose values are credentials: an `Authorization` or
/// `Proxy-Authorization` a server echoes, session cookies in either direction, the
/// `Authentication-Info` a server returns after a login, and the vendor tokens a server echoes
/// back (`X-Api-Key`, `X-Amz-Security-Token`). Names are lowercase and matched without case.
///
/// A response header outside this list prints in full. The list leaves out the challenge
/// headers `WWW-Authenticate` and `Proxy-Authenticate`, which carry no secret, the obsolete
/// `Set-Cookie2`, and any vendor token header it does not name. Request header maps do not use
/// it: they hide every value.
pub(crate) const SENSITIVE_HEADERS: [&str; 7] = [
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

/// `Debug` view of a header map that shows every name and every value, except the value
/// of a [`SENSITIVE_HEADERS`] entry, which prints as the placeholder.
pub(crate) struct RedactedHeaders<'a, K, V>(pub(crate) &'a std::collections::HashMap<K, V>);

impl<K: AsRef<str> + std::fmt::Debug, V: std::fmt::Debug> std::fmt::Debug for RedactedHeaders<'_, K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map()
            .entries(self.0.iter().map(|(name, value)| {
                // ~keep The placeholder replaces the whole value, a list of values included, so a
                // ~keep sensitive multi-value header prints as one string on purpose.
                let value: &dyn std::fmt::Debug = if is_sensitive_header(name.as_ref()) {
                    &REDACTED_PLACEHOLDER
                } else {
                    value
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
    fn redact_url_to_origin_hides_the_path_query_fragment_and_userinfo() {
        // ~keep This is the canonical CDP endpoint shape. The GUID in the path IS the
        // ~keep capability, so it must not survive; an earlier version of this test pinned
        // ~keep the opposite, asserting the whole path printed unchanged.
        assert_eq!(
            redact_url_to_origin("ws://127.0.0.1:9222/devtools/browser/b1946ac9-2d2e-4f1f"),
            "ws://127.0.0.1:9222"
        );
        assert_eq!(
            redact_url_to_origin("wss://user:pw@chrome.example:3000/devtools?token=abc123#frag"),
            "wss://chrome.example:3000"
        );
    }

    #[test]
    fn redact_url_to_origin_omits_a_default_port_and_brackets_ipv6() {
        assert_eq!(
            redact_url_to_origin("wss://chrome.example:443/devtools"),
            "wss://chrome.example"
        );
        assert_eq!(
            redact_url_to_origin("ws://[::1]:9222/devtools/browser/42"),
            "ws://[::1]:9222"
        );
    }

    #[test]
    fn redact_url_to_origin_fails_closed() {
        for hostless in [
            "not a url at all",
            "/devtools/browser/b1946ac9-2d2e-4f1f",
            "ws://:9222/devtools/browser/42",
            "data:text/plain,secret",
        ] {
            assert_eq!(
                redact_url_to_origin(hostless),
                "***",
                "an endpoint with no parseable host must never be echoed, got input '{hostless}'"
            );
        }
    }

    #[test]
    fn redacted_values_shows_keys_only() {
        let map = std::collections::HashMap::from([("Authorization".to_owned(), "Bearer abc123".to_owned())]);
        assert_eq!(format!("{:?}", RedactedValues(&map)), r#"{"Authorization": "***"}"#);
    }

    #[test]
    fn redacted_headers_hide_only_sensitive_values_in_any_case() {
        let map = std::collections::HashMap::from([
            ("Authorization".to_owned(), vec!["Bearer abc123".to_owned()]),
            ("set-cookie".to_owned(), vec!["sid=s3cr3t".to_owned()]),
            ("content-type".to_owned(), vec!["text/html".to_owned()]),
        ]);
        let debug = format!("{:?}", RedactedHeaders(&map));
        assert!(!debug.contains("abc123") && !debug.contains("s3cr3t"), "got {debug}");
        assert!(debug.contains(r#""Authorization": "***""#), "got {debug}");
        assert!(debug.contains(r#""set-cookie": "***""#), "got {debug}");
        assert!(debug.contains(r#""content-type": ["text/html"]"#), "got {debug}");
    }

    #[test]
    fn redacted_headers_hide_echoed_vendor_tokens_and_login_info() {
        for name in ["X-Api-Key", "x-amz-security-token", "Authentication-Info"] {
            let map = std::collections::HashMap::from([(name.to_owned(), vec!["s3cr3t".to_owned()])]);
            let debug = format!("{:?}", RedactedHeaders(&map));
            assert_eq!(debug, format!(r#"{{"{name}": "***"}}"#));
        }
    }

    #[cfg(feature = "browser-native")]
    #[test]
    fn header_redaction_renders_the_same_in_the_native_browser_crate() {
        // ~keep One header per event, so the text compared does not depend on map order.
        for name in SENSITIVE_HEADERS
            .iter()
            .chain(crawlberg_browser::redact::SENSITIVE_HEADERS.iter())
            .map(|name| name.to_ascii_uppercase())
            .chain(["Content-Type".to_owned(), "Server".to_owned()])
        {
            let headers = std::collections::HashMap::from([(name.clone(), "v4lue".to_owned())]);
            let ours = format!("{:?}", RedactedHeaders(&headers));
            let event = crawlberg_browser::adapter::NativeNetworkEvent {
                url: String::new(),
                method: String::new(),
                resource_type: String::new(),
                status: 200,
                request_headers: std::collections::HashMap::new(),
                response_headers: headers,
                body_size: 0,
                timestamp_ms: 0,
            };
            let theirs = format!("{event:?}");
            assert!(
                theirs.contains(&format!("response_headers: {ours}")),
                "header {name} renders differently: crawlberg {ours}, native browser {theirs}"
            );
        }
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
    fn hides_a_value_with_an_at_sign_that_is_not_one_url() {
        // ~keep The accepted cost of hiding the whole value: an address or message that holds an
        // `@` which is not userinfo (a `mailto:` or `data:` value, an e-mail address in a
        // sentence, a file name with an `@`) is hidden too.
        for value in [
            "https://user:pw@ex ample.com/x",
            "user:pw@host",
            "mailto:user@example.com",
            "data:,foo@bar",
            "contact admin@example.com for help",
            "https://example.com/My Page@2x.png",
            "u@",
            "@host",
        ] {
            assert_eq!(redact_url_credentials(value), HIDDEN_ADDRESS, "input '{value}'");
        }
    }

    #[test]
    fn leaves_values_without_an_at_sign_unchanged() {
        for value in [
            "",
            "https://ex ample.com/clean",
            "https://example.com/My Page.html",
            "file:///x",
            "mailto:",
            "connect to [2001:db8::1]:443 refused",
            "C:\\Users\\bob\\file.txt",
        ] {
            assert_eq!(
                redact_url_credentials(value),
                value,
                "'{value}' must come back unchanged"
            );
        }
    }

    #[test]
    fn hostile_values_come_back_redacted_or_hidden() {
        // ~keep Every credential-bearing input the reviews of this redactor probed. A value that
        // parses with a host keeps its parsed redaction; every other one is hidden whole.
        let parsed = [
            ("https:/u:pw@h", "https://***:***@h/"),
            ("https:u:pw@h", "https://***:***@h/"),
            ("http:///u:pw@proxy", "http://***:***@proxy/"),
            ("https://a@b@host/", "https://***@host/"),
        ];
        let hidden = [
            "https://u:p@ex ample.com/a@b",
            "u:p@host",
            "user:pw@host/path",
            "user:pw@host/a//b",
            "user:pw@host//x",
            "http://u:p@[fe80::1%eth0]/",
            "http://u:p@[fe80::1%25eth0]:8080/x",
            "https://u:p@ss@ex ample.com/",
            "https://u%40x:p%40@ex ample.com/",
            "https://user:pa/ss@ex ample.com/",
            "https:\\\\u:p@ex ample.com\\x",
            "HTTPS://U:P@EX AMPLE.COM/",
            "file://u:p@ex ample/",
            "http://u:p@ex ample.com\\x",
            "https://u:p@ex ample.com?x=y@z",
            "https://u:p@@ex ample.com/",
            "u:p@",
            "u:p@ex ample.com",
            "user:pw@host ",
            "connect to user:pw@proxy:8080 failed",
            "redis://u:hunter2@ex ample.com/",
            "socks4://u:hunter2@ex ample.com:1080",
            "sftp://u:hunter2@ex ample.com/",
            "file://u:hunter2@ex ample/",
            "https:\\\\u:hunter2@ex ample.com\\x",
            "redis://u:hunter2@ex ample",
            "error: https://u:hunter2@ex ample.com",
            "connect to \"user:pw@proxy:8080\" failed",
            "proxy=http://u:pw@ex ample.com failed",
            "(https://u:pw@ex ample.com)",
            "a\tuser:pw@host",
            "line1\nhttps://u:pw@ex ample.com",
            "connect\u{00A0}user:pw@host failed",
            "connect\u{3000}to user:pw@host",
            "https://example.com/?q=1 via http://u:pw@proxy",
            "https://example.com/?q=1 via user:pw@proxy",
            "https://example.com/?q=1\tvia\tuser:pw@proxy",
            "https://example.com/?q=1\nvia user:pw@proxy",
            "https://example.com/?q=1\r\nvia user:pw@proxy",
            "https://example.com/?q=1\u{00A0}via\u{00A0}user:pw@proxy",
            "https://example.com/?q=1\u{2028}via\u{2028}user:pw@proxy",
            "HTTPS://example.com/ user:pw@proxy",
            "https://ex ample.com/#x then user:pw@proxy",
            "retry https://u:pw@h1/ and https://v:hunter2@h2/ failed",
            "git clone https://TOKEN@github.com/o/r failed: x y",
            "https://user:p?ss@ex ample.com/",
            "https://user:p#ss@ex ample.com/",
            "user:p#ss@host",
            "user:p?ss@host",
            "connect to user:pw@ failed",
            "failed: user:pw@/tmp/sock",
            "user:pw@",
            "http://user:p#ss@proxy:8080",
            "{\"a\":\"#1\",\"proxy\":\"http://u:pw@h\"}",
            "{\"proxy\":\"http://u:pw@h:1\",\"x\":\"#\"}",
            "ftp://u:pw@ex ample.com/?a=b@c",
            "socks5h://u:pw@ex ample:1080 refused",
            "via\u{2028}user:pw@host",
            "x https://u:pw@ex ample.com?y=1 z user:hunter2@q",
            "user:pw@?x",
            "user:pw@#frag",
            "user:pw@\\\\x",
            "in msg user:pw@?x now",
            "in msg user:pw@#f now",
            "https://u:p#ss@h then https://v:pw@k done",
            "https://a.example/?x=1 then user:pw@h done",
            "file:///x then user:pw@host",
            "mailto:a@b then user:pw@host",
            "data:user:pw@h",
            "x data:user:pw@h y",
            "sip:alice:pw@host",
            "connect http:///u:pw@proxy failed",
            "connect https:////u:pw@proxy failed",
            "connect http:\\\\\\u:pw@proxy failed",
            "http:///u:pw@proxy/a b",
            "https:///u:pw@proxy/My Page.html",
            "redis:///u:pw@h",
            "connect redis:///u:pw@h failed",
            "connect https:/u:pw@h failed",
        ];
        let rows = parsed
            .into_iter()
            .chain(hidden.into_iter().map(|input| (input, HIDDEN_ADDRESS)));
        for (input, expected) in rows {
            assert_eq!(redact_url_credentials(input), expected, "input '{input}'");
        }
    }

    #[test]
    fn a_schemeless_address_with_a_password_is_hidden() {
        // ~keep `user:pw@host/path` parses as scheme `user` with no host, so nothing in it is
        // read as userinfo. The password may hold `/`, `#` or `?`, which end the URL path.
        let values = [
            "user:hunter2@evil.example/path",
            "user:hunter2@evil.example:8080/path",
            "user:hunt%40er2@evil.example/path",
            "user:hunt/er2@evil.example/path",
            "user:hunt#er2@evil.example/path",
            "user:hunt?er2@evil.example/path",
        ];
        let wrong: Vec<(&str, String)> = values
            .iter()
            .map(|value| (*value, redact_url_credentials(value)))
            .filter(|(_, redacted)| redacted != HIDDEN_ADDRESS)
            .collect();
        assert!(wrong.is_empty(), "expected every value hidden, got {wrong:?}");
    }

    #[test]
    fn ordinary_urls_are_redacted_exactly_as_before() {
        // ~keep Every value here parses with a host and has no whitespace, so it is read as a
        // URL. The expected column is the output of the parsed path alone, as before.
        let table = [
            ("https://example.com/", "https://example.com/"),
            ("http://example.com", "http://example.com"),
            ("https://example.com/a/b?c=d#e", "https://example.com/a/b?c=d#e"),
            ("https://user:pass@example.com/x", "https://***:***@example.com/x"),
            ("https://user@example.com/", "https://***@example.com/"),
            ("https://:pw@example.com/", "https://:***@example.com/"),
            ("http://127.0.0.1:8080/p", "http://127.0.0.1:8080/p"),
            ("http://[::1]:3000/", "http://[::1]:3000/"),
            ("https://u:p@[2001:db8::1]/x", "https://***:***@[2001:db8::1]/x"),
            ("https://xn--nxasmq6b.com/", "https://xn--nxasmq6b.com/"),
            ("https://例え.jp/パス", "https://例え.jp/パス"),
            ("ftp://u:p@ftp.example.com/f.txt", "ftp://***:***@ftp.example.com/f.txt"),
            ("ws://u:p@host:9/s", "ws://***:***@host:9/s"),
            ("https://example.com/%7Euser", "https://example.com/%7Euser"),
            ("https://example.com/a@b", "https://example.com/a@b"),
            (
                "https://example.com/?next=http://u:p@x.com",
                "https://example.com/?next=http://u:p@x.com",
            ),
            ("HTTP://EXAMPLE.COM/UP", "HTTP://EXAMPLE.COM/UP"),
            ("https://a@b@host/", "https://***@host/"),
            ("socks5://u:p@proxy:1080", "socks5://***:***@proxy:1080"),
            ("https://example.com:443/", "https://example.com:443/"),
        ];
        for (input, expected) in table {
            assert_eq!(redact_url_credentials(input), expected, "input '{input}'");
        }
    }
}
