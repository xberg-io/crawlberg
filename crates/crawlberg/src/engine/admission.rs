//! Seed admission: the one place a caller's URL enters the engine.
//!
//! Every public entry point admits its URL here before anything else reads it. Admission
//! parses the URL once, moves any `user:pass@` into the engine's credential scope, and hands
//! on a [`SeedUrl`] that has no userinfo. The crawl internals take a `&SeedUrl`, so they
//! cannot be called with a raw string.

use url::Url;

use super::CrawlEngine;
use crate::error::CrawlError;
use crate::net::CredentialScope;
use crate::net::userinfo;

/// Placeholder reported instead of a URL that did not parse, so the raw input is never echoed.
const UNPARSEABLE_URL: &str = "(unparseable URL)";

/// Parse a caller's URL, refusing one that has no host and contains an `@`.
///
/// ~keep `user:pw@host/path` parses as scheme `user` with no host, so it has no userinfo to
/// ~keep split off, and its password would become the seed's text. The credential redactor
/// ~keep hides such a value whole for the same reason.
fn parse_caller_url(raw: &str) -> Result<Url, String> {
    let url = Url::parse(raw).map_err(|e| format!("invalid URL: {e}"))?;
    if url.host().is_none() && raw.contains('@') {
        return Err("invalid URL: it has no host and contains an `@`, which may be a credential".to_owned());
    }
    Ok(url)
}

/// A caller's URL after admission: parsed, and without userinfo.
///
/// The text is the caller's own string when it carried no userinfo, so a URL without
/// credentials reaches results exactly as the caller wrote it. Otherwise it is the parsed URL
/// with the userinfo removed.
#[derive(Debug, Clone)]
pub(crate) struct SeedUrl {
    url: Url,
    text: String,
}

impl SeedUrl {
    /// The admitted URL as a string.
    pub(crate) fn as_str(&self) -> &str {
        &self.text
    }

    /// The admitted URL.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub(crate) fn url(&self) -> &Url {
        &self.url
    }
}

#[cfg(test)]
impl SeedUrl {
    /// Wrap a test URL that has no userinfo, as admission would.
    pub(crate) fn for_test(url: &str) -> Self {
        let url = Url::parse(url).expect("test URL must parse");
        assert!(!userinfo::has_userinfo(&url), "a SeedUrl never carries userinfo");
        Self {
            text: url.as_str().to_owned(),
            url,
        }
    }
}

impl std::fmt::Display for SeedUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

/// The URL a batch result or stream error reports for the caller's `raw` URL.
///
/// It is the admitted URL when admission succeeded, and otherwise `raw` without userinfo.
/// A `raw` URL that does not parse is never echoed.
pub(crate) fn admission_key(raw: &str, admitted: Option<&SeedUrl>) -> String {
    if let Some(seed) = admitted {
        return seed.as_str().to_owned();
    }
    parse_caller_url(raw).map_or_else(
        |_| UNPARSEABLE_URL.to_owned(),
        |mut url| {
            userinfo::strip(&mut url);
            url.into()
        },
    )
}

