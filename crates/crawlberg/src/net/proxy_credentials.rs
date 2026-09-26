//! Proxy-credential embedding shared by both native browser backends:
//! `crate::native_browser` (crawl and scrape) and `crate::interact::native`
//! (interact).
//!
//! Credentials are embedded into the proxy URL's userinfo via
//! `url::Url::set_username`/`set_password`, which read the scheme through
//! the URL parser (so an upper- or mixed-case scheme still gets its
//! credentials inlined) and percent-encode the userinfo component. A
//! `format!("{scheme}://{user}:{pass}@{rest}")` splice on the raw string
//! would let a `:`, `@`, or `/` in a credential corrupt the authority and
//! smuggle in a different host; the setters avoid that by construction.
//! They work for any scheme with an authority component (`http`, `https`,
//! `socks5`, `socks5h`, ...), so credentials survive for every proxy
//! scheme, not only `http`/`https`.
//!
//! `url`'s userinfo percent-encoding leaves a literal `%` untouched (it is
//! not in the reserved set, because `%` itself introduces an escape), but
//! the proxy consumer (`reqwest::Proxy`) percent-*decodes* the resolved
//! username/password once, to build the proxy's Basic-Auth header. An
//! unescaped `%` therefore reads back as the start of an escape: a
//! password of `p%41` decodes to `pA`. This module escapes a literal `%`
//! to `%25` before handing the credential to `set_username`/`set_password`,
//! so it round-trips byte-for-byte, matching the plain HTTP proxy path
//! (`crate::http::client`'s `apply_proxy`), which sends credentials via
//! `Proxy::basic_auth` and never percent-encodes them at all.

use url::Url;

use crate::error::CrawlError;
use crate::types::ProxyConfig;

/// Embed `proxy`'s username/password into its URL, if either is set.
///
/// Returns the URL unchanged when there are no credentials to embed.
/// Returns [`CrawlError::InvalidConfig`] when the URL does not parse, or
/// when its scheme has no authority component to hold credentials.
pub(crate) fn embed_proxy_credentials(proxy: &ProxyConfig) -> Result<String, CrawlError> {
    if proxy.username.is_none() && proxy.password.is_none() {
        return Ok(proxy.url.clone());
    }

    let mut parsed =
        Url::parse(&proxy.url).map_err(|e| CrawlError::invalid_config(format!("invalid proxy URL: {e}")))?;

    let scheme_rejects_credentials = |parsed: &Url| {
        CrawlError::invalid_config(format!(
            "proxy scheme {:?} does not support embedded credentials",
            parsed.scheme()
        ))
    };

    let username = escape_percent(proxy.username.as_deref().unwrap_or(""));
    parsed
        .set_username(&username)
        .map_err(|()| scheme_rejects_credentials(&parsed))?;

    let password = proxy.password.as_deref().map(escape_percent);
    parsed
        .set_password(password.as_deref())
        .map_err(|()| scheme_rejects_credentials(&parsed))?;

    Ok(parsed.to_string())
}

