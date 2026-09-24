//! Response-body reading: the size cap, bounded chunked reads, and charset decoding.

use crate::types::CrawlConfig;

/// Decode `bytes` as UTF-8, moving the buffer directly into the returned `String`
/// with no copy when it is already valid UTF-8. Falls back to lossy replacement —
/// byte-identical to `String::from_utf8_lossy(&bytes).into_owned()` — when it is not.
///
/// ~keep `String::from_utf8_lossy(&bytes).into_owned()` always allocates a fresh
/// buffer and copies into it, even on the (common) valid-UTF-8 path where the bytes
/// could simply become the `String`'s own buffer. `String::from_utf8` validates and,
/// on success, moves `bytes` in with no copy; on failure it hands the original bytes
/// back via `FromUtf8Error::into_bytes`, so the lossy fallback reuses them instead of
/// cloning. Only call this where the caller does not also need to keep `bytes` as a
/// separate `Vec<u8>` afterward — callers that also need the raw bytes (e.g. for
/// `body_bytes`) still need two independently owned buffers and gain nothing from
/// moving one into the other.
pub(crate) fn decode_body_lossy(bytes: Vec<u8>) -> String {
    match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => String::from_utf8_lossy(&e.into_bytes()).into_owned(),
    }
}

/// Truncate `body` to at most `max_size` bytes without splitting a UTF-8 character.
///
/// `String::truncate` panics when the index is not a char boundary. `max_size` comes
/// from `CrawlConfig::max_body_size`, so on any non-ASCII page an unlucky byte count
/// would otherwise panic the crawl.
pub(crate) fn truncate_body_at_char_boundary(body: &mut String, max_size: usize) {
    if body.len() <= max_size {
        return;
    }
    let mut boundary = max_size;
    while boundary > 0 && !body.is_char_boundary(boundary) {
        boundary -= 1;
    }
    body.truncate(boundary);
}

/// Re-decode `body_bytes` using `charset` when it names a recognized non-UTF-8 encoding.
///
/// ~keep Shared by the scrape path (`scrape.rs`) and the crawl path
/// (`engine/crawl_loop.rs`) so both apply the charset `detect_charset` reports instead
/// of only reporting it. `resp.body`/`fetch.body` is always a `String::from_utf8_lossy`
/// decode of the raw bytes (see `read_body_bounded` callers in this file and in
/// `tower/service.rs`); for any non-UTF-8/us-ascii charset that lossy decode has
/// already replaced every non-ASCII byte with U+FFFD, so callers must re-decode from
/// `body_bytes` rather than post-process the lossy string.
///
/// Returns `None` — meaning "keep the caller's existing lossy-UTF-8 body" — when
/// `charset` is `"utf-8"`/`"us-ascii"`, is not a label `encoding_rs` recognizes, or
/// decoding hit unmappable sequences (an unreliable decode is worse than the lossy
/// fallback, which at least round-trips the ASCII-safe portion of the page).
pub(crate) fn redecode_with_charset(charset: &str, body_bytes: &[u8]) -> Option<String> {
    if charset == "utf-8" || charset == "us-ascii" {
        return None;
    }
    let encoding = encoding_rs::Encoding::for_label(charset.as_bytes())?;
    let (decoded, _, had_errors) = encoding.decode(body_bytes);
    if had_errors { None } else { Some(decoded.into_owned()) }
}

/// Safety ceiling on a response body when `max_body_size` is unset.
///
/// ~keep reqwest is built with gzip and brotli, and `Response::chunk` yields
/// *decompressed* bytes, so an unset cap let a few hundred compressed bytes expand to
/// gigabytes in memory. 100 MiB sits far above any real HTML page while still bounding
/// the process; the document path applies its own, smaller
/// [`crate::document::DEFAULT_DOCUMENT_MAX_SIZE`] before this is reached.
pub(crate) const DEFAULT_MAX_BODY_SIZE: usize = 100 * 1024 * 1024;

