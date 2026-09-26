//! SSRF policy and validation for outbound HTTP requests.
//!
//! Provides a deny-by-default policy on private IP space, DNS rebinding mitigation,
//! and allowlist matching for URLs in crawl, sitemap, and robots.txt operations.
//!
//! The implementation is split across private submodules; every public path this module
//! ever exposed is re-exported here unchanged, because these names are part of the
//! binding-generator surface.

mod error;
mod matcher;
mod policy;
mod validate;

pub use error::SsrfError;
pub use matcher::HostMatcher;
pub use policy::SsrfPolicy;
pub use validate::validate_url;

// ~keep Each re-export is gated to its only consumer -- `net::resolver` (non-wasm only) and the
// ~keep `net::browser_policy` deny-list parity test. Ungated, either is an unused import in the
// ~keep builds that lack that consumer, which -D warnings rejects.
#[cfg(all(test, feature = "browser-native"))]
pub(crate) use validate::DEFAULT_DENY_NET_CIDRS;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use validate::{classify_private_ip, is_ip_permitted};

/// IPv6 literals that embed an IPv4 address, each paired with the denial reason the
/// default policy must report, or `None` when the address must stay permitted.
///
/// Shared by the pre-connect check, the connect-time resolver and the browser parity
/// test, so all three are held to one table.
#[cfg(test)]
pub(crate) const EMBEDDED_IPV4_CASES: &[(&str, Option<&str>)] = &[
    // IPv4-mapped, RFC 4291 section 2.5.5.2.
    ("::ffff:127.0.0.1", Some("loopback")),
    ("::ffff:10.0.0.5", Some("private_network")),
    ("::ffff:169.254.169.254", Some("link_local")),
    ("::ffff:8.8.8.8", None),
    // IPv4-compatible, RFC 4291 section 2.5.5.1.
    ("::127.0.0.1", Some("loopback")),
    ("::10.0.0.5", Some("private_network")),
    ("::169.254.169.254", Some("link_local")),
    ("::8.8.8.8", None),
    // IPv4-translated, RFC 2765 section 2.1.
    ("::ffff:0:127.0.0.1", Some("loopback")),
    ("::ffff:0:10.0.0.5", Some("private_network")),
    ("::ffff:0:169.254.169.254", Some("link_local")),
    ("::ffff:0:8.8.8.8", None),
    // NAT64 well-known prefix, RFC 6052 section 2.1.
    ("64:ff9b::127.0.0.1", Some("loopback")),
    ("64:ff9b::10.0.0.5", Some("private_network")),
    ("64:ff9b::169.254.169.254", Some("link_local")),
    ("64:ff9b::8.8.8.8", None),
    // 6to4, RFC 3056 section 2: the IPv4 address sits in bits 16 to 47.
    ("2002:7f00:1::", Some("loopback")),
    ("2002:a00:5::", Some("private_network")),
    ("2002:a9fe:a9fe::", Some("link_local")),
    ("2002:808:808::", None),
    // Local-use NAT64 prefix, RFC 8215, with the IPv4 address at each position RFC 6052
    // section 2.2 allows inside a /48: after a /48, /56, /64 and /96 prefix.
    ("64:ff9b:1:a00:0:500::", Some("private_network")),
    ("64:ff9b:1:808:8:800::", None),
    ("64:ff9b:1:a:0:5::", Some("private_network")),
    ("64:ff9b:1:8:8:808::", None),
    ("64:ff9b:1:0:a:0:500:0", Some("private_network")),
    ("64:ff9b:1:0:8:808:800:0", None),
    ("64:ff9b:1::10.0.0.5", Some("private_network")),
    ("64:ff9b:1::127.0.0.1", Some("loopback")),
    ("64:ff9b:1::8.8.8.8", None),
    // Every octet of each position decides: 169.254.0.0/16 and 172.16.0.0/12 need the second
    // octet read from the right place. The /48 metadata address also reads as 169.254.0.0 at
    // the /64 position, so 172.16.8.8 is the case that pins the /48 position alone.
    ("64:ff9b:1:a9fe:a9:fe00::", Some("link_local")),
    ("64:ff9b:1:ac10:8:800::", Some("private_network")),
    ("64:ff9b:1:a9:fe:a9fe::", Some("link_local")),
    ("64:ff9b:1:ac:10:808::", Some("private_network")),
    ("64:ff9b:1:0:a9:fea9:fe00:0", Some("link_local")),
    ("64:ff9b:1:0:ac:1008:800:0", Some("private_network")),
    ("64:ff9b:1::169.254.169.254", Some("link_local")),
    // A position that reads as multicast is skipped: 8.8.8.230 after a /64 prefix reads as
    // 230.0.0.0 at the /96 position.
    ("64:ff9b:1:0:8:808:e600:0", None),
    // An address whose every position reads as 0.0.0.0/8, multicast or 240.0.0.0/4 carries no real
    // destination, and a stateful NAT64 translator can forward 0.0.0.0 to its own host.
    ("64:ff9b:1::", Some("unspecified")),
    ("64:ff9b:1::1", Some("unspecified")),
    ("64:ff9b:1:0:0:1::", Some("unspecified")),
    ("64:ff9b:1:e000::", Some("multicast")),
    ("64:ff9b:1::e000:1", Some("unspecified")),
    // The reserved range 240.0.0.0/4, which holds the broadcast address 255.255.255.255, in
    // each form, and in local-use NAT64 at the /48, /56, /64 and /96 positions.
    ("::ffff:240.0.0.1", Some("private_network")),
    ("::ffff:255.255.255.255", Some("private_network")),
    ("64:ff9b::240.0.0.1", Some("private_network")),
    ("64:ff9b::255.255.255.255", Some("private_network")),
    ("2002:f000:1::", Some("private_network")),
    ("2002:ffff:ffff::", Some("private_network")),
    ("2001:db8::5efe:240.0.0.1", Some("private_network")),
    ("64:ff9b:1:f000:0:100::", Some("private_network")),
    ("64:ff9b:1:f0:0:1::", Some("unspecified")),
    ("64:ff9b:1:0:f0::", Some("unspecified")),
    ("64:ff9b:1::240.0.0.1", Some("unspecified")),
    ("64:ff9b:1:ffff:ffff:ffff:ffff:ffff", Some("private_network")),
    // A position that reads as 240.0.0.0/4 is skipped like a multicast one: 8.8.240.1 after a
    // /48 prefix reads as 240.1.0.0 at the /64 position.
    ("64:ff9b:1:808:f0:100::", None),
    // ISATAP, RFC 5214 section 6.1: the interface identifier 0000:5efe or 0200:5efe carries
    // the IPv4 address under any prefix.
    ("2001:db8::5efe:10.0.0.5", Some("private_network")),
    ("2001:db8::200:5efe:127.0.0.1", Some("loopback")),
    ("2001:db8::5efe:8.8.8.8", None),
    ("2001:db8::200:5efe:8.8.8.8", None),
    // A link-local ISATAP address stays denied as link-local whatever address it carries.
    ("fe80::5efe:8.8.8.8", Some("link_local")),
    ("fe80::200:5efe:10.0.0.5", Some("link_local")),
    // Teredo, RFC 4380 section 4: public server and client addresses.
    ("2001:0:4136:e378:8000:63bf:3fff:fdd2", None),
    // The IPv6 loopback and unspecified addresses keep their IPv6 meaning.
    ("::1", Some("loopback")),
    ("::", Some("unspecified")),
];

