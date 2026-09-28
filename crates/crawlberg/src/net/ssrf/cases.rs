//! The shared table of IPv6 literals that embed an IPv4 address.

/// IPv6 literals that embed an IPv4 address, each paired with the denial reason the
/// default policy must report, or `None` when the address must stay permitted.
///
/// Shared by the pre-connect check, the connect-time resolver and the browser parity
/// test, so all three are held to one table.
///
/// ~keep A row marked `GUARD` decides exactly the same way on the release before this change,
/// so it proves nothing about it; it is there to catch a regression in behaviour that already
/// worked. Do not count a GUARD row as coverage of the fix. The split was measured rather than
/// assumed, by running this table against the previous extraction (IPv4-mapped and NAT64
/// well-known only): 21 of the 50 rows failed, and they are precisely the 21 that carry no
/// `GUARD`. No row failed the other way, so nothing this table permits was newly refused.
pub(crate) const EMBEDDED_IPV4_CASES: &[(&str, Option<&str>)] = &[
    // IPv4-mapped, RFC 4291 section 2.5.5.2. Already unwrapped before this change.
    ("::ffff:127.0.0.1", Some("loopback")),         // GUARD
    ("::ffff:10.0.0.5", Some("private_network")),   // GUARD
    ("::ffff:169.254.169.254", Some("link_local")), // GUARD
    ("::ffff:8.8.8.8", None),                       // GUARD
    // IPv4-compatible, RFC 4291 section 2.5.5.1.
    ("::127.0.0.1", Some("loopback")),
    ("::10.0.0.5", Some("private_network")),
    ("::169.254.169.254", Some("link_local")),
    ("::8.8.8.8", None), // GUARD
    // IPv4-translated, RFC 2765 section 2.1.
    ("::ffff:0:127.0.0.1", Some("loopback")),
    ("::ffff:0:10.0.0.5", Some("private_network")),
    ("::ffff:0:169.254.169.254", Some("link_local")),
    ("::ffff:0:8.8.8.8", None), // GUARD
    // NAT64 well-known prefix, RFC 6052 section 2.1. Already unwrapped before this change.
    ("64:ff9b::127.0.0.1", Some("loopback")),         // GUARD
    ("64:ff9b::10.0.0.5", Some("private_network")),   // GUARD
    ("64:ff9b::169.254.169.254", Some("link_local")), // GUARD
    ("64:ff9b::8.8.8.8", None),                       // GUARD
    // 6to4, RFC 3056 section 2: the IPv4 address sits in bits 16 to 47.
    ("2002:7f00:1::", Some("loopback")),
    ("2002:a00:5::", Some("private_network")),
    ("2002:a9fe:a9fe::", Some("link_local")),
    ("2002:808:808::", None), // GUARD
    // Local-use NAT64 prefix, RFC 8215, read at the /96 position: the last 32 bits.
    ("64:ff9b:1::10.0.0.5", Some("private_network")),
    ("64:ff9b:1::127.0.0.1", Some("loopback")),
    ("64:ff9b:1::169.254.169.254", Some("link_local")),
    ("64:ff9b:1::8.8.8.8", None), // GUARD
    // A /96 network whose prefix bytes are not zero. 64:ff9b:1:a00::/96 once had every
    // destination refused, because a /48 reading of its prefix is 10.0.0.0.
    ("64:ff9b:1:a00::808:808", None), // GUARD
    ("64:ff9b:1:a00::10.0.0.5", Some("private_network")),
    ("64:ff9b:1:a00::a9fe:a9fe", Some("link_local")),
    // 8.8.8.8 on a /48, /56 and /64 network, and 8.8.8.10 and 8.8.8.230 on a /64 network. The
    // /96 position reads their unused bits as 0.0.0.0, 10.0.0.0 or 230.0.0.0, which is padding
    // rather than a destination, so each stays permitted.
    ("64:ff9b:1:808:8:800::", None),    // GUARD
    ("64:ff9b:1:8:8:808::", None),      // GUARD
    ("64:ff9b:1:0:8:808:800:0", None),  // GUARD
    ("64:ff9b:1:0:8:808:a00:0", None),  // GUARD
    ("64:ff9b:1:0:8:808:e600:0", None), // GUARD
    // ~keep A /48, /56 or /64 network is not decoded, so a private destination on one is
    // checked as IPv6 only and permitted, as it was before: 10.0.0.5 after a /48 prefix.
    // Reading those positions refused every destination on some /96 networks instead.
    ("64:ff9b:1:a00:0:500::", None), // GUARD
    // With bytes 6 to 11 all zero every prefix length reads the same address, so the /96
    // reading stands even when it looks like padding.
    ("64:ff9b:1::", Some("unspecified")),
    ("64:ff9b:1::1", Some("unspecified")),
    ("64:ff9b:1::e000:1", Some("multicast")),
    // Teredo, RFC 4380 section 4: the client address is stored inverted in the last 32 bits.
    // 5601:5601 inverts to 169.254.169.254 and f5ff:fffa to 10.0.0.5.
    ("2001:0:4136:e378:0:ffff:5601:5601", Some("link_local")),
    ("2001:0:4136:e378:8000:ffff:f5ff:fffa", Some("private_network")),
    // 63bf:3fff:fdd2 inverts to 192.0.2.45, which no deny row covers; 2001:db8::/32 is not
    // Teredo, because only 2001:0::/32 is.
    ("2001:0:4136:e378:8000:63bf:3fff:fdd2", None), // GUARD
    ("2001:db8::1", None),                          // GUARD
    // ISATAP, RFC 5214 section 6.1: the interface identifier 0000:5efe or 0200:5efe carries
    // the IPv4 address under any prefix.
    ("2001:db8::5efe:10.0.0.5", Some("private_network")),
    ("2001:db8::200:5efe:127.0.0.1", Some("loopback")),
    ("2001:db8::5efe:8.8.8.8", None),     // GUARD
    ("2001:db8::200:5efe:8.8.8.8", None), // GUARD
    // A link-local ISATAP address stays denied as link-local whatever address it carries;
    // fe80::/10 already refused both of these.
    ("fe80::5efe:8.8.8.8", Some("link_local")),      // GUARD
    ("fe80::200:5efe:10.0.0.5", Some("link_local")), // GUARD
    // fe80::/10 and fc00::/7 are ranges, not their first hextet. These rows embed nothing;
    // they are here so the browser parity test compares the reason for them too.
    ("feaa::1", Some("link_local")),   // GUARD
    ("fd12::1", Some("unique_local")), // GUARD
    // The IPv6 loopback and unspecified addresses keep their IPv6 meaning.
    ("::1", Some("loopback")),   // GUARD
    ("::", Some("unspecified")), // GUARD
];
