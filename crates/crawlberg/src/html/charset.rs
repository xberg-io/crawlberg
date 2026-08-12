//! Character encoding detection from Content-Type headers, byte-order marks, and HTML meta tags.

/// Detect the character encoding from the Content-Type header, a byte-order mark, or
/// an ASCII-safe scan of the body for a `<meta charset=...>` declaration.
///
/// ~keep Operates on `body_bytes` — the response body *before* any UTF-8 decoding — not a
/// lossily-decoded `&str`. A UTF-16 byte-order mark is two bytes that are not valid
/// UTF-8 on their own, so `String::from_utf8_lossy` replaces it with U+FFFD before any
/// text-based sniffing could see it; scanning raw bytes keeps it intact. ASCII-pattern
/// search on raw bytes is safe for arbitrary encodings too: `charset=`'s bytes never
/// occur as a spurious match inside a multi-byte sequence of any encoding this crate
/// targets, so no char-boundary bookkeeping is needed.
pub(crate) fn detect_charset(content_type: &str, body_bytes: &[u8]) -> Option<String> {
    // ~keep BOM outranks the transport layer, per the WHATWG encoding sniffing algorithm.
    // ~keep Checking the header first silently corrupted BOM-tagged bodies whenever the two
    // ~keep disagreed: a stale `charset=utf-8` header on a real UTF-16 body made
    // ~keep `redecode_with_charset` short-circuit on "utf-8" and keep the lossy decode, so
    // ~keep every non-ASCII character became U+FFFD with no error anywhere.
    if let Some(charset) = detect_bom_charset(body_bytes) {
        return Some(charset);
    }

    if let Some(pos) = ascii_find_case_insensitive(content_type.as_bytes(), b"charset=") {
        let charset = content_type[pos + 8..]
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .trim_matches('"')
            .to_lowercase();
        if !charset.is_empty() {
            return Some(charset);
        }
    }

    let search_len = body_bytes.len().min(2048);
    let head = &body_bytes[..search_len];
    if let Some(pos) = ascii_find_case_insensitive(head, b"charset=") {
        let charset = extract_meta_charset_value(&head[pos + 8..]);
        if !charset.is_empty() {
            return Some(charset);
        }
    }

    None
}

/// Detect a recognized byte-order mark at the start of `body_bytes`.
///
/// UTF-8's BOM is checked here too (not just in the old lossy-body fallback) so BOM
/// detection is a single, authoritative, raw-byte pass rather than two paths that can
/// disagree.
fn detect_bom_charset(body_bytes: &[u8]) -> Option<String> {
    if body_bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        Some("utf-8".to_owned())
    } else if body_bytes.starts_with(&[0xFF, 0xFE]) {
        Some("utf-16le".to_owned())
    } else if body_bytes.starts_with(&[0xFE, 0xFF]) {
        Some("utf-16be".to_owned())
    } else {
        None
    }
}

/// Extract the charset label following a `charset=` match in a `<meta>` tag, mirroring
/// the historical `&str`-based implementation but operating on raw bytes.
fn extract_meta_charset_value(after: &[u8]) -> String {
    let after = match after.first() {
        Some(b'"' | b'\'') => &after[1..],
        _ => after,
    };
    let end = after
        .iter()
        .position(|&b| matches!(b, b'"' | b'\'' | b'>' | b';') || b.is_ascii_whitespace())
        .unwrap_or(after.len());
    String::from_utf8_lossy(&after[..end]).trim().to_lowercase()
}

/// Case-insensitive search for an ASCII needle within a byte slice.
fn ascii_find_case_insensitive(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    let needle_lower: Vec<u8> = needle.iter().map(|b| b.to_ascii_lowercase()).collect();
    'outer: for i in 0..=(haystack.len() - needle.len()) {
        for (j, &nb) in needle_lower.iter().enumerate() {
            if haystack[i + j].to_ascii_lowercase() != nb {
                continue 'outer;
            }
        }
        return Some(i);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_charset_from_content_type_header() {
        let result = detect_charset("text/html; charset=ISO-8859-1", b"<html></html>");
        assert_eq!(
            result,
            Some("iso-8859-1".to_owned()),
            "content-type charset must be lowercased and extracted, got {result:?}"
        );
    }

    #[test]
    fn detects_charset_from_meta_tag_when_header_absent() {
        let body = br#"<html><head><meta charset="shift_jis"></head></html>"#;
        let result = detect_charset("text/html", body);
        assert_eq!(
            result,
            Some("shift_jis".to_owned()),
            "meta charset attribute must be detected, got {result:?}"
        );
    }

    #[test]
    fn detects_utf16le_bom_on_raw_bytes() {
        // ~keep Regression: a lossy-decoded &str destroys the FF FE marker before
        // detection could see it (it is not valid UTF-8), so this must run on raw bytes.
        let mut body = vec![0xFF, 0xFE];
        body.extend_from_slice(b"h\0e\0l\0l\0o\0");
        let result = detect_charset("text/html", &body);
        assert_eq!(
            result,
            Some("utf-16le".to_owned()),
            "UTF-16LE BOM must be detected from raw bytes, got {result:?}"
        );
    }

    #[test]
    fn detects_utf16be_bom_on_raw_bytes() {
        let mut body = vec![0xFE, 0xFF];
        body.extend_from_slice(b"\0h\0e\0l\0l\0o");
        let result = detect_charset("text/html", &body);
        assert_eq!(
            result,
            Some("utf-16be".to_owned()),
            "UTF-16BE BOM must be detected from raw bytes, got {result:?}"
        );
    }

    #[test]
    fn detects_utf8_bom() {
        let mut body = vec![0xEF, 0xBB, 0xBF];
        body.extend_from_slice(b"<html></html>");
        let result = detect_charset("text/html", &body);
        assert_eq!(
            result,
            Some("utf-8".to_owned()),
            "UTF-8 BOM must be detected, got {result:?}"
        );
    }

    #[test]
    fn bom_outranks_a_contradicting_content_type_header() {
        // ~keep Regression: the header used to win, so a stale `charset=utf-8` on a real
        // UTF-16 body made `redecode_with_charset` short-circuit and keep the lossy decode —
        // every non-ASCII character silently became U+FFFD.
        let mut body = vec![0xFF, 0xFE];
        body.extend_from_slice(b"h\0e\0l\0l\0o\0");
        let result = detect_charset("text/html; charset=utf-8", &body);
        assert_eq!(
            result,
            Some("utf-16le".to_owned()),
            "the BOM must outrank a contradicting Content-Type charset, got {result:?}"
        );
    }

    #[test]
    fn bom_outranks_a_contradicting_legacy_content_type_header() {
        let mut body = vec![0xEF, 0xBB, 0xBF];
        body.extend_from_slice("héllo".as_bytes());
        let result = detect_charset("text/html; charset=iso-8859-1", &body);
        assert_eq!(
            result,
            Some("utf-8".to_owned()),
            "a UTF-8 BOM must outrank a contradicting Content-Type charset, got {result:?}"
        );
    }

    #[test]
    fn content_type_charset_still_wins_when_no_bom_is_present() {
        let result = detect_charset("text/html; charset=iso-8859-1", b"<html>caf\xe9</html>");
        assert_eq!(
            result,
            Some("iso-8859-1".to_owned()),
            "without a BOM the transport-layer charset is authoritative, got {result:?}"
        );
    }

    #[test]
    fn returns_none_when_no_charset_signal_present() {
        let result = detect_charset("text/html", b"<html><body>hello</body></html>");
        assert_eq!(result, None, "no charset signal must yield None, got {result:?}");
    }
}
