//! The character set of a page: the one a browser uses for its bytes, decided once, before the
//! bytes become text.
//!
//! The order of the sources is the encoding sniffing algorithm of the HTML standard: a byte-order
//! mark, the `charset` of the `Content-Type` header, a declaration in the document (a `<meta>` tag,
//! then an XML declaration), and detection from the bytes when nothing is declared. Labels and
//! decoders come from `encoding_rs`, detection from `chardetng`, and the `<meta>` scan reads the
//! document with html5ever's tokenizer.
//!
//! Where the decision differs from Chrome, on purpose:
//!
//! - Undeclared bytes that are UTF-8 are read as UTF-8. Chrome reads an undeclared page as
//!   windows-1252 even when it is UTF-8, which loses every non-ASCII letter of the page.
//! - Undeclared bytes that are UTF-8 except for a few sequences are read as UTF-8 too, with a
//!   replacement character for each bad sequence. A body cut by a size limit ends inside a
//!   sequence, and one bad byte must not turn a whole page into wrong letters.
//! - UTF-16 with no byte-order mark and no XML declaration is not recognized. Chrome does not
//!   recognize it either; both read it as a single-byte encoding and the page is unreadable.
//! - The report names the label a declaration used (`iso-8859-1`, `iso-2022-kr`, `utf-16`), where
//!   `document.characterSet` names the encoding the label stands for.
//! - `None` is reported for an undeclared page that is ASCII or UTF-8, and for a binary body,
//!   where Chrome names the encoding it fell back to.
//! - The detector names `koi8-u` where Chrome names `koi8-r`. The two decode Russian text to the
//!   same letters.
//! - The `<meta>` scan reads at most [`META_SCAN_LIMIT`] bytes of a body, and the detector at
//!   most [`DETECTION_LIMIT`]. Chrome reads on while the document is in its head.

use std::cell::{Cell, RefCell};

use encoding_rs::{Encoding, UTF_8, UTF_16BE, UTF_16LE, WINDOWS_1252, X_USER_DEFINED};
use html5ever::tendril::StrTendril;
use html5ever::tokenizer::states::RawKind;
use html5ever::tokenizer::{BufferQueue, Tag, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts};

use super::is_binary_content_type;
use crate::tower::BodyText;

/// The bytes a `<meta>` tag is honoured in wherever it stands. Past them it counts only while
/// the document is still in its head.
///
/// ~keep The standard asks for a scan of the first 1024 bytes and lets a browser read further.
/// ~keep Chrome reads on for as long as the document stays in its head, and so does this scan,
/// ~keep up to [`META_SCAN_LIMIT`].
const META_SCAN_MINIMUM: usize = 1024;

/// The bytes of a body the `<meta>` scan reads at most.
///
/// ~keep A head has no end the scan can rely on. A server chooses the size of a body, and a
/// ~keep head, a comment or a tag that never closes holds the scan for all of it, at 30 to 75
/// ~keep milliseconds for each megabyte.
const META_SCAN_LIMIT: usize = 1024 * 1024;

/// The bytes the `<meta>` scan feeds to the tokenizer at a time once it is past
/// [`META_SCAN_MINIMUM`] with the document still in its head.
const META_SCAN_CHUNK: usize = 4096;

/// The bytes of a body the detector reads at most.
///
/// ~keep The detector costs about a tenth of a second for each megabyte, and a server chooses
/// ~keep the size of a body. A megabyte of text is enough to tell one encoding from another.
const DETECTION_LIMIT: usize = 1024 * 1024;

/// How many UTF-8 sequences of more than one byte a body must hold for each sequence that is not
/// UTF-8, to be read as UTF-8.
///
/// ~keep Text in a legacy encoding read as UTF-8 is mostly bad sequences: about four in five for
/// ~keep EUC-KR, nearly all for windows-1252. UTF-8 with a stray byte is nearly all good ones.
const UTF_8_GOOD_FOR_EACH_BAD: usize = 4;

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

