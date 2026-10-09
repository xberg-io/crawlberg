//! The character set of a page: the one a browser uses for its bytes, decided once, before the
//! bytes become text.
//!
//! The order of the sources is the encoding sniffing algorithm of the HTML standard: a byte-order
//! mark, the `charset` of the `Content-Type` header, a declaration in the document (a `<meta>` tag,
//! then an XML declaration), and detection from the bytes when nothing is declared. Labels and
//! decoders come from `encoding_rs`, detection from `chardetng`, and the `<meta>` scan reads the
//! document with html5ever's tokenizer.

use std::cell::{Cell, RefCell};

use encoding_rs::{Encoding, UTF_8, UTF_16BE, UTF_16LE, WINDOWS_1252, X_USER_DEFINED};
use html5ever::tendril::StrTendril;
use html5ever::tokenizer::states::RawKind;
use html5ever::tokenizer::{BufferQueue, Tag, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts};

use super::is_binary_content_type;
use crate::tower::BodyText;

/// The bytes a `<meta>` tag is honoured in wherever it stands. Past them it counts only while
/// the document is still in its head, as Chrome reads it.
const META_SCAN_MINIMUM: usize = 1024;

/// The bytes the `<meta>` scan reads at most.
///
/// ~keep Chrome has no such limit: it scans for as long as the document stays in its head. The
/// ~keep limit keeps the scan of a hostile page that never leaves its head short.
const META_SCAN_LIMIT: usize = 64 * 1024;

/// The character set decided for the bytes of a page.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Charset {
    encoding: &'static Encoding,
    /// What a result reports: the label a declaration used, in lowercase, or the standard name of
    /// the encoding when a byte-order mark or detection decided. `None` when nothing is declared
    /// and the bytes are UTF-8.
    label: Option<String>,
}

impl Charset {
    /// `encoding`, reported by its standard name.
    fn named(encoding: &'static Encoding) -> Self {
        Self {
            encoding,
            label: Some(encoding.name().to_ascii_lowercase()),
        }
    }

    /// The encoding the header label `label` names, reported by that label.
    fn from_header(label: String) -> Option<Self> {
        let encoding = Encoding::for_label(label.as_bytes())?;
        Some(Self {
            encoding,
            label: Some(label),
        })
    }

    /// The encoding a declaration inside the document names with `label`.
    ///
    /// ~keep The bytes were read as ASCII to find the declaration, so they are not UTF-16, and
    /// ~keep the standard reads such a document as UTF-8. It reads `x-user-defined` as
    /// ~keep windows-1252. Both report the encoding used, not the label.
    fn from_document(label: &str) -> Option<Self> {
        let label = label.trim_matches(|c: char| c.is_ascii_whitespace());
        let encoding = Encoding::for_label(label.as_bytes())?;
        if encoding == UTF_16BE || encoding == UTF_16LE {
            Some(Self::named(UTF_8))
        } else if encoding == X_USER_DEFINED {
            Some(Self::named(WINDOWS_1252))
        } else {
            Some(Self {
                encoding,
                label: Some(label.to_ascii_lowercase()),
            })
        }
    }
}

/// The text of a page whose character set is decided here, and the character set to report.
///
/// The text is `None` when the lossy UTF-8 read the caller already holds is the text of the page:
/// the page is UTF-8, or `body` says a browser decoded it. Text a browser decoded is never
/// decoded again; its character set is the one the browser reports.
///
/// ~keep A decided encoding decodes the whole body, with a replacement character for each
/// ~keep sequence it cannot read, as a browser does. Nothing is decided a second time from the
/// ~keep result: a page can hold a real replacement character.
pub(crate) fn decode_page(
    body: &BodyText,
    content_type: &str,
    url: &str,
    body_bytes: &[u8],
) -> (Option<String>, Option<String>) {
    match body {
        BodyText::Decoded { charset } => (None, charset.clone()),
        BodyText::Undecoded => {
            let charset = decide(content_type, url, body_bytes);
            let text = (charset.encoding != UTF_8)
                .then(|| charset.encoding.decode_with_bom_removal(body_bytes).0.into_owned());
            (text, charset.label)
        }
    }
}