#[cfg(test)]
mod tests {
    use super::matcher::CIDR_PARSE_CACHE;
    use super::*;
    use std::net::IpAddr;

    #[test]
    fn test_default_policy() {
        let policy = SsrfPolicy::default();
        assert!(policy.deny_private);
        assert!(policy.allowlist.is_empty());
        assert_eq!(policy.max_redirects, 5);
        assert_eq!(policy.scheme_allowlist, vec!["http", "https"]);
    }

    #[test]
    fn test_host_matcher_exact() {
        let matcher = HostMatcher::exact("example.com");
        assert!(matcher.matches_host("example.com"));
        assert!(matcher.matches_host("EXAMPLE.COM"));
        assert!(!matcher.matches_host("api.example.com"));
    }

    #[test]
    fn test_host_matcher_suffix() {
        let matcher = HostMatcher::suffix(".example.com");
        assert!(matcher.matches_host("api.example.com"));
        assert!(matcher.matches_host("example.com"));
        assert!(matcher.matches_host("API.EXAMPLE.COM"));
        assert!(!matcher.matches_host("notexample.com"));

        let matcher_no_dot = HostMatcher::suffix("example.com");
        assert!(matcher_no_dot.matches_host("example.com"));
        assert!(matcher_no_dot.matches_host("api.example.com"));
        assert!(!matcher_no_dot.matches_host("notexample.com"));
    }

    #[test]
    fn test_host_matcher_cidr_matches_only_ips_in_range() {
        let matcher = HostMatcher::cidr("10.0.0.0/8").expect("literal CIDR is valid");
        assert!(
            matcher.matches_ip(&"10.5.6.7".parse::<IpAddr>().unwrap()),
            "10.5.6.7 is inside 10.0.0.0/8"
        );
        assert!(
            !matcher.matches_ip(&"11.0.0.1".parse::<IpAddr>().unwrap()),
            "11.0.0.1 is outside 10.0.0.0/8"
        );
        assert!(
            !matcher.matches_host("10.0.0.1"),
            "a CIDR matcher is IP-space only and must never match a host string"
        );
    }

