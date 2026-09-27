//! Origin comparison for credential scoping across redirect hops and, more generally,
//! across the fetches one crawl makes.

use url::Url;

/// Whether `target`'s host matches `origin_host`, ASCII-case-insensitively.
///
/// Host comparison is ASCII-case-insensitive (DNS names are case-insensitive, and
/// `Url::host_str` preserves the case the author wrote). Scheme and port are
/// deliberately *not* compared: the property being enforced is "these credentials
/// stay with the party they were issued to", and an `http` → `https` upgrade or a
/// port change on the same host does not change that party. There is deliberately no
/// subdomain widening either: a crawl may follow a subdomain when `allow_subdomains`
/// permits it, but that is a *crawl-scope* decision, not a credential-scope one --
/// configured credentials stay with the exact host they were configured for.
///
/// `target` having no host (`data:`, `file:`) never matches, so credentials are withheld.
pub(crate) fn is_authorized_host(origin_host: &str, target: &Url) -> bool {
    target
        .host_str()
        .is_some_and(|host| host.eq_ignore_ascii_case(origin_host))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        s.parse().expect("test URL must parse")
    }

    #[test]
    fn is_authorized_host_matches_identical_hosts() {
        assert!(is_authorized_host("example.com", &url("https://example.com/b")));
    }

    #[test]
    fn is_authorized_host_matches_case_insensitively() {
        assert!(is_authorized_host("Example.COM", &url("https://example.com/a")));
    }

    #[test]
    fn is_authorized_host_ignores_scheme_and_port() {
        assert!(is_authorized_host("example.com", &url("https://example.com:8443/b")));
    }

    #[test]
    fn is_authorized_host_rejects_a_different_host() {
        assert!(!is_authorized_host("example.com", &url("https://attacker.test/a")));
    }

    #[test]
    fn is_authorized_host_rejects_a_subdomain() {
        assert!(!is_authorized_host("example.com", &url("https://evil.example.com/a")));
    }

    #[test]
    fn is_authorized_host_rejects_a_suffix_extension_of_the_origin_host() {
        assert!(!is_authorized_host(
            "example.com",
            &url("https://example.com.attacker.test/b")
        ));
    }

    #[test]
    fn is_authorized_host_rejects_a_hostless_url() {
        assert!(!is_authorized_host("example.com", &url("data:text/plain,x")));
    }
}