/// The text of a document the native browser backend fetched, and the character set to report.
/// It is the decision and the decode [`decode_page`] makes for a response from the server.
#[cfg(feature = "browser-native")]
pub(crate) fn decode_document(content_type: &str, url: &str, body_bytes: &[u8]) -> (String, Option<String>) {
    let (text, charset) = decode_page(&BodyText::Undecoded, content_type, url, body_bytes);
    (
        text.unwrap_or_else(|| String::from_utf8_lossy(body_bytes).into_owned()),
        charset,
    )
}

/// Decide the character set of `body_bytes`, served with `content_type` from `url`.
fn decide(content_type: &str, url: &str, body_bytes: &[u8]) -> Charset {
    if let Some((encoding, _)) = Encoding::for_bom(body_bytes) {
        return Charset::named(encoding);
    }
    if let Some(charset) = header_label(content_type).and_then(Charset::from_header) {
        return charset;
    }
    if let Some(charset) = declared_in_document(body_bytes) {
        return charset;
    }
    // ~keep Undeclared bytes that are UTF-8 are UTF-8, as Chrome reads them. A detector asked
    // ~keep about a binary body would only guess, so a binary body keeps its lossy UTF-8 read.
    if std::str::from_utf8(body_bytes).is_ok() || is_binary_content_type(content_type) {
        return Charset {
            encoding: UTF_8,
            label: None,
        };
    }
    Charset::named(detect(url, body_bytes))
}

/// The `charset` parameter of a `Content-Type` header value, in lowercase.
fn header_label(content_type: &str) -> Option<String> {
    const PARAMETER: &str = "charset=";
    let start = find_ascii_case_insensitive(content_type.as_bytes(), PARAMETER.as_bytes())? + PARAMETER.len();
    let label = content_type
        .get(start..)?
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .trim_matches('"')
        .to_ascii_lowercase();
    (!label.is_empty()).then_some(label)
}

/// The character set the document declares: in UTF-16 by the shape of its XML declaration, in a
/// `<meta>` tag, or in an XML declaration.
fn declared_in_document(body_bytes: &[u8]) -> Option<Charset> {
    if body_bytes.starts_with(b"<\0?\0x\0") {
        return Some(Charset::named(UTF_16LE));
    }
    if body_bytes.starts_with(b"\0<\0?\0x") {
        return Some(Charset::named(UTF_16BE));
    }
    meta_declaration(body_bytes).or_else(|| xml_declaration(body_bytes))
}

/// The encoding an XML declaration at the start of `body_bytes` names.
fn xml_declaration(body_bytes: &[u8]) -> Option<Charset> {
    const NAME: &[u8] = b"encoding";
    if !body_bytes.starts_with(b"<?xml") {
        return None;
    }
    let end = body_bytes.iter().position(|&byte| byte == b'>')?;
    let declaration = &body_bytes[..end];
    let after_name = declaration.windows(NAME.len()).position(|window| window == NAME)? + NAME.len();
    let mut rest = declaration[after_name..].trim_ascii_start();
    rest = rest.strip_prefix(b"=")?.trim_ascii_start();
    let (&quote, value) = rest.split_first()?;
    if quote != b'"' && quote != b'\'' {
        return None;
    }
    let value = &value[..value.iter().position(|&byte| byte == quote)?];
    Charset::from_document(std::str::from_utf8(value).ok()?)
}

/// The character set the first `<meta>` tag that declares one names, read as Chrome reads it:
/// anywhere in the first [`META_SCAN_MINIMUM`] bytes, and past them while the document is still
/// in its head.
fn meta_declaration(body_bytes: &[u8]) -> Option<Charset> {
    let scanned = &body_bytes[..body_bytes.len().min(META_SCAN_LIMIT)];
    let tokenizer = Tokenizer::new(MetaScan::default(), TokenizerOpts::default());
    let input = BufferQueue::default();
    for (index, chunk) in scanned.chunks(META_SCAN_MINIMUM).enumerate() {
        if index > 0 {
            tokenizer.sink.past_minimum.set(true);
        }
        if tokenizer.sink.is_done() {
            break;
        }
        // ~keep The declaration is ASCII in every encoding this scan can find it in, so each
        // ~keep byte is read as one character.
        input.push_back(StrTendril::from(encoding_rs::mem::decode_latin1(chunk).as_ref()));
        let _ = tokenizer.feed(&input);
    }
    tokenizer.sink.found.take()
}