/// What a `Content-Type` says a body is, for the declarations a browser reads in it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// `text/html`, or no content type at all. A `<meta>` tag or an XML declaration can declare
    /// the character set.
    Html,
    /// An XML type, `application/xhtml+xml` among them. Only an XML declaration can declare it:
    /// Chrome reads no `<meta>` tag in a document it parses as XML.
    Xml,
    /// JSON, which is UTF-8 by RFC 8259. Chrome reads it as UTF-8 too.
    Json,
    /// Any other text: plain text, CSS, CSV. Nothing in it declares a character set, so a
    /// `<meta>` tag written in it is text, as it is for Chrome.
    Text,
    /// A type that is not text. It keeps its lossy UTF-8 read: a detector would only guess.
    Binary,
}

impl Kind {
    fn of(content_type: &str) -> Self {
        let essence = content_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if is_binary_content_type(content_type) {
            Self::Binary
        } else if essence.is_empty() || essence == "text/html" {
            Self::Html
        } else if essence == "application/json" || essence == "text/json" || essence.ends_with("+json") {
            Self::Json
        } else if essence == "text/xml" || essence == "application/xml" || essence.ends_with("+xml") {
            Self::Xml
        } else {
            Self::Text
        }
    }
}

/// Decide the character set of `body_bytes`, served with `content_type` from `url`.
fn decide(content_type: &str, url: &str, body_bytes: &[u8]) -> Charset {
    if let Some((encoding, _)) = Encoding::for_bom(body_bytes) {
        return Charset::named(encoding);
    }
    if let Some(charset) = header_label(content_type).and_then(Charset::from_header) {
        return charset;
    }
    let kind = Kind::of(content_type);
    let declared = match kind {
        Kind::Html => declared_in_html(body_bytes),
        Kind::Xml => utf_16_xml_declaration(body_bytes).or_else(|| xml_declaration(body_bytes)),
        Kind::Json | Kind::Text | Kind::Binary => None,
    };
    if let Some(charset) = declared {
        return charset;
    }
    if matches!(kind, Kind::Json | Kind::Binary) || is_utf_8_but_for_a_few_sequences(body_bytes) {
        return Charset {
            encoding: UTF_8,
            label: None,
        };
    }
    Charset::named(detect(url, body_bytes))
}