/// The body cap actually enforced for `config`.
///
/// ~keep Resolved here rather than in `CrawlConfig::default` so it cannot be bypassed:
/// a config deserialized from JSON, or built by a language binding that omits the field,
/// gets `None` for the field regardless of what `Default` says. Every fetch path routes
/// through this, so the ceiling holds for all of them. A caller who genuinely wants an
/// unbounded read opts in explicitly with a large `max_body_size`.
pub(crate) fn effective_max_body_size(config: &CrawlConfig) -> Option<usize> {
    Some(config.max_body_size.unwrap_or(DEFAULT_MAX_BODY_SIZE))
}

/// Read a response body in bounded chunks, stopping once more than `max_size` bytes
/// have been received. Returns the bytes read together with whether the read stopped
/// early because the cap was hit (as opposed to a natural end-of-body).
///
/// ~keep `resp.bytes()` buffers the *entire* body — including whatever reqwest's
/// transparent gzip/brotli decompression produces — before `max_body_size` truncation
/// ever runs downstream, so a decompression bomb (e.g. a 10 GB gzip response behind a
/// 1 MB cap) still allocates its full decompressed size. Reading chunk-by-chunk via
/// `Response::chunk` (available without the `stream` cargo feature, unlike
/// `bytes_stream`) and stopping as soon as the cap is crossed bounds peak memory to
/// roughly `max_size` plus one chunk width, regardless of the declared or true
/// decompressed size — the cap is enforced while reading, not after the fact.
///
/// When `max_size` is `None`, reads to completion exactly as `resp.bytes()` would.
///
/// Takes `resp` by value: every call site reads the body as its last operation on the
/// response before returning or moving on to the next redirect hop, and the native
/// implementation needs `&mut` access to `Response::chunk` internally.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) async fn read_body_bounded(
    mut resp: reqwest::Response,
    max_size: Option<usize>,
) -> Result<(Vec<u8>, bool), reqwest::Error> {
    let mut buf: Vec<u8> = Vec::with_capacity(max_size.unwrap_or(8192).min(1 << 20));
    while let Some(chunk) = resp.chunk().await? {
        buf.extend_from_slice(&chunk);
        if let Some(max_size) = max_size
            && buf.len() > max_size
        {
            return Ok((buf, true));
        }
    }
    Ok((buf, false))
}

/// wasm32 fallback: the browser-`fetch`-backed `reqwest::Response` on this target does
/// not expose `Response::chunk` (only `bytes()`, which reads to completion, or
/// `bytes_stream()`, which requires the `stream` cargo feature this crate does not
/// enable). The memory-exhaustion threat this bounds — a malicious server streaming an
/// unbounded decompression bomb at a long-running native crawler process — does not
/// apply the same way inside a browser's sandboxed wasm runtime, so this reads to
/// completion and always reports `hit_cap = false`; callers still apply
/// `max_body_size` truncation to the result afterwards, matching prior wasm behavior.
#[cfg(target_arch = "wasm32")]
pub(crate) async fn read_body_bounded(
    resp: reqwest::Response,
    _max_size: Option<usize>,
) -> Result<(Vec<u8>, bool), reqwest::Error> {
    Ok((resp.bytes().await?.to_vec(), false))
}

