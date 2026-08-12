//! Origin comparison for credential scoping across redirect hops.

use url::Url;

/// Whether `target` is on the same host as `origin`.
///
/// Host comparison is ASCII-case-insensitive (DNS names are case-insensitive, and
/// `Url::host_str` preserves the case the author wrote). Scheme and port are
/// deliberately *not* compared: the property being enforced is "these credentials
/// stay with the party they were issued to", and an `http` → `https` upgrade or a
/// port change on the same host does not change that party.
///
/// A URL with no host (`data:`, `file:`) never matches, so credentials are withheld.
pub(crate) fn same_host(origin: &Url, target: &Url) -> bool {
    match (origin.host_str(), target.host_str()) {
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        s.parse().expect("test URL must parse")
    }

    #[test]
    fn same_host_matches_identical_hosts() {
        assert!(same_host(&url("https://example.com/a"), &url("https://example.com/b")));
    }

    #[test]
    fn same_host_ignores_host_case() {
        assert!(same_host(&url("https://Example.COM/a"), &url("https://example.com/b")));
    }

    #[test]
    fn same_host_ignores_scheme_and_port() {
        assert!(same_host(
            &url("http://example.com/a"),
            &url("https://example.com:8443/b")
        ));
    }

    #[test]
    fn same_host_rejects_a_different_host() {
        assert!(!same_host(
            &url("https://example.com/a"),
            &url("https://attacker.test/b")
        ));
    }

    #[test]
    fn same_host_rejects_a_subdomain() {
        assert!(!same_host(
            &url("https://example.com/a"),
            &url("https://evil.example.com/b")
        ));
    }

    #[test]
    fn same_host_rejects_a_suffix_extension_of_the_origin_host() {
        assert!(!same_host(
            &url("https://example.com/a"),
            &url("https://example.com.attacker.test/b")
        ));
    }

    #[test]
    fn same_host_rejects_a_hostless_url() {
        assert!(!same_host(&url("https://example.com/a"), &url("data:text/plain,x")));
    }
}