/// Whether `body_bytes` are UTF-8, or UTF-8 but for a few sequences.
///
/// ~keep The detector cannot make this call: its UTF-8 candidate is out at the first sequence
/// ~keep that is not UTF-8 (`chardetng` 0.1.17, `Utf8Candidate::feed`), so one bad byte in a
/// ~keep UTF-8 page made it answer with a legacy encoding for the whole page.
/// ~keep A sequence that the end of the body cuts short is not a bad sequence: a body read up to
/// ~keep a size limit ends wherever the limit falls.
fn is_utf_8_but_for_a_few_sequences(body_bytes: &[u8]) -> bool {
    let is_lead = |byte: &&u8| **byte >= 0xC0;
    let (mut good, mut bad) = (0usize, 0usize);
    let mut rest = body_bytes;
    loop {
        match std::str::from_utf8(rest) {
            Ok(_) => {
                good += rest.iter().filter(is_lead).count();
                break;
            }
            Err(error) => {
                let (valid, after) = rest.split_at(error.valid_up_to());
                good += valid.iter().filter(is_lead).count();
                let Some(length) = error.error_len() else {
                    break;
                };
                bad += 1;
                rest = &after[length..];
            }
        }
    }
    bad == 0 || bad * UTF_8_GOOD_FOR_EACH_BAD <= good
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

/// The character set an HTML document declares: in UTF-16 by the shape of its XML declaration, in
/// a `<meta>` tag, or in an XML declaration.
fn declared_in_html(body_bytes: &[u8]) -> Option<Charset> {
    utf_16_xml_declaration(body_bytes)
        .or_else(|| meta_declaration(body_bytes))
        .or_else(|| xml_declaration(body_bytes))
}

/// UTF-16, when `body_bytes` start with `<?x` written in UTF-16.
fn utf_16_xml_declaration(body_bytes: &[u8]) -> Option<Charset> {
    if body_bytes.starts_with(b"<\0?\0x\0") {
        Some(Charset::named(UTF_16LE))
    } else if body_bytes.starts_with(b"\0<\0?\0x") {
        Some(Charset::named(UTF_16BE))
    } else {
        None
    }
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
/// a tag that starts in the first [`META_SCAN_MINIMUM`] bytes wherever it stands, and a later
/// tag while the document is still in its head, up to [`META_SCAN_LIMIT`].
fn meta_declaration(body_bytes: &[u8]) -> Option<Charset> {
    let body_bytes = &body_bytes[..body_bytes.len().min(META_SCAN_LIMIT)];
    let tokenizer = Tokenizer::new(MetaScan::default(), TokenizerOpts::default());
    let scan = &tokenizer.sink;
    let input = BufferQueue::default();
    let mut offset = 0;
    while offset < body_bytes.len() && !scan.is_done() {
        // ~keep One byte at a time while the place of a token decides: up to the minimum, and
        // ~keep after the document left its head. The sink then knows where each token ends.
        let step = if offset < META_SCAN_MINIMUM || scan.left_head.get() {
            1
        } else {
            META_SCAN_CHUNK
        };
        let end = body_bytes.len().min(offset + step);
        // ~keep The declaration is ASCII in every encoding this scan can find it in, so each
        // ~keep byte is read as one character.
        input.push_back(StrTendril::from(
            encoding_rs::mem::decode_latin1(&body_bytes[offset..end]).as_ref(),
        ));
        scan.fed.set(end);
        let _ = tokenizer.feed(&input);
        offset = end;
    }
    scan.found.take()
}

/// A token sink that keeps the character set of the first `<meta>` tag that declares one.
///
/// ~keep Chrome checks after each token whether to stop: it stops when the document has left its
/// ~keep head and [`META_SCAN_MINIMUM`] bytes are read. So the token that crosses the minimum is
/// ~keep still read, and a `<meta>` tag that starts before the minimum counts.
#[derive(Default)]
struct MetaScan {
    found: RefCell<Option<Charset>>,
    /// A tag that cannot stand in a head was read.
    left_head: Cell<bool>,
    /// The bytes given to the tokenizer so far. A token ends at or before this offset.
    fed: Cell<usize>,
    /// Where the last token ended.
    last_token_end: Cell<usize>,
}

impl MetaScan {
    fn is_done(&self) -> bool {
        self.found.borrow().is_some() || self.is_past_the_end()
    }

    /// Whether the document has left its head and the last token ended past the minimum.
    fn is_past_the_end(&self) -> bool {
        self.left_head.get() && self.last_token_end.get() >= META_SCAN_MINIMUM
    }
}

impl TokenSink for MetaScan {
    type Handle = ();

    fn process_token(&self, token: Token, _line_number: u64) -> TokenSinkResult<()> {
        if matches!(token, Token::ParseError(_) | Token::EOFToken) || self.is_done() {
            return TokenSinkResult::Continue;
        }
        self.last_token_end.set(self.fed.get());
        let Token::TagToken(tag) = token else {
            return TokenSinkResult::Continue;
        };
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
        // ~keep The content of these elements is text, so a `<meta>` written inside one is not a
        // ~keep tag. `<noscript>` is not one of them: Chrome reads a `<meta>` tag inside it.
        match name {
            "title" | "textarea" => TokenSinkResult::RawData(RawKind::Rcdata),
            "style" | "xmp" | "iframe" | "noembed" | "noframes" => TokenSinkResult::RawData(RawKind::Rawtext),
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

/// Detect the encoding of `body_bytes`, which declare none and are not UTF-8, from their first
/// [`DETECTION_LIMIT`] bytes.
///
/// ~keep The top-level domain of `url` is a hint, as it is in Firefox: a short page under `.jp`
/// ~keep is more likely Japanese than a short page under `.de`.
///
/// ~keep At the end of its input the detector drops every encoding that the input ends inside
/// ~keep a character of. A body read up to a size limit ends wherever the limit falls. So the
/// ~keep guess from before the end stands when its encoding reads all but a last character
/// ~keep that is cut short, and the detector makes the same guess for the body without it.
fn detect(url: &str, body_bytes: &[u8]) -> &'static Encoding {
    let sample = &body_bytes[..body_bytes.len().min(DETECTION_LIMIT)];
    let top_level_domain = top_level_domain(url);
    let hint = top_level_domain.as_deref().map(str::as_bytes);
    let mut detector = chardetng::EncodingDetector::new();
    detector.feed(sample, false);
    let unfinished = detector.guess(hint, false);
    if sample.len() < body_bytes.len() {
        return unfinished;
    }
    detector.feed(&[], true);
    let finished = detector.guess(hint, false);
    let stands = finished != unfinished
        && before_a_cut_character(unfinished, sample).is_some_and(|complete| {
            let mut detector = chardetng::EncodingDetector::new();
            detector.feed(complete, true);
            detector.guess(hint, false) == unfinished
        });
    if stands { unfinished } else { finished }
}

/// `bytes` without their last character, when they are text in `encoding` but for a last
/// character that their end cuts short.
fn before_a_cut_character<'a>(encoding: &'static Encoding, bytes: &'a [u8]) -> Option<&'a [u8]> {
    let reads = |bytes: &[u8]| {
        encoding
            .decode_without_bom_handling_and_without_replacement(bytes)
            .is_some()
    };
    if reads(bytes) {
        return None;
    }
    (1..=3)
        .filter_map(|cut| bytes.len().checked_sub(cut))
        .map(|end| &bytes[..end])
        .find(|complete| reads(complete))
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
            (
                r#"<meta content="text/html; charset=gbk;x=y" http-equiv="content-type">"#,
                ("GBK", label("gbk")),
            ),
            (
                r#"<meta content="text/html; charset=gbk x" http-equiv="content-type">"#,
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
            "<plaintext><meta charset=shift_jis>",
            "<xmp><meta charset=shift_jis></xmp>",
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
        let far_in_head = format!(
            "<html><head><style>{}</style><meta charset=shift_jis>",
            "x".repeat(70_000)
        );
        assert_eq!(
            decided("text/html", far_in_head.as_bytes()),
            ("Shift_JIS", label("shift_jis")),
            "the scan must go on for as long as the document is in its head"
        );
    }

    /// A page that leaves its head with `tag` and has `<meta charset=shift_jis>` at `offset`.
    fn meta_at(tag: &str, offset: usize) -> Vec<u8> {
        let mut page = format!("<html><head></head>{tag}");
        assert!(page.len() <= offset, "the tag must end before the meta tag starts");
        page.push_str(&"x".repeat(offset - page.len()));
        page.push_str("<meta charset=shift_jis>");
        page.into_bytes()
    }

    #[test]
    fn out_of_the_head_a_meta_tag_counts_when_it_starts_in_the_first_kilobyte() {
        assert_eq!(
            decided("text/html", &meta_at("<body><p>", META_SCAN_MINIMUM - 9)),
            ("Shift_JIS", label("shift_jis")),
            "a tag that starts before the minimum and ends past it must count"
        );
        assert_eq!(
            decided("text/html", &meta_at("<body><p>", META_SCAN_MINIMUM - 1)),
            ("Shift_JIS", label("shift_jis")),
            "a tag that starts at the last byte before the minimum must count"
        );
        assert_eq!(
            decided("text/html", &meta_at("<body><p>", META_SCAN_MINIMUM)),
            ("UTF-8", None),
            "a tag that starts at the minimum must not count"
        );
        assert_eq!(decided("text/html", &meta_at("<body><p>", 3000)), ("UTF-8", None));
    }

    #[test]
    fn an_end_tag_of_the_head_ends_the_head() {
        for end_tag in ["</head>", "</html>"] {
            let mut page = format!("<html><head>{end_tag}<!-- {} -->", "x".repeat(2 * META_SCAN_MINIMUM)).into_bytes();
            page.extend_from_slice(b"<meta charset=shift_jis>");
            assert_eq!(decided("text/html", &page), ("UTF-8", None), "for {end_tag}");
        }
        let open_head = format!(
            "<html><head><!-- {} --><meta charset=shift_jis>",
            "x".repeat(2 * META_SCAN_MINIMUM)
        );
        assert_eq!(
            decided("text/html", open_head.as_bytes()),
            ("Shift_JIS", label("shift_jis")),
            "the same page with no end tag is still in its head"
        );
    }

    #[test]
    fn a_meta_tag_in_noscript_declares_as_it_does_in_chrome() {
        let page = "<html><head><noscript><meta charset=shift_jis></noscript></head>";
        assert_eq!(decided("text/html", page.as_bytes()), ("Shift_JIS", label("shift_jis")));
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
        assert_eq!(top_level_domain("https://example.xn--p1ai/"), label("xn--p1ai"));
        assert_eq!(top_level_domain("https://example.a1/"), label("a1"));
        assert_eq!(top_level_domain("https://example.a_b/"), None);
        assert_eq!(top_level_domain("https://localhost./"), label("localhost"));
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

    /// UTF-8 text with accents, long enough to hold many sequences of more than one byte.
    const UTF_8_TEXT: &str = "café déjà vu naïve über straße, crème brûlée and smörgåsbord for señor Ångström";

    #[test]
    fn undeclared_utf_8_with_one_bad_byte_is_still_utf_8() {
        let mut page = format!("<p>{UTF_8_TEXT}</p>").into_bytes();
        page.insert(10, 0xE9);
        assert!(
            std::str::from_utf8(&page).is_err(),
            "the fixture must not be valid UTF-8"
        );
        assert_eq!(decided("text/html", &page), ("UTF-8", None));
        assert_eq!(decided("text/plain", &page), ("UTF-8", None));
        assert_eq!(decided("", &page), ("UTF-8", None));
        let (text, charset) = decode_page(&BodyText::Undecoded, "text/html", URL, &page);
        assert_eq!(
            (text, charset),
            (None, None),
            "the caller's lossy UTF-8 read is the text"
        );
    }

    #[test]
    fn a_sequence_cut_by_the_end_of_the_body_is_no_evidence_against_utf_8() {
        // ~keep One accent, so nothing but the cut could speak against UTF-8.
        for text in ["<p>abc d\u{e9}", "<p>日本語のテキスト"] {
            let bytes = text.as_bytes();
            for cut in 1..=2 {
                let page = &bytes[..bytes.len() - cut];
                if std::str::from_utf8(page).is_ok() {
                    continue;
                }
                assert_eq!(decided("text/html", page), ("UTF-8", None), "for {text:?} cut by {cut}");
            }
        }
        // ~keep The first byte of a two-byte sequence, alone at the end.
        let alone = b"<p>abc d\xc3";
        assert_eq!(decided("text/html", alone), ("UTF-8", None));
    }

    #[test]
    fn a_legacy_page_is_not_taken_for_utf_8_with_bad_bytes() {
        let korean = encoding_rs::EUC_KR
            .encode("한국어로 쓴 웹 페이지입니다. 문자 인코딩 판정을 확인합니다.")
            .0
            .into_owned();
        assert_eq!(decided("text/html", &korean), ("EUC-KR", label("euc-kr")));
        // ~keep Latin-1 with one pair of bytes that happens to be a UTF-8 sequence ("Ã©").
        let latin = b"caf\xe9 d\xe9j\xe0 vu na\xefve \xc3\xa9 \xfcber stra\xdfe";
        assert_eq!(decided("text/html", latin), ("windows-1252", label("windows-1252")));
        let one_letter = b"<p>caf\xe9</p>";
        assert_eq!(
            decided("text/html", one_letter),
            ("windows-1252", label("windows-1252"))
        );
    }

    /// `before` and `after` two-byte UTF-8 sequences around `bad` bytes that are never UTF-8.
    fn utf_8_with_bad_bytes(before: usize, bad: usize, after: usize) -> Vec<u8> {
        let mut page = "\u{e9}".repeat(before).into_bytes();
        for _ in 0..bad {
            page.extend_from_slice(b"\xffx");
        }
        page.extend_from_slice("\u{e9}".repeat(after).as_bytes());
        page
    }

    #[test]
    fn utf_8_needs_four_good_sequences_for_each_bad_one() {
        for (before, bad, after, expected) in [
            (4, 1, 0, true),
            (0, 1, 4, true),
            (2, 1, 2, true),
            (3, 1, 0, false),
            (0, 1, 3, false),
            (4, 2, 4, true),
            (4, 2, 3, false),
            (3, 2, 4, false),
            (0, 1, 0, false),
            (5, 0, 0, true),
        ] {
            let page = utf_8_with_bad_bytes(before, bad, after);
            assert_eq!(
                is_utf_8_but_for_a_few_sequences(&page),
                expected,
                "for {before} good sequences, {bad} bad bytes, {after} good sequences"
            );
            assert_eq!(
                decided("text/plain", &page).0 == "UTF-8",
                expected,
                "the decision must follow the rule for {before}, {bad}, {after}"
            );
        }
    }

    #[test]
    fn a_legacy_page_with_a_few_pairs_that_are_utf_8_is_not_utf_8() {
        // ~keep Windows-1252 with three pairs that happen to be UTF-8 sequences ("Ã©", "Â°",
        // ~keep "Ã¼") beside eight letters that are not.
        let latin = b"caf\xe9 d\xe9j\xe0 vu \xc3\xa9 na\xefve 20\xc2\xb0 \xfcber stra\xdfe \xc3\xbc cr\xe8me se\xf1or";
        assert!(!is_utf_8_but_for_a_few_sequences(latin));
        assert_eq!(decided("text/html", latin), ("windows-1252", label("windows-1252")));
    }

    #[test]
    fn nothing_is_read_after_the_scan_is_done() {
        // ~keep Past the first kilobyte of a head the scan reads in chunks, so the tags below
        // ~keep reach the tokenizer together.
        let padding = "x".repeat(2 * META_SCAN_MINIMUM);
        let two = format!("<html><head><!-- {padding} --><meta charset=shift_jis><meta charset=euc-kr>");
        assert_eq!(
            decided("text/html", two.as_bytes()),
            ("Shift_JIS", label("shift_jis")),
            "the first declaration must stay"
        );
        let after_the_head = format!("<html><head><!-- {padding} --></head><body><meta charset=shift_jis>");
        assert_eq!(
            decided("text/html", after_the_head.as_bytes()),
            ("UTF-8", None),
            "a tag after the end of the head must not count past the first kilobyte"
        );
    }

    #[test]
    fn json_is_utf_8_whatever_it_holds() {
        let cut = "{\"text\": \"日本語のテキスト".as_bytes();
        let cut = &cut[..cut.len() - 1];
        assert!(
            std::str::from_utf8(cut).is_err(),
            "the fixture must end inside a sequence"
        );
        let with_meta = "{\"html\": \"<meta charset=windows-1251> café\"}".as_bytes();
        let latin = b"{\"text\": \"caf\xe9 d\xe9j\xe0 vu na\xefve\"}";
        for content_type in [
            "application/json",
            "Application/JSON; x=y",
            "application/ld+json",
            "text/json",
        ] {
            for body in [cut, with_meta, &latin[..]] {
                assert_eq!(decided(content_type, body), ("UTF-8", None), "for {content_type}");
            }
        }
        assert_eq!(
            decided("application/json; charset=iso-8859-1", latin),
            ("windows-1252", label("iso-8859-1")),
            "a header charset still decides"
        );
    }

    #[test]
    fn only_html_is_scanned_for_a_meta_tag() {
        let page = "<meta charset=\"windows-1251\"> café".as_bytes();
        assert_eq!(decided("text/html", page), ("windows-1251", label("windows-1251")));
        assert_eq!(decided("", page), ("windows-1251", label("windows-1251")));
        for content_type in [
            "text/plain",
            "text/css",
            "text/csv",
            "application/xhtml+xml",
            "application/rss+xml",
        ] {
            assert_eq!(decided(content_type, page), ("UTF-8", None), "for {content_type}");
        }
        let latin = b"caf\xe9 d\xe9j\xe0 vu na\xefve";
        assert_eq!(
            decided("text/plain", latin),
            ("windows-1252", label("windows-1252")),
            "plain text with no declaration still goes to the detector"
        );
    }

    #[test]
    fn an_xml_type_reads_its_xml_declaration() {
        let declared = b"<?xml version=\"1.0\" encoding=\"iso-8859-1\"?><rss>caf\xe9</rss>";
        for content_type in [
            "text/xml",
            "application/xml",
            "application/rss+xml",
            "application/xhtml+xml",
        ] {
            assert_eq!(
                decided(content_type, declared),
                ("windows-1252", label("iso-8859-1")),
                "for {content_type}"
            );
        }
        assert_eq!(decided("text/plain", declared), ("windows-1252", label("windows-1252")));
        assert_eq!(decided("text/xml", b"<\0?\0x\0m\0l\0"), ("UTF-16LE", label("utf-16le")));
    }

    #[test]
    fn a_byte_order_mark_is_not_part_of_the_text() {
        let mut page = vec![0xFF, 0xFE];
        page.extend("<p>h\u{e9}</p>".encode_utf16().flat_map(u16::to_le_bytes));
        let (text, charset) = decode_page(&BodyText::Undecoded, "text/html", URL, &page);
        assert_eq!(text.as_deref(), Some("<p>h\u{e9}</p>"));
        assert_eq!(charset, label("utf-16le"));
    }

    #[test]
    fn the_domain_of_the_page_is_a_hint_for_the_detector() {
        // ~keep Half-width katakana in Shift_JIS: one byte each, and letters in windows-1252 too.
        let bytes = encoding_rs::SHIFT_JIS.encode("ﾊﾝｶｸｶﾀｶﾅ ﾃｽﾄ ﾍﾟｰｼﾞ").0.into_owned();
        let japanese = decide("text/html", "https://example.co.jp/page", &bytes);
        let german = decide("text/html", "https://example.de/page", &bytes);
        assert_eq!(japanese.encoding.name(), "Shift_JIS");
        assert_ne!(
            german.encoding.name(),
            "Shift_JIS",
            "the same bytes under .de must read differently"
        );
    }

    #[test]
    fn the_detector_reads_the_start_of_a_long_body() {
        let mut page = vec![b' '; DETECTION_LIMIT];
        page.extend_from_slice(
            &encoding_rs::SHIFT_JIS
                .encode("日本語のテキストです。これは文章です。")
                .0,
        );
        assert!(std::str::from_utf8(&page).is_err(), "the fixture must not be UTF-8");
        assert_ne!(
            decided("text/html", &page).0,
            "Shift_JIS",
            "text past the limit must not reach the detector"
        );
        let early = &page[DETECTION_LIMIT - 4096..];
        assert_eq!(
            decided("text/html", early).0,
            "Shift_JIS",
            "the same text inside the limit decides"
        );
    }

    #[test]
    fn a_legacy_page_cut_inside_its_last_character_keeps_its_encoding() {
        for (encoding, text) in [
            (encoding_rs::SHIFT_JIS, "日本語のテキストです。これは文章です"),
            (encoding_rs::EUC_JP, "日本語のテキストです。これは文章です"),
            (encoding_rs::EUC_KR, "한국어로 쓴 웹 페이지입니다"),
            (encoding_rs::GBK, "这是一个用中文写的网页"),
            (encoding_rs::BIG5, "這是一個用中文寫的網頁"),
        ] {
            let mut page = b"<p>".to_vec();
            page.extend_from_slice(&encoding.encode(text).0);
            let cut = &page[..page.len() - 1];
            assert_eq!(
                decided("text/html", &page).0,
                encoding.name(),
                "the whole page in {}",
                encoding.name()
            );
            assert_eq!(
                decided("text/html", cut).0,
                encoding.name(),
                "the page in {} without its last byte",
                encoding.name()
            );
        }
    }

    #[test]
    fn the_end_of_a_whole_body_ends_its_last_word_for_the_detector() {
        // ~keep Russian in KOI8-R. Before the end of the input the detector takes these bytes
        // ~keep for GBK: six bytes are three characters, five are two and one that is cut short.
        let russian = encoding_rs::KOI8_R.encode("Библио").0.into_owned();
        assert_eq!(decided("text/plain", &russian).0, "KOI8-U");
        assert!(
            before_a_cut_character(encoding_rs::GBK, &russian[..5]).is_some(),
            "the fixture must read as GBK but for its end"
        );
        assert_eq!(
            decided("text/plain", &russian[..5]).0,
            "KOI8-U",
            "a guess the detector does not make for the body without its last byte must not stand"
        );
    }

    #[test]
    fn only_a_last_character_that_is_cut_short_is_set_aside() {
        let japanese = encoding_rs::SHIFT_JIS.encode("日本語").0.into_owned();
        assert_eq!(before_a_cut_character(encoding_rs::SHIFT_JIS, &japanese), None);
        assert_eq!(
            before_a_cut_character(encoding_rs::SHIFT_JIS, &japanese[..5]),
            Some(&japanese[..4])
        );
        assert_eq!(
            before_a_cut_character(encoding_rs::SHIFT_JIS, b"\x93\xfa\xff\xff\xff\xff\x93"),
            None,
            "bytes the encoding cannot read before the end are not a cut"
        );
        assert_eq!(before_a_cut_character(encoding_rs::SHIFT_JIS, b"\x93"), Some(&b""[..]));
    }

    #[test]
    fn the_meta_scan_reads_the_start_of_a_long_head() {
        const TAG: &str = "<meta charset=shift_jis>";
        let head = |padding: usize| format!("<html><head><!-- {} -->{TAG}", "x".repeat(padding));
        let inside = head(META_SCAN_LIMIT - 21 - TAG.len());
        assert_eq!(inside.len(), META_SCAN_LIMIT, "the tag must end at the limit");
        assert_eq!(
            decided("text/html", inside.as_bytes()),
            ("Shift_JIS", label("shift_jis"))
        );
        let outside = head(META_SCAN_LIMIT - 20 - TAG.len());
        assert_eq!(
            decided("text/html", outside.as_bytes()),
            ("UTF-8", None),
            "a tag that ends past the limit must not be read"
        );
    }

    #[cfg(feature = "browser-native")]
    #[test]
    fn a_document_of_the_native_backend_is_decoded_as_a_page_is() {
        assert_eq!(
            decode_document("text/html; charset=shift_jis", URL, &[0x93, 0xFA, 0x96, 0x7B]),
            ("日本".to_owned(), label("shift_jis"))
        );
        assert_eq!(
            decode_document("text/html", URL, "<p>caf\u{e9}</p>".as_bytes()),
            ("<p>caf\u{e9}</p>".to_owned(), None)
        );
        assert_eq!(
            decode_document("text/html; charset=utf-8", URL, b"caf\xe9"),
            ("caf\u{FFFD}".to_owned(), label("utf-8"))
        );
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