/// A token sink that keeps the character set of the first `<meta>` tag that declares one.
#[derive(Default)]
struct MetaScan {
    found: RefCell<Option<Charset>>,
    /// A tag that cannot stand in a head was read.
    left_head: Cell<bool>,
    /// The first [`META_SCAN_MINIMUM`] bytes are read.
    past_minimum: Cell<bool>,
}

impl MetaScan {
    fn is_done(&self) -> bool {
        self.found.borrow().is_some() || (self.left_head.get() && self.past_minimum.get())
    }
}

impl TokenSink for MetaScan {
    type Handle = ();

    fn process_token(&self, token: Token, _line_number: u64) -> TokenSinkResult<()> {
        let Token::TagToken(tag) = token else {
            return TokenSinkResult::Continue;
        };
        if self.is_done() {
            return TokenSinkResult::Continue;
        }
        let name: &str = &tag.name;
        let is_start = tag.kind == TagKind::StartTag;
        if is_start
            && name == "meta"
            && let Some(charset) = meta_charset(&tag)
        {
            *self.found.borrow_mut() = Some(charset);
            return TokenSinkResult::Continue;
        }
        let stays_in_head = matches!(
            name,
            "script" | "noscript" | "style" | "link" | "meta" | "object" | "title" | "base"
        ) || (is_start && matches!(name, "html" | "head"));
        if !stays_in_head {
            self.left_head.set(true);
        }
        if !is_start {
            return TokenSinkResult::Continue;
        }
        // ~keep The content of these elements is text, so a `<meta>` written inside one is not a tag.
        match name {
            "title" | "textarea" => TokenSinkResult::RawData(RawKind::Rcdata),
            "style" | "xmp" | "iframe" | "noembed" | "noframes" | "noscript" => {
                TokenSinkResult::RawData(RawKind::Rawtext)
            }
            "script" => TokenSinkResult::RawData(RawKind::ScriptData),
            "plaintext" => TokenSinkResult::Plaintext,
            _ => TokenSinkResult::Continue,
        }
    }
}

/// The character set a `<meta>` tag declares: its `charset` attribute, or the `charset` in the
/// `content` of a tag whose `http-equiv` is `Content-Type`.
fn meta_charset(tag: &Tag) -> Option<Charset> {
    let attr = |name: &str| {
        tag.attrs
            .iter()
            .find(|attr| &*attr.name.local == name)
            .map(|attr| &*attr.value)
    };
    if let Some(label) = attr("charset") {
        return Charset::from_document(label);
    }
    if !attr("http-equiv")?.eq_ignore_ascii_case("content-type") {
        return None;
    }
    Charset::from_document(content_charset(attr("content")?)?)
}

/// The label after `charset=` in the `content` of a `<meta http-equiv="Content-Type">` tag.
fn content_charset(content: &str) -> Option<&str> {
    const NAME: &[u8] = b"charset";
    let mut from = 0;
    loop {
        let after_name = from + find_ascii_case_insensitive(&content.as_bytes()[from..], NAME)? + NAME.len();
        from = after_name;
        let rest = content
            .get(after_name..)?
            .trim_start_matches(|c: char| c.is_ascii_whitespace());
        let Some(value) = rest.strip_prefix('=') else {
            continue;
        };
        let value = value.trim_start_matches(|c: char| c.is_ascii_whitespace());
        return match value.chars().next()? {
            quote @ ('"' | '\'') => value[1..].split_once(quote).map(|(label, _)| label),
            _ => value.split(|c: char| c.is_ascii_whitespace() || c == ';').next(),
        };
    }
}

/// Detect the encoding of `body_bytes`, which declare none and are not UTF-8.
///
/// ~keep The top-level domain of `url` is a hint, as it is in Firefox: a short page under `.jp`
/// ~keep is more likely Japanese than a short page under `.de`.
fn detect(url: &str, body_bytes: &[u8]) -> &'static Encoding {
    let mut detector = chardetng::EncodingDetector::new();
    detector.feed(body_bytes, true);
    let top_level_domain = top_level_domain(url);
    detector.guess(top_level_domain.as_deref().map(str::as_bytes), false)
}