/// Read a response body via [`read_body_bounded`] and lossily decode it as UTF-8,
/// returning an empty string on any read error.
///
/// Used by WAF-classification paths that only need best-effort body text and already
/// tolerate a missing body (they previously used `resp.text().await.unwrap_or_default()`,
/// which has the same unbounded-memory problem `read_body_bounded` fixes).
pub(crate) async fn read_text_bounded(resp: reqwest::Response, max_size: Option<usize>) -> String {
    match read_body_bounded(resp, max_size).await {
        Ok((bytes, _)) => decode_body_lossy(bytes),
        Err(_) => String::new(),
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::super::build_client;
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn decode_body_lossy_matches_from_utf8_lossy_for_valid_utf8() {
        let bytes = "héllo wörld".as_bytes().to_vec();
        let expected = String::from_utf8_lossy(&bytes).into_owned();
        assert_eq!(
            decode_body_lossy(bytes.clone()),
            expected,
            "the move fast-path must produce the same String as the lossy path for valid UTF-8"
        );
    }

    #[test]
    fn decode_body_lossy_is_byte_identical_to_from_utf8_lossy_for_invalid_utf8() {
        // ~keep Deliberately invalid UTF-8: a lone continuation byte (0x80) followed by a
        // truncated 2-byte sequence (0xC3 with no continuation), surrounded by valid ASCII.
        // Regression target: `decode_body_lossy`'s fallback must replicate
        // `String::from_utf8_lossy`'s replacement behavior exactly, not just avoid panicking.
        let bytes: Vec<u8> = vec![b'a', b'b', 0x80, b'c', 0xC3, b'd', b'e'];
        let expected = String::from_utf8_lossy(&bytes).into_owned();
        let actual = decode_body_lossy(bytes.clone());
        assert_eq!(
            actual, expected,
            "invalid UTF-8 must decode identically to String::from_utf8_lossy, got {actual:?} vs {expected:?}"
        );
        assert!(
            actual.contains('\u{FFFD}'),
            "the invalid bytes must be replaced with U+FFFD, got {actual:?}"
        );
    }

    #[test]
    fn truncate_body_never_splits_a_utf8_character() {
        // ~keep Regression: String::truncate panics on a non-char-boundary index, and
        // max_body_size is user-supplied, so any non-ASCII page could panic the crawl.
        let original = "héllo wörld";
        for max_size in 0..=original.len() {
            let mut body = original.to_string();
            truncate_body_at_char_boundary(&mut body, max_size);
            assert!(
                body.len() <= max_size,
                "truncation to {max_size} produced {} bytes",
                body.len()
            );
            assert!(
                original.starts_with(&body),
                "truncation to {max_size} must yield a prefix, got {body:?}"
            );
        }
    }

    #[test]
    fn truncate_body_leaves_short_bodies_untouched() {
        let mut body = "abc".to_string();
        truncate_body_at_char_boundary(&mut body, 100);
        assert_eq!(body, "abc", "a body under the limit must not be modified");
    }

    #[test]
    fn redecode_with_charset_decodes_windows_1252_bytes_exactly() {
        // ~keep Real Windows-1252 bytes for "café €100" (verified via Python's `str.encode`).
        // A UTF-8-lossy decode of these bytes would replace 0xE9 and 0x80 with U+FFFD.
        let bytes: &[u8] = &[0x63, 0x61, 0x66, 0xE9, 0x20, 0x80, 0x31, 0x30, 0x30];
        let decoded = redecode_with_charset("windows-1252", bytes);
        assert_eq!(
            decoded,
            Some("café €100".to_owned()),
            "windows-1252 bytes must decode to the exact original string, got {decoded:?}"
        );
    }

    #[test]
    fn redecode_with_charset_decodes_shift_jis_bytes_exactly() {
        // ~keep Real Shift_JIS bytes for "日本語 テスト" (verified via Python's `str.encode`).
        let bytes: &[u8] = &[
            0x93, 0xFA, 0x96, 0x7B, 0x8C, 0xEA, 0x20, 0x83, 0x65, 0x83, 0x58, 0x83, 0x67,
        ];
        let decoded = redecode_with_charset("shift_jis", bytes);
        assert_eq!(
            decoded,
            Some("日本語 テスト".to_owned()),
            "shift_jis bytes must decode to the exact original string, got {decoded:?}"
        );
    }

    #[test]
    fn redecode_with_charset_is_a_noop_for_utf8() {
        let decoded = redecode_with_charset("utf-8", "hello".as_bytes());
        assert_eq!(
            decoded, None,
            "utf-8 must be a no-op (caller keeps its existing lossy body), got {decoded:?}"
        );
    }

    #[test]
    fn redecode_with_charset_returns_none_for_unrecognized_label() {
        let decoded = redecode_with_charset("not-a-real-charset", b"hello");
        assert_eq!(
            decoded, None,
            "an unrecognized charset label must not panic and must return None, got {decoded:?}"
        );
    }

    #[tokio::test]
    async fn read_body_bounded_stops_reading_once_max_size_is_exceeded() {
        let mock = MockServer::start().await;

        // ~keep A ~5 MB response with max_size(1024) proves the reader stops early —
        // simulates the "declared/actual size exceeds the cap" scenario without
        // needing a real decompression bomb for this specific unit test.
        let full_size = 5 * 1024 * 1024;
        Mock::given(method("GET"))
            .and(path("/big"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'a'; full_size]))
            .mount(&mock)
            .await;

        let mut config = CrawlConfig::default();
        config.ssrf.deny_private = false;
        let client = build_client(&config).expect("client must build");
        let url = format!("{}/big", mock.uri());
        let resp = client.get(&url).send().await.expect("request must succeed");

        let max_size = 1024usize;
        let (bytes, hit_cap) = read_body_bounded(resp, Some(max_size))
            .await
            .expect("bounded read must not error");

        assert!(hit_cap, "hit_cap must be true once the body exceeds max_size");
        assert!(
            bytes.len() < full_size / 100,
            "bounded read must stop far short of the full {full_size}-byte body, got {} bytes",
            bytes.len()
        );
        assert!(
            bytes.len() > max_size,
            "the chunk that crosses the cap should still be included, got {} bytes",
            bytes.len()
        );
    }

    /// Security regression: a gzip decompression bomb (~5 MB decompressed from a few
    /// hundred compressed bytes) behind a small `max_body_size` must not allocate
    /// anywhere near its full decompressed size. `read_body_bounded` reads
    /// `Response::chunk`-by-chunk (decompressed by reqwest's transparent gzip layer as
    /// it streams) and stops as soon as the cap is crossed, rather than buffering the
    /// entire decompressed body via `resp.bytes()` before any cap is applied.
    #[tokio::test]
    async fn read_body_bounded_caps_a_decompression_bomb() {
        let mock = MockServer::start().await;

        let decompressed_size = 5 * 1024 * 1024;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        std::io::Write::write_all(&mut encoder, &vec![b'a'; decompressed_size]).expect("gzip write must succeed");
        let compressed = encoder.finish().expect("gzip finish must succeed");
        assert!(
            compressed.len() < 10_000,
            "test fixture must actually compress well (highly repetitive input), got {} bytes",
            compressed.len()
        );

        Mock::given(method("GET"))
            .and(path("/bomb"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-encoding", "gzip")
                    .set_body_raw(compressed, "application/octet-stream"),
            )
            .mount(&mock)
            .await;

        let mut config = CrawlConfig::default();
        config.ssrf.deny_private = false;
        let client = build_client(&config).expect("client must build");
        let url = format!("{}/bomb", mock.uri());
        let resp = client.get(&url).send().await.expect("request must succeed");

        let max_size = 1024usize;
        let (bytes, hit_cap) = read_body_bounded(resp, Some(max_size))
            .await
            .expect("bounded read must not error");

        assert!(
            hit_cap,
            "hit_cap must be true once the decompressed body exceeds max_size"
        );
        assert!(
            bytes.len() < decompressed_size / 100,
            "bounded read must stop far short of the bomb's full decompressed size \
             ({decompressed_size} bytes), got {} bytes",
            bytes.len()
        );
    }

    #[test]
    fn an_unset_body_cap_resolves_to_the_safety_ceiling() {
        let config = CrawlConfig {
            max_body_size: None,
            ..CrawlConfig::default()
        };
        assert_eq!(
            effective_max_body_size(&config),
            Some(DEFAULT_MAX_BODY_SIZE),
            "an unset cap must resolve to the ceiling, not to an unbounded read"
        );
    }

    #[test]
    fn an_explicit_body_cap_is_passed_through_untouched() {
        let config = CrawlConfig {
            max_body_size: Some(4096),
            ..CrawlConfig::default()
        };
        assert_eq!(
            effective_max_body_size(&config),
            Some(4096),
            "an explicit cap must win over the ceiling, in both directions"
        );

        let unbounded_by_opt_in = CrawlConfig {
            max_body_size: Some(DEFAULT_MAX_BODY_SIZE * 4),
            ..CrawlConfig::default()
        };
        assert_eq!(
            effective_max_body_size(&unbounded_by_opt_in),
            Some(DEFAULT_MAX_BODY_SIZE * 4),
            "raising the cap above the ceiling is the documented opt-in for large bodies"
        );
    }
}