    #[test]
    fn cidr_matcher_reuses_cached_parse_across_repeated_matches() {
        // ~keep Proves the perf fix: repeated matches_ip calls on the same CIDR string
        // must not reparse it, and results must stay correct across many calls.
        let cidr = "203.0.113.0/24"; // TEST-NET-3 (RFC 5737); unique to this test.
        let matcher = HostMatcher::cidr(cidr).expect("literal CIDR is valid");
        for _ in 0..50 {
            assert!(
                matcher.matches_ip(&"203.0.113.5".parse::<IpAddr>().unwrap()),
                "203.0.113.5 must match {cidr} on every repeated call"
            );
            assert!(
                !matcher.matches_ip(&"203.0.114.5".parse::<IpAddr>().unwrap()),
                "203.0.114.5 must never match {cidr}"
            );
        }
        let cached = CIDR_PARSE_CACHE
            .read()
            .expect("CIDR_PARSE_CACHE poisoned")
            .get(cidr)
            .cloned();
        assert!(
            matches!(cached, Some(Ok(_))),
            "expected {cidr} to be memoized as a successful parse, got {cached:?}"
        );
    }

    #[test]
    fn cidr_matcher_caches_malformed_cidr_and_keeps_denying() {
        // ~keep Struct-literal bypass of HostMatcher::cidr(), per the ~keep note on
        // matches_ip: a silent false must remain a false, and must stay cached as such.
        let malformed = "not-a-cidr-unique-marker";
        let matcher = HostMatcher::Cidr {
            value: malformed.to_owned(),
        };
        for _ in 0..5 {
            assert!(
                !matcher.matches_ip(&"1.2.3.4".parse::<IpAddr>().unwrap()),
                "a malformed CIDR must never match, even from cache"
            );
        }
        let cached = CIDR_PARSE_CACHE
            .read()
            .expect("CIDR_PARSE_CACHE poisoned")
            .get(malformed)
            .cloned();
        assert!(
            matches!(cached, Some(Err(_))),
            "expected {malformed} to be memoized as a failed parse, got {cached:?}"
        );
    }

    #[test]
    fn host_matcher_cidr_rejects_malformed_block() {
        // ~keep A silent non-match here would be an allowlist hole, not a no-op.
        let err = HostMatcher::cidr("10.0.0.0/99").expect_err("prefix length 99 is not valid");
        assert!(
            matches!(err, SsrfError::InvalidCidr(ref message) if message.contains("10.0.0.0/99")),
            "error must name the offending block, got {err:?}"
        );

        let err = HostMatcher::cidr("not-a-cidr").expect_err("garbage must not parse");
        assert!(
            matches!(err, SsrfError::InvalidCidr(_)),
            "expected InvalidCidr, got {err:?}"
        );
    }

    #[test]
    fn host_matcher_round_trips_through_tagged_json() {
        let cases = [
            (
                HostMatcher::exact("api.example.com"),
                r#"{"type":"exact","value":"api.example.com"}"#,
            ),
            (
                HostMatcher::suffix(".example.com"),
                r#"{"type":"suffix","value":".example.com"}"#,
            ),
            (
                HostMatcher::cidr("10.0.0.0/8").expect("literal CIDR is valid"),
                r#"{"type":"cidr","value":"10.0.0.0/8"}"#,
            ),
        ];

        for (matcher, expected_json) in cases {
            let encoded = serde_json::to_string(&matcher).expect("matcher must serialize");
            assert_eq!(encoded, expected_json, "unexpected wire form for {matcher:?}");

            let decoded: HostMatcher = serde_json::from_str(&encoded).expect("matcher must deserialize");
            assert_eq!(decoded, matcher, "round trip changed the matcher");
        }
    }