/// The last label of the host name of `url`, in lowercase, when the host is a domain name.
fn top_level_domain(url: &str) -> Option<String> {
    let url = url::Url::parse(url).ok()?;
    let label = url
        .domain()?
        .trim_end_matches('.')
        .rsplit('.')
        .next()?
        .to_ascii_lowercase();
    let is_label = !label.is_empty() && label.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-');
    is_label.then_some(label)
}

/// Case-insensitive search for an ASCII needle within a byte slice.
fn find_ascii_case_insensitive(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "http://127.0.0.1/";

    fn decided(content_type: &str, body: &[u8]) -> (&'static str, Option<String>) {
        let charset = decide(content_type, URL, body);
        (charset.encoding.name(), charset.label)
    }

    fn label(text: &str) -> Option<String> {
        Some(text.to_owned())
    }

    #[test]
    fn a_header_label_is_reported_as_it_is_written_in_lowercase() {
        assert_eq!(
            decided("text/html; charset=ISO-8859-1", b"<html>caf\xe9</html>"),
            ("windows-1252", label("iso-8859-1"))
        );
        assert_eq!(
            decided("text/html; charset=\"Shift_JIS\"; x=y", b"<html></html>"),
            ("Shift_JIS", label("shift_jis"))
        );
    }

    #[test]
    fn a_byte_order_mark_outranks_the_header() {
        let mut utf16 = vec![0xFF, 0xFE];
        utf16.extend_from_slice(b"h\0i\0");
        assert_eq!(
            decided("text/html; charset=utf-8", &utf16),
            ("UTF-16LE", label("utf-16le"))
        );
        assert_eq!(
            decided("text/html", &[0xFE, 0xFF, 0, b'h']),
            ("UTF-16BE", label("utf-16be"))
        );
        let mut utf8 = vec![0xEF, 0xBB, 0xBF];
        utf8.extend_from_slice("héllo".as_bytes());
        assert_eq!(
            decided("text/html; charset=iso-8859-1", &utf8),
            ("UTF-8", label("utf-8"))
        );
    }

    #[test]
    fn the_header_outranks_a_meta_tag_and_a_meta_tag_outranks_detection() {
        let body = b"<html><head><meta charset=\"shift_jis\"></head><body>caf\xe9</body></html>";
        assert_eq!(
            decided("text/html; charset=utf-8", body),
            ("UTF-8", label("utf-8")),
            "the header must win over the meta tag"
        );
        assert_eq!(
            decided("text/html", body),
            ("Shift_JIS", label("shift_jis")),
            "the meta tag must win over detection"
        );
        assert_eq!(
            decided("text/html", b"<html><body>caf\xe9 d\xe9j\xe0 vu na\xefve</body></html>"),
            ("windows-1252", label("windows-1252")),
            "with no declaration the bytes decide"
        );
    }

    #[test]
    fn a_label_no_encoding_has_is_passed_over() {
        let body = b"<html><head><meta charset=\"euc-kr\"></head></html>";
        assert_eq!(
            decided("text/html; charset=not-a-charset", body),
            ("EUC-KR", label("euc-kr")),
            "an unknown header label must leave the decision to the meta tag"
        );
        let two = b"<meta charset=\"nonsense\"><meta charset=\"euc-kr\">";
        assert_eq!(
            decided("text/html", two),
            ("EUC-KR", label("euc-kr")),
            "an unknown meta label must leave the decision to the next meta tag"
        );
    }

    #[test]
    fn a_meta_tag_declares_with_charset_or_with_a_content_type_pragma() {
        for (meta, expected) in [
            (r#"<meta charset=x-sjis>"#, ("Shift_JIS", label("x-sjis"))),
            (r#"<META CHARSET=" Latin1 ">"#, ("windows-1252", label("latin1"))),
            (
                r#"<meta http-equiv="Content-Type" content="text/html; charset=euc-kr">"#,
                ("EUC-KR", label("euc-kr")),
            ),
            (
                r#"<meta content="text/html;CHARSET = 'big5' ; x" http-equiv=content-type>"#,
                ("Big5", label("big5")),
            ),
            (
                r#"<meta content="charset charset=gbk" http-equiv="content-type">"#,
                ("GBK", label("gbk")),
            ),
            (r#"<meta charset="utf-16">"#, ("UTF-8", label("utf-8"))),
            (
                r#"<meta charset="x-user-defined">"#,
                ("windows-1252", label("windows-1252")),
            ),
        ] {
            assert_eq!(decided("text/html", meta.as_bytes()), expected, "for {meta}");
        }
    }

    #[test]
    fn text_that_only_looks_like_a_declaration_declares_nothing() {
        for body in [
            "<!-- <meta charset=shift_jis> -->",
            "<!-- charset=shift_jis -->",
            "<title><meta charset=shift_jis></title>",
            "<script>var a = '<meta charset=shift_jis>';</script>",
            "<style><meta charset=shift_jis></style>",
            "<p>charset=shift_jis</p>",
            "<meta name=\"description\" content=\"charset=shift_jis\">",
            "<meta http-equiv=\"refresh\" content=\"charset=shift_jis\">",
            "</meta charset=shift_jis>",
        ] {
            assert_eq!(decided("text/html", body.as_bytes()), ("UTF-8", None), "for {body}");
        }
    }

    #[test]
    fn a_meta_tag_past_the_first_kilobyte_counts_only_in_the_head() {
        let padding = "x".repeat(2 * META_SCAN_MINIMUM);
        let in_head = format!("<html><head><!-- {padding} --><title>t</title><meta charset=shift_jis></head>");
        assert_eq!(
            decided("text/html", in_head.as_bytes()),
            ("Shift_JIS", label("shift_jis"))
        );
        let in_body = format!("<html><head></head><body><p>{padding}</p><meta charset=shift_jis>");
        assert_eq!(decided("text/html", in_body.as_bytes()), ("UTF-8", None));
        let early_in_body = "<html><body><p>text</p><meta charset=shift_jis>";
        assert_eq!(
            decided("text/html", early_in_body.as_bytes()),
            ("Shift_JIS", label("shift_jis")),
            "in the first kilobyte a meta tag counts wherever it stands"
        );
        let past_the_limit = format!(
            "<html><head><!-- {} --><meta charset=shift_jis>",
            "x".repeat(META_SCAN_LIMIT)
        );
        assert_eq!(decided("text/html", past_the_limit.as_bytes()), ("UTF-8", None));
    }

    #[test]
    fn an_xml_declaration_declares_when_no_meta_tag_does() {
        assert_eq!(
            decided(
                "text/html",
                b"<?xml version=\"1.0\" encoding=\"Shift_JIS\"?><html></html>"
            ),
            ("Shift_JIS", label("shift_jis"))
        );
        assert_eq!(
            decided("text/html", b"<?xml version='1.0' encoding = 'euc-kr' ?><html></html>"),
            ("EUC-KR", label("euc-kr"))
        );
        assert_eq!(
            decided(
                "text/html",
                b"<?xml version=\"1.0\" encoding=\"euc-kr\"?><meta charset=shift_jis>"
            ),
            ("Shift_JIS", label("shift_jis")),
            "a meta tag must win over the XML declaration"
        );
        for body in [
            &b"<?xml version=\"1.0\"?><html></html>"[..],
            b"<?xml encoding=\"euc-kr\"",
            b"<?xml encoding=euc-kr?>",
            b"<?xml encoding=\"euc-kr?>",
            b" <?xml encoding=\"euc-kr\"?>",
        ] {
            assert_eq!(
                decided("text/html", body),
                ("UTF-8", None),
                "for {:?}",
                String::from_utf8_lossy(body)
            );
        }
    }

    #[test]
    fn a_utf_16_xml_declaration_declares_utf_16() {
        assert_eq!(
            decided("text/html", b"<\0?\0x\0m\0l\0"),
            ("UTF-16LE", label("utf-16le"))
        );
        assert_eq!(
            decided("text/html", b"\0<\0?\0x\0m\0l"),
            ("UTF-16BE", label("utf-16be"))
        );
    }

    #[test]
    fn undeclared_bytes_are_utf_8_when_they_are_valid_utf_8() {
        assert_eq!(
            decided("text/html", b"<html><body>hello</body></html>"),
            ("UTF-8", None)
        );
        assert_eq!(decided("text/html", "<p>café 日本語</p>".as_bytes()), ("UTF-8", None));
        assert_eq!(decided("", b""), ("UTF-8", None));
    }

    #[test]
    fn a_binary_body_is_not_put_to_the_detector() {
        // ~keep "%PDF" and then four bytes that are not UTF-8.
        let body = [0x25, 0x50, 0x44, 0x46, 0xE9, 0xFF, 0x00, 0x93];
        assert_eq!(decided("application/pdf", &body), ("UTF-8", None));
        assert_ne!(
            decided("text/html", &body).1,
            None,
            "the same bytes served as a page must go to the detector"
        );
    }

    #[test]
    fn the_top_level_domain_is_the_last_label_of_a_domain_name() {
        assert_eq!(top_level_domain("https://www.example.co.jp/a"), label("jp"));
        assert_eq!(top_level_domain("https://EXAMPLE.DE./"), label("de"));
        assert_eq!(top_level_domain("http://127.0.0.1:8080/"), None);
        assert_eq!(top_level_domain("http://[::1]/"), None);
        assert_eq!(top_level_domain("not a url"), None);
    }

    #[test]
    fn text_a_browser_decoded_is_not_decoded_again() {
        let text = "<html><head><meta charset=\"iso-8859-1\"></head><body>café</body></html>";
        let browser = BodyText::Decoded {
            charset: label("windows-1252"),
        };
        assert_eq!(
            decode_page(&browser, "text/html", URL, text.as_bytes()),
            (None, label("windows-1252")),
            "the text of a browser must stay as it is, with the browser's character set"
        );
        let (text, charset) = decode_page(&BodyText::Undecoded, "text/html", URL, text.as_bytes());
        assert_eq!(charset, label("iso-8859-1"));
        assert!(
            text.is_some_and(|text| text.contains("cafÃ©")),
            "the same bytes from a server are decoded as their meta tag says"
        );
    }

    #[test]
    fn a_decided_encoding_decodes_past_a_sequence_it_cannot_read() {
        // ~keep Shift_JIS for "日本" and then the first byte of a two-byte character, alone.
        let body = [0x93, 0xFA, 0x96, 0x7B, 0x93];
        let (text, charset) = decode_page(&BodyText::Undecoded, "text/html; charset=shift_jis", URL, &body);
        assert_eq!(text.as_deref(), Some("日本\u{FFFD}"));
        assert_eq!(charset, label("shift_jis"));
    }

    #[test]
    fn a_declared_encoding_decodes_its_bytes_exactly() {
        // ~keep Windows-1252 for "café €100" and Shift_JIS for "日本語 テスト".
        for (content_type, bytes, expected) in [
            (
                "text/html; charset=windows-1252",
                &[0x63, 0x61, 0x66, 0xE9, 0x20, 0x80, 0x31, 0x30, 0x30][..],
                "café €100",
            ),
            (
                "text/html; charset=shift_jis",
                &[
                    0x93, 0xFA, 0x96, 0x7B, 0x8C, 0xEA, 0x20, 0x83, 0x65, 0x83, 0x58, 0x83, 0x67,
                ],
                "日本語 テスト",
            ),
        ] {
            let (text, _) = decode_page(&BodyText::Undecoded, content_type, URL, bytes);
            assert_eq!(text.as_deref(), Some(expected), "for {content_type}");
        }
    }

    #[test]
    fn a_utf_8_page_keeps_the_text_the_caller_holds() {
        let (text, charset) = decode_page(&BodyText::Undecoded, "text/html; charset=utf-8", URL, b"caf\xe9");
        assert_eq!((text, charset), (None, label("utf-8")));
        let (text, charset) = decode_page(
            &BodyText::Undecoded,
            "text/html",
            URL,
            b"caf\xe9 d\xe9j\xe0 vu na\xefve",
        );
        assert_eq!(text.as_deref(), Some("café déjà vu naïve"));
        assert_eq!(charset, label("windows-1252"));
    }
}