/// Escape a literal `%` to `%25` so it survives the percent-decode a proxy
/// consumer applies when reading the credential back out of the URL.
/// `url::Url::set_username`/`set_password` percent-encode every other
/// userinfo-reserved byte already; `%` is the one byte they leave alone.
fn escape_percent(value: &str) -> String {
    value.replace('%', "%25")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proxy(url: &str, username: Option<&str>, password: Option<&str>) -> ProxyConfig {
        ProxyConfig {
            url: url.to_owned(),
            username: username.map(str::to_owned),
            password: password.map(str::to_owned),
        }
    }

    /// Minimal percent-decoder sufficient for the ASCII userinfo characters this module encodes.
    /// No percent-decoding crate is a direct dependency, so this mirrors the decode step the
    /// real proxy consumer (`reqwest::Proxy`) performs when it reads the credential back out.
    fn urlencoding_decode(input: &str) -> String {
        let bytes = input.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%'
                && i + 2 < bytes.len()
                && let Ok(text) = std::str::from_utf8(&bytes[i + 1..i + 3])
                && let Ok(value) = u8::from_str_radix(text, 16)
            {
                out.push(value);
                i += 3;
                continue;
            }
            out.push(bytes[i]);
            i += 1;
        }
        String::from_utf8(out).expect("decoded bytes must be valid UTF-8")
    }

    #[test]
    fn credentials_are_embedded_for_lower_upper_and_mixed_case_schemes() {
        let lower = embed_proxy_credentials(&proxy("http://proxy.test:8080", Some("alice"), Some("s3cr3t")))
            .expect("lower-case scheme with credentials must resolve");
        assert_eq!(lower, "http://alice:s3cr3t@proxy.test:8080/");

        let upper = embed_proxy_credentials(&proxy("HTTP://proxy.test:8080", Some("alice"), Some("s3cr3t")))
            .expect("upper-case scheme with credentials must resolve");
        assert_eq!(
            upper, "http://alice:s3cr3t@proxy.test:8080/",
            "an upper-case scheme must still get its credentials inlined"
        );

        let mixed = embed_proxy_credentials(&proxy("Https://proxy.test:8443", Some("alice"), Some("s3cr3t")))
            .expect("mixed-case scheme with credentials must resolve");
        assert_eq!(
            mixed, "https://alice:s3cr3t@proxy.test:8443/",
            "a mixed-case scheme must still get its credentials inlined"
        );
    }

    #[test]
    fn socks5_credentials_are_not_silently_dropped() {
        let resolved = embed_proxy_credentials(&proxy("socks5://proxy.test:1080", Some("alice"), Some("s3cr3t")))
            .expect("socks5 proxy with credentials must resolve");
        assert_eq!(
            resolved, "socks5://alice:s3cr3t@proxy.test:1080",
            "SOCKS5 credentials must be embedded, not dropped"
        );
    }

    #[test]
    fn socks5h_credentials_are_embedded() {
        let resolved = embed_proxy_credentials(&proxy("socks5h://proxy.test:1080", Some("bob"), Some("hunter2")))
            .expect("socks5h proxy with credentials must resolve");
        assert_eq!(resolved, "socks5h://bob:hunter2@proxy.test:1080");
    }

    #[test]
    fn special_characters_in_credentials_are_percent_encoded_not_spliced() {
        // A `:`/`@`/`/` in a credential must not be able to terminate the userinfo early and
        // smuggle in a different host, or split a single credential into `user:pass` pairs.
        let resolved = embed_proxy_credentials(&proxy(
            "http://proxy.test:8080",
            Some("weird:user@name"),
            Some("p/a:s@s"),
        ))
        .expect("proxy with special-character credentials must still resolve");

        // The credentials must decode back to the exact original values, and the host must
        // still be `proxy.test:8080`, not hijacked by a `@` or `:` inside a credential.
        let parsed = url::Url::parse(&resolved).expect("resolved proxy URL must itself be valid");
        assert_eq!(parsed.host_str(), Some("proxy.test"));
        assert_eq!(parsed.port(), Some(8080));
        assert_eq!(
            urlencoding_decode(parsed.username()),
            "weird:user@name",
            "username must decode back to the exact original value"
        );
        assert_eq!(
            urlencoding_decode(parsed.password().expect("password must be present")),
            "p/a:s@s",
            "password must decode back to the exact original value"
        );
    }

    #[test]
    fn a_percent_sign_in_a_credential_survives_byte_for_byte() {
        // A raw `p%41` embedded without escaping the `%` first would round-trip through the
        // proxy consumer's single percent-decode as `pA` ('%41' decodes to 'A'): the corruption
        // this test guards against.
        let resolved = embed_proxy_credentials(&proxy("http://proxy.test:8080", Some("alice"), Some("p%41ss")))
            .expect("proxy with a percent sign in the password must still resolve");

        let parsed = url::Url::parse(&resolved).expect("resolved proxy URL must itself be valid");
        let decoded_password = urlencoding_decode(parsed.password().expect("password must be present"));
        assert_eq!(
            decoded_password, "p%41ss",
            "a literal '%' in a credential must survive the encode/decode round trip byte-for-byte"
        );
        assert_ne!(
            decoded_password, "pAss",
            "the '%41' must not be read back as a percent-escaped 'A'"
        );
    }

    #[test]
    fn credential_free_proxy_url_is_passed_through_unchanged() {
        let resolved = embed_proxy_credentials(&proxy("http://proxy.test:8080", None, None))
            .expect("credential-free proxy must resolve");
        assert_eq!(
            resolved, "http://proxy.test:8080",
            "URL must be passed through verbatim when there are no credentials to embed"
        );
    }

    #[test]
    fn an_unparseable_proxy_url_with_credentials_returns_invalid_config_error() {
        let result = embed_proxy_credentials(&proxy("not a url", Some("alice"), Some("s3cr3t")));
        assert!(
            matches!(result, Err(CrawlError::InvalidConfig { .. })),
            "malformed proxy URL with credentials must return InvalidConfig, got {result:?}"
        );
    }
}