    #[test]
    fn host_matcher_accepts_legacy_bare_string_as_exact() {
        // ~keep The pre-tagged representation serialized every variant as a bare string.
        let decoded: HostMatcher = serde_json::from_str(r#""api.example.com""#).expect("legacy form must deserialize");
        assert_eq!(
            decoded,
            HostMatcher::exact("api.example.com"),
            "a bare string must resolve to Exact"
        );
    }

    #[test]
    fn host_matcher_deserialization_rejects_malformed_cidr() {
        let err = serde_json::from_str::<HostMatcher>(r#"{"type":"cidr","value":"10.0.0.0/99"}"#)
            .expect_err("malformed CIDR must not deserialize");
        assert!(
            err.to_string().contains("10.0.0.0/99"),
            "deserialization error must name the offending block, got: {err}"
        );
    }

    #[test]
    fn ssrf_policy_round_trips_a_populated_allowlist() {
        let mut policy = SsrfPolicy::default();
        policy.allowlist.push(HostMatcher::suffix(".internal.example.com"));
        policy
            .allowlist
            .push(HostMatcher::cidr("10.0.0.0/8").expect("literal CIDR is valid"));

        let encoded = serde_json::to_string(&policy).expect("policy must serialize");
        let decoded: SsrfPolicy = serde_json::from_str(&encoded).expect("policy must deserialize");

        assert_eq!(
            decoded.allowlist, policy.allowlist,
            "allowlist must survive a JSON round trip"
        );
    }

    #[test]
    fn test_default_policy_deny_private_true() {
        let policy = SsrfPolicy::default();
        assert!(policy.deny_private);
        assert_eq!(policy.max_redirects, 5);
    }

    #[test]
    fn test_default_policy_scheme_allowlist() {
        let policy = SsrfPolicy::default();
        assert_eq!(policy.scheme_allowlist, vec!["http", "https"]);
    }

    #[test]
    fn test_classify_ipv4_loopback() {
        assert_eq!(
            classify_private_ip(IpAddr::V4("127.0.0.1".parse().unwrap())),
            "loopback"
        );
    }

    #[test]
    fn test_classify_ipv4_private_10() {
        assert_eq!(
            classify_private_ip(IpAddr::V4("10.0.0.1".parse().unwrap())),
            "private_network"
        );
    }

    #[test]
    fn test_classify_ipv4_private_172() {
        assert_eq!(
            classify_private_ip(IpAddr::V4("172.16.0.1".parse().unwrap())),
            "private_network"
        );
    }

    #[test]
    fn test_classify_ipv4_private_192() {
        assert_eq!(
            classify_private_ip(IpAddr::V4("192.168.0.1".parse().unwrap())),
            "private_network"
        );
    }

    #[test]
    fn test_classify_ipv4_link_local() {
        assert_eq!(
            classify_private_ip(IpAddr::V4("169.254.1.1".parse().unwrap())),
            "link_local"
        );
    }

    #[test]
    fn test_classify_ipv6_loopback() {
        assert_eq!(classify_private_ip(IpAddr::V6("::1".parse().unwrap())), "loopback");
    }

    #[test]
    fn test_classify_ipv6_link_local() {
        assert_eq!(
            classify_private_ip(IpAddr::V6("fe80::1".parse().unwrap())),
            "link_local"
        );
    }

    #[test]
    fn test_classify_ipv6_unique_local() {
        assert_eq!(
            classify_private_ip(IpAddr::V6("fc00::1".parse().unwrap())),
            "unique_local"
        );
    }

    #[test]
    fn test_classify_ipv6_multicast() {
        assert_eq!(classify_private_ip(IpAddr::V6("ff00::1".parse().unwrap())), "multicast");
    }

    #[tokio::test]
    async fn validate_url_rejects_loopback_v4() {
        let policy = SsrfPolicy::default();
        let url = "http://127.0.0.1/".parse::<url::Url>().unwrap();
        let err = validate_url(&url, &policy).await.unwrap_err();
        assert!(
            matches!(err, SsrfError::DeniedByPolicy { reason: "loopback" }),
            "expected DeniedByPolicy loopback, got {err:?}"
        );
    }

    #[tokio::test]
    async fn validate_url_rejects_private_10() {
        let policy = SsrfPolicy::default();
        let url = "http://10.0.0.1/".parse::<url::Url>().unwrap();
        let err = validate_url(&url, &policy).await.unwrap_err();
        assert!(
            matches!(
                err,
                SsrfError::DeniedByPolicy {
                    reason: "private_network"
                }
            ),
            "expected DeniedByPolicy private_network, got {err:?}"
        );
    }

    #[tokio::test]
    async fn validate_url_rejects_private_172() {
        let policy = SsrfPolicy::default();
        let url = "http://172.16.0.1/".parse::<url::Url>().unwrap();
        let err = validate_url(&url, &policy).await.unwrap_err();
        assert!(
            matches!(
                err,
                SsrfError::DeniedByPolicy {
                    reason: "private_network"
                }
            ),
            "expected DeniedByPolicy private_network, got {err:?}"
        );
    }

    #[tokio::test]
    async fn validate_url_rejects_private_192() {
        let policy = SsrfPolicy::default();
        let url = "http://192.168.1.1/".parse::<url::Url>().unwrap();
        let err = validate_url(&url, &policy).await.unwrap_err();
        assert!(
            matches!(
                err,
                SsrfError::DeniedByPolicy {
                    reason: "private_network"
                }
            ),
            "expected DeniedByPolicy private_network, got {err:?}"
        );
    }

    #[tokio::test]
    async fn validate_url_rejects_metadata_ip() {
        let policy = SsrfPolicy::default();
        let url = "http://169.254.169.254/".parse::<url::Url>().unwrap();
        let err = validate_url(&url, &policy).await.unwrap_err();
        assert!(
            matches!(err, SsrfError::DeniedByPolicy { reason: "link_local" }),
            "expected DeniedByPolicy link_local, got {err:?}"
        );
    }

    #[tokio::test]
    async fn validate_url_rejects_unspecified() {
        let policy = SsrfPolicy::default();
        let url = "http://0.0.0.0/".parse::<url::Url>().unwrap();
        let err = validate_url(&url, &policy).await.unwrap_err();
        assert!(
            matches!(err, SsrfError::DeniedByPolicy { reason: "unspecified" }),
            "expected DeniedByPolicy unspecified, got {err:?}"
        );
    }

    #[tokio::test]
    async fn validate_url_rejects_ipv6_unspecified() {
        let policy = SsrfPolicy::default();
        let url = "http://[::]/".parse::<url::Url>().unwrap();
        let err = validate_url(&url, &policy).await.unwrap_err();
        assert!(
            matches!(err, SsrfError::DeniedByPolicy { reason: "unspecified" }),
            "expected DeniedByPolicy unspecified for the IPv6 unspecified address, got {err:?}"
        );
    }

    #[tokio::test]
    async fn validate_url_rejects_shared_address_space() {
        let policy = SsrfPolicy::default();
        for host in ["100.100.100.200", "100.64.0.1", "100.127.255.254"] {
            let url = format!("http://{host}/").parse::<url::Url>().unwrap();
            let err = validate_url(&url, &policy)
                .await
                .expect_err("RFC 6598 shared address space must be denied");
            assert!(
                matches!(err, SsrfError::DeniedByPolicy { .. }),
                "expected DeniedByPolicy for RFC 6598 address {host}, got {err:?}"
            );
        }
    }

    #[tokio::test]
    async fn validate_url_permits_public_addresses_adjacent_to_shared_address_space() {
        let policy = SsrfPolicy::default();
        for host in ["100.63.255.255", "100.128.0.1"] {
            let url = format!("http://{host}/").parse::<url::Url>().unwrap();
            validate_url(&url, &policy)
                .await
                .unwrap_or_else(|e| panic!("{host} is outside 100.64.0.0/10 and must be permitted: {e:?}"));
        }
    }

    #[tokio::test]
    async fn validate_url_rejects_the_reserved_range_and_broadcast() {
        let policy = SsrfPolicy::default();
        for host in ["240.0.0.1", "250.1.2.3", "255.255.255.254", "255.255.255.255"] {
            let url = format!("http://{host}/").parse::<url::Url>().unwrap();
            let err = validate_url(&url, &policy)
                .await
                .expect_err("240.0.0.0/4 must be denied");
            assert!(
                matches!(err, SsrfError::DeniedByPolicy { .. }),
                "expected DeniedByPolicy for {host}, got {err:?}"
            );
        }
    }

    #[tokio::test]
    async fn validate_url_rejects_multicast() {
        let policy = SsrfPolicy::default();
        let url = "http://224.0.0.1/".parse::<url::Url>().unwrap();
        let err = validate_url(&url, &policy).await.unwrap_err();
        assert!(
            matches!(err, SsrfError::DeniedByPolicy { reason: "multicast" }),
            "expected DeniedByPolicy multicast, got {err:?}"
        );
    }

    #[tokio::test]
    async fn validate_url_rejects_ipv6_loopback() {
        let policy = SsrfPolicy::default();
        let url = "http://[::1]/".parse::<url::Url>().unwrap();
        let err = validate_url(&url, &policy).await.unwrap_err();
        assert!(
            matches!(err, SsrfError::DeniedByPolicy { reason: "loopback" }),
            "expected DeniedByPolicy loopback, got {err:?}"
        );
    }

    #[tokio::test]
    async fn validate_url_rejects_ipv6_link_local() {
        let policy = SsrfPolicy::default();
        let url = "http://[fe80::1]/".parse::<url::Url>().unwrap();
        let err = validate_url(&url, &policy).await.unwrap_err();
        assert!(
            matches!(err, SsrfError::DeniedByPolicy { reason: "link_local" }),
            "expected DeniedByPolicy link_local, got {err:?}"
        );
    }

    #[tokio::test]
    async fn validate_url_rejects_ipv6_ula() {
        let policy = SsrfPolicy::default();
        let url = "http://[fc00::1]/".parse::<url::Url>().unwrap();
        let err = validate_url(&url, &policy).await.unwrap_err();
        assert!(
            matches!(err, SsrfError::DeniedByPolicy { reason: "unique_local" }),
            "expected DeniedByPolicy unique_local, got {err:?}"
        );
    }

    #[tokio::test]
    async fn validate_url_rejects_ipv6_multicast() {
        let policy = SsrfPolicy::default();
        let url = "http://[ff00::1]/".parse::<url::Url>().unwrap();
        let err = validate_url(&url, &policy).await.unwrap_err();
        assert!(
            matches!(err, SsrfError::DeniedByPolicy { reason: "multicast" }),
            "expected DeniedByPolicy multicast, got {err:?}"
        );
    }

    #[tokio::test]
    async fn validate_url_rejects_disallowed_scheme_ftp() {
        let policy = SsrfPolicy::default();
        let url = "ftp://example.com/".parse::<url::Url>().unwrap();
        let err = validate_url(&url, &policy).await.unwrap_err();
        match err {
            SsrfError::DisallowedScheme(s) => assert_eq!(s, "ftp"),
            other => panic!("expected DisallowedScheme(\"ftp\"), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn validate_url_rejects_unsupported_scheme_even_if_configured() {
        let policy = SsrfPolicy {
            scheme_allowlist: vec!["ftp".to_owned()],
            ..Default::default()
        };
        let url = "ftp://example.com/".parse::<url::Url>().unwrap();

        let error = validate_url(&url, &policy).await.unwrap_err();
        assert!(
            matches!(error, SsrfError::DisallowedScheme(ref scheme) if scheme == "ftp"),
            "unsupported transports must fail closed, got: {error:?}"
        );
    }

    #[tokio::test]
    async fn validate_url_honors_a_configured_supported_subset() {
        let policy = SsrfPolicy {
            scheme_allowlist: vec!["https".to_owned()],
            ..Default::default()
        };
        let url = "http://1.1.1.1/".parse::<url::Url>().unwrap();

        let error = validate_url(&url, &policy).await.unwrap_err();
        assert!(
            matches!(error, SsrfError::DisallowedScheme(ref scheme) if scheme == "http"),
            "an omitted supported scheme must be denied, got: {error:?}"
        );
    }

    #[tokio::test]
    async fn validate_url_rejects_disallowed_scheme_file() {
        let policy = SsrfPolicy::default();
        let url = "file:///etc/passwd".parse::<url::Url>().unwrap();
        let err = validate_url(&url, &policy).await.unwrap_err();
        match err {
            SsrfError::DisallowedScheme(s) => assert_eq!(s, "file"),
            other => panic!("expected DisallowedScheme(\"file\"), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn validate_url_permits_public_ipv4() {
        let policy = SsrfPolicy::default();
        let url = "http://1.1.1.1/".parse::<url::Url>().unwrap();
        validate_url(&url, &policy)
            .await
            .expect("public IPv4 should be permitted");
    }

    #[tokio::test]
    async fn validate_url_permits_public_ipv6() {
        let policy = SsrfPolicy::default();
        let url = "http://[2606:4700:4700::1111]/".parse::<url::Url>().unwrap();
        validate_url(&url, &policy)
            .await
            .expect("public IPv6 should be permitted");
    }

    #[tokio::test]
    async fn validate_url_cidr_allowlist_permits_private() {
        let mut policy = SsrfPolicy::default();
        policy
            .allowlist
            .push(HostMatcher::cidr("10.0.0.0/8").expect("literal CIDR is valid"));
        let url = "http://10.5.6.7/".parse::<url::Url>().unwrap();
        validate_url(&url, &policy)
            .await
            .expect("10.0.0.0/8 in allowlist should permit 10.5.6.7");
    }

    #[tokio::test]
    async fn validate_url_exact_allowlist_does_not_match_literal_ip() {
        // ~keep Exact matchers are hostname-only; CIDR is required for literal IP allowlisting.
        let mut policy = SsrfPolicy::default();
        policy.allowlist.push(HostMatcher::exact("10.0.0.1"));
        let url = "http://10.0.0.1/".parse::<url::Url>().unwrap();
        let err = validate_url(&url, &policy).await.unwrap_err();
        assert!(
            matches!(
                err,
                SsrfError::DeniedByPolicy {
                    reason: "private_network"
                }
            ),
            "Exact matcher must not bypass CIDR-based IP denial; got {err:?}"
        );
    }

    #[test]
    fn validate_url_suffix_no_leading_dot_does_not_match_substring() {
        let matcher = HostMatcher::suffix("example.com");
        assert!(
            !matcher.matches_host("notexample.com"),
            "Suffix(\"example.com\") must not match \"notexample.com\""
        );
    }

    #[tokio::test]
    async fn deny_private_false_permits_everything() {
        let policy = SsrfPolicy {
            deny_private: false,
            ..SsrfPolicy::default()
        };
        let url = "http://127.0.0.1/".parse::<url::Url>().unwrap();
        validate_url(&url, &policy)
            .await
            .expect("deny_private=false must permit loopback");
    }

    #[allow(unsafe_code)]
    #[tokio::test]
    #[serial_test::serial]
    async fn from_env_honors_crawlberg_allow_private_network() {
        // ~keep SAFETY: #[serial] prevents concurrent environment access in this test process.
        unsafe { std::env::set_var("CRAWLBERG_ALLOW_PRIVATE_NETWORK", "true") };
        let policy = SsrfPolicy::from_env();
        unsafe { std::env::remove_var("CRAWLBERG_ALLOW_PRIVATE_NETWORK") };
        let url = "http://10.0.0.1/".parse::<url::Url>().unwrap();
        validate_url(&url, &policy)
            .await
            .expect("CRAWLBERG_ALLOW_PRIVATE_NETWORK=true must permit private IPs");
    }

    #[allow(unsafe_code)]
    #[tokio::test]
    #[serial_test::serial]
    async fn from_env_default_denies() {
        // ~keep SAFETY: #[serial] prevents concurrent environment mutation in this test process.
        unsafe { std::env::remove_var("CRAWLBERG_ALLOW_PRIVATE_NETWORK") };
        let policy = SsrfPolicy::from_env();
        let url = "http://10.0.0.1/".parse::<url::Url>().unwrap();
        let err = validate_url(&url, &policy).await.unwrap_err();
        assert!(
            matches!(
                err,
                SsrfError::DeniedByPolicy {
                    reason: "private_network"
                }
            ),
            "default from_env policy must deny private IPs; got {err:?}"
        );
    }

    /// Regression test for rc.71: the field-level default for an omitted SSRF
    /// policy must honor `CRAWLBERG_ALLOW_PRIVATE_NETWORK`.
    #[allow(unsafe_code)]
    #[test]
    #[serial_test::serial]
    fn crawl_config_json_deserialize_honors_env_var() {
        use crate::types::CrawlConfig;

        // ~keep SAFETY: #[serial] prevents concurrent environment mutation in this test process.
        unsafe { std::env::set_var("CRAWLBERG_ALLOW_PRIVATE_NETWORK", "true") };
        let cfg: CrawlConfig = serde_json::from_str("{}").expect("empty object must deserialize");
        unsafe { std::env::remove_var("CRAWLBERG_ALLOW_PRIVATE_NETWORK") };
        assert!(
            !cfg.ssrf.deny_private,
            "JSON `{{}}` with CRAWLBERG_ALLOW_PRIVATE_NETWORK=true must produce deny_private=false (got deny_private=true)",
        );

        let cfg_default_env: CrawlConfig = serde_json::from_str("{}").expect("empty object must deserialize");
        assert!(
            cfg_default_env.ssrf.deny_private,
            "JSON `{{}}` without env var must produce deny_private=true (got deny_private=false)",
        );
    }

    /// Regression test for rc.77: `SsrfPolicy` JSON deserialization must
    /// tolerate partial input. Bindings whose CrawlConfig DTO emits
    /// `"ssrf": {}` (e.g. Go's `Ssrf SsrfPolicy `json:"ssrf"`` with all
    /// fields `*pointer + omitempty`) previously failed with
    /// `missing field deny_private`. Field-level `#[serde(default)]` lets the
    /// policy fall back to safe defaults when fields are omitted.
    #[test]
    fn ssrf_policy_deserializes_from_empty_object() {
        let policy: SsrfPolicy = serde_json::from_str("{}").expect("empty object must deserialize");
        assert!(policy.deny_private, "deny_private must default to true");
        assert_eq!(policy.max_redirects, 5, "max_redirects must default to 5");
        assert!(policy.allowlist.is_empty(), "allowlist must default to empty");
        assert!(
            policy.scheme_allowlist == vec!["http", "https"],
            "scheme_allowlist must default to http/https"
        );
    }

    /// Companion to `ssrf_policy_deserializes_from_empty_object`: partial input
    /// where some fields are present and others omitted must keep provided
    /// values and default the rest.
    #[test]
    #[serial_test::serial]
    fn ssrf_policy_deserializes_from_partial_object() {
        let policy: SsrfPolicy =
            serde_json::from_str(r#"{"max_redirects": 3}"#).expect("partial object must deserialize");
        assert!(policy.deny_private, "deny_private must default to true");
        assert_eq!(policy.max_redirects, 3, "explicit max_redirects must be honored");

        let policy: SsrfPolicy =
            serde_json::from_str(r#"{"deny_private": false}"#).expect("partial object must deserialize");
        assert!(!policy.deny_private, "explicit deny_private must be honored");
        assert_eq!(policy.max_redirects, 5, "max_redirects must default to 5");
    }

    #[test]
    fn ssrf_policy_json_round_trip_preserves_scheme_allowlist() {
        let policy: SsrfPolicy = serde_json::from_str(r#"{"scheme_allowlist":["https"]}"#)
            .expect("a custom supported subset must deserialize");
        let json = serde_json::to_string(&policy).expect("serialize");
        let restored: SsrfPolicy = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(
            restored.scheme_allowlist,
            vec!["https".to_owned()],
            "configured schemes must survive JSON round-trip"
        );
    }

    #[test]
    fn default_scheme_allowlist_keeps_the_legacy_json_shape() {
        let json = serde_json::to_value(SsrfPolicy::default()).expect("serialize default policy");
        assert_eq!(
            json.get("scheme_allowlist"),
            None,
            "the default scheme allowlist must remain omitted from JSON"
        );
    }

    #[tokio::test]
    async fn validate_url_denies_ipv4_mapped_ipv6_literals() {
        // ~keep Regression: ipnet's contains() only matches within an address family, so
        // ::ffff:127.0.0.1 was tested against the IPv6 nets only and was PERMITTED.
        // A dual-stack host routes it to 127.0.0.1, making it a real loopback bypass.
        for (target, expected_reason) in [
            ("http://[::ffff:127.0.0.1]/", "loopback"),
            ("http://[::ffff:169.254.169.254]/", "link_local"),
            ("http://[::ffff:10.0.0.1]/", "private_network"),
            ("http://[::ffff:192.168.1.1]/", "private_network"),
        ] {
            let url = target.parse::<url::Url>().expect("valid URL");
            let err = validate_url(&url, &SsrfPolicy::default())
                .await
                .expect_err(&format!("{target} must be denied"));
            assert!(
                matches!(err, SsrfError::DeniedByPolicy { reason } if reason == expected_reason),
                "{target} must be denied as {expected_reason}, got {err:?}"
            );
        }
    }

    #[tokio::test]
    async fn validate_url_denies_nat64_embedded_private_addresses() {
        // ~keep RFC 6052 well-known prefix embeds an IPv4 address the same way.
        let url = "http://[64:ff9b::7f00:1]/".parse::<url::Url>().expect("valid URL");
        let err = validate_url(&url, &SsrfPolicy::default())
            .await
            .expect_err("64:ff9b::7f00:1 embeds 127.0.0.1 and must be denied");
        assert!(
            matches!(err, SsrfError::DeniedByPolicy { reason: "loopback" }),
            "expected loopback denial for a NAT64-embedded loopback address, got {err:?}"
        );
    }

    #[tokio::test]
    async fn validate_url_checks_the_ipv4_address_embedded_in_each_ipv6_form() {
        let mut mismatches = Vec::new();
        for &(literal, expected) in EMBEDDED_IPV4_CASES {
            let url = format!("http://[{literal}]/").parse::<url::Url>().expect("valid URL");
            let actual = match validate_url(&url, &SsrfPolicy::default()).await {
                Ok(()) => None,
                Err(SsrfError::DeniedByPolicy { reason }) => Some(reason),
                Err(other) => panic!("{literal}: expected a policy decision, got {other:?}"),
            };
            if actual != expected {
                mismatches.push(format!("{literal}: expected {expected:?}, got {actual:?}"));
            }
        }
        assert!(
            mismatches.is_empty(),
            "policy decisions differ:\n{}",
            mismatches.join("\n")
        );
    }

    #[tokio::test]
    async fn validate_url_checks_embedded_ipv4_under_an_allowlisted_ipv6_prefix() {
        // ~keep An allowlist entry for the local-use NAT64 prefix must not admit the private
        // addresses inside it; an entry for the IPv4 range still does.
        let policy_for = |cidr: &str| SsrfPolicy {
            allowlist: vec![HostMatcher::cidr(cidr).expect("literal CIDR is valid")],
            ..SsrfPolicy::default()
        };
        let url = "http://[64:ff9b:1::10.0.0.5]/".parse::<url::Url>().expect("valid URL");

        let err = validate_url(&url, &policy_for("64:ff9b:1::/48"))
            .await
            .expect_err("the IPv6 prefix entry must not permit an embedded private address");
        assert!(
            matches!(
                err,
                SsrfError::DeniedByPolicy {
                    reason: "private_network"
                }
            ),
            "expected a private_network denial, got {err:?}"
        );
        validate_url(&url, &policy_for("10.0.0.0/8"))
            .await
            .expect("an IPv4 allowlist entry must permit the embedded address");
    }

    #[tokio::test]
    async fn validate_url_still_permits_genuine_public_ipv6() {
        let url = "http://[2606:4700:4700::1111]/".parse::<url::Url>().expect("valid URL");
        validate_url(&url, &SsrfPolicy::default())
            .await
            .expect("a public IPv6 address must remain permitted");
    }
}