impl CrawlEngine {
    /// Admit a caller's URL: split off its userinfo and scope credentials to its host.
    ///
    /// Returns a clone of this engine whose configuration carries the credential scope, and
    /// the clean seed URL. A URL that does not parse, or has no host and contains an `@`, is
    /// refused without echoing it. A URL that carries userinfo while `auth` is also configured
    /// is a configuration error.
    pub(crate) fn admit(&self, raw: &str) -> Result<(CrawlEngine, SeedUrl), CrawlError> {
        let parsed = parse_caller_url(raw).map_err(|reason| CrawlError::ssrf_violation(UNPARSEABLE_URL, reason))?;
        let (clean, basic) = userinfo::split(parsed);
        let text = if basic.is_some() {
            clean.as_str().to_owned()
        } else {
            raw.to_owned()
        };
        if basic.is_some() && self.config.auth.is_some() {
            return Err(CrawlError::invalid_config(
                "the URL carries credentials and `auth` is also set; use only one of them",
            ));
        }
        let mut engine = self.clone();
        engine.config.credential_scope = CredentialScope::for_seed(&clean, basic);
        Ok((engine, SeedUrl { url: clean, text }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AuthConfig, CrawlConfig};

    fn engine_with(config: CrawlConfig) -> CrawlEngine {
        CrawlEngine::builder()
            .config(config)
            .build()
            .expect("engine must build")
    }

    #[test]
    fn admission_moves_userinfo_into_the_credential_scope() {
        let engine = engine_with(CrawlConfig::default());
        let (admitted, seed) = engine
            .admit("http://user:secret@example.com/a")
            .expect("URL must be admitted");
        assert_eq!(seed.as_str(), "http://example.com/a");
        let scope = admitted.config.credential_scope.expect("scope must be set");
        assert!(scope.has_url_credentials());
        assert_eq!(scope.host(), "example.com");
        assert!(
            engine.config.credential_scope.is_none(),
            "the caller's engine is unchanged"
        );
    }

    #[test]
    fn admission_keeps_a_url_without_userinfo_as_the_caller_wrote_it() {
        let engine = engine_with(CrawlConfig::default());
        let (_, seed) = engine.admit("http://example.com").expect("URL must be admitted");
        assert_eq!(seed.as_str(), "http://example.com");
        let (_, seed) = engine
            .admit("http://user:secret@example.com")
            .expect("URL must be admitted");
        assert_eq!(seed.as_str(), "http://example.com/");
    }

    #[test]
    fn admission_scopes_configured_auth_to_the_seed_host() {
        let engine = engine_with(CrawlConfig::default());
        let (admitted, _) = engine.admit("http://example.com/").expect("URL must be admitted");
        let scope = admitted.config.credential_scope.expect("scope must be set");
        assert!(!scope.has_url_credentials());
        assert_eq!(scope.host(), "example.com");
    }

    #[test]
    fn admission_refuses_url_credentials_together_with_configured_auth() {
        let engine = engine_with(CrawlConfig {
            auth: Some(AuthConfig::Bearer {
                token: "tok".to_owned(),
            }),
            ..CrawlConfig::default()
        });
        let Err(error) = engine.admit("http://user:secret@example.com/") else {
            panic!("both credentials must be refused");
        };
        assert!(matches!(error, CrawlError::InvalidConfig { .. }), "{error:?}");
        assert!(!error.to_string().contains("secret"), "{error}");
        assert!(engine.admit("http://example.com/").is_ok());
    }

    #[test]
    fn admission_refuses_an_unparseable_url_without_echoing_it() {
        let engine = engine_with(CrawlConfig::default());
        let Err(error) = engine.admit("ht!tp://user:secret@exa mple/") else {
            panic!("an unparseable URL must be refused");
        };
        let rendered = format!("{error} {error:?}");
        assert!(!rendered.contains("secret"), "{rendered}");
        assert!(rendered.contains("invalid URL"), "{rendered}");
    }

    #[test]
    fn admission_refuses_a_hostless_url_with_an_at_sign_without_echoing_it() {
        let engine = engine_with(CrawlConfig::default());
        for raw in ["user:hunter2@evil.example/path", "user:hunt#er2@evil.example/path"] {
            let Err(error) = engine.admit(raw) else {
                panic!("{raw} must be refused");
            };
            let rendered = format!("{error} {error:?}");
            assert!(!rendered.contains("hunt"), "{rendered}");
            assert!(rendered.contains(UNPARSEABLE_URL), "{rendered}");
            assert_eq!(admission_key(raw, None), UNPARSEABLE_URL);
        }
        let (_, seed) = engine.admit("mailto:").expect("a hostless URL without `@` is admitted");
        assert_eq!(seed.as_str(), "mailto:");
    }
}
