//! Content type detection (HTML, binary, PDF).

/// Binary file extensions used to detect non-HTML content.
///
/// Covers every binary document format extractable downstream (kept in sync with
/// the xberg format registry). Text-based document formats (json, csv, md, xml, …)
/// are intentionally absent: they survive lossy UTF-8 decoding and are handled via
/// the regular page path.
static BINARY_EXTENSIONS: &[&str] = &[
    ".jpg", ".jpeg", ".png", ".gif", ".bmp", ".webp", ".svg", ".ico", ".tiff", ".tif", ".avif", ".heic", ".heics",
    ".heif", ".j2c", ".j2k", ".jp2", ".jpm", ".jpx", ".mj2", ".jb2", ".jbig2", ".pbm", ".pgm", ".ppm", ".pnm", ".mp4",
    ".avi", ".mov", ".wmv", ".flv", ".mkv", ".webm", ".mp3", ".wav", ".ogg", ".flac", ".aac", ".wma", ".m4a", ".mpeg",
    ".mpga", ".docx", ".docm", ".dotx", ".dot", ".dotm", ".doc", ".xlsx", ".xlsm", ".xlsb", ".xls", ".xla", ".xlam",
    ".xlt", ".xltx", ".pptx", ".pptm", ".potx", ".pot", ".potm", ".ppsx", ".ppt", ".odt", ".ods", ".odp", ".odg",
    ".key", ".numbers", ".pages", ".hwpx", ".hwp", ".epub", ".rtf", ".fb2", ".msg", ".pst", ".eml", ".dbf", ".zip",
    ".gz", ".tgz", ".tar", ".7z", ".rar", ".bz2", ".xz", ".zst", ".exe", ".dll", ".so", ".bin",
];

/// Tag-name prefixes (already lowercase) that mark `body` as HTML.
const HTML_TAG_PREFIXES: &[&str] = &[
    "<!doctype",
    "<html",
    "<head",
    "<body",
    "<div",
    "<p",
    "<h1",
    "<script",
    "<meta",
    "<link",
    "<!",
];

/// Case-insensitive ASCII prefix check that avoids allocating a lowercased copy of `haystack`.
fn starts_with_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    let haystack = haystack.as_bytes();
    let needle = needle.as_bytes();
    haystack.len() >= needle.len() && haystack[..needle.len()].eq_ignore_ascii_case(needle)
}

/// Case-insensitive ASCII substring check that avoids allocating a lowercased copy of `haystack`.
fn contains_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    let haystack = haystack.as_bytes();
    let needle = needle.as_bytes();
    if needle.is_empty() {
        return true;
    }
    needle.len() <= haystack.len() && haystack.windows(needle.len()).any(|w| w.eq_ignore_ascii_case(needle))
}

/// Check whether content appears to be HTML based on Content-Type header or body content.
///
/// HTML tag names are ASCII, so only a byte-wise ASCII case fold is needed here — lowercasing
/// the whole (potentially multi-megabyte) `body` via `str::to_lowercase` for a handful of short
/// prefix/substring checks was wasted work on large pages.
pub(crate) fn is_html_content(content_type: &str, body: &str) -> bool {
    if content_type.contains("html") {
        return true;
    }
    let trimmed = body.trim_start();
    if !trimmed.starts_with('<') {
        return false;
    }
    if starts_with_ignore_ascii_case(trimmed, "<?xml") && !contains_ignore_ascii_case(trimmed, "<html") {
        return false;
    }
    HTML_TAG_PREFIXES
        .iter()
        .any(|prefix| starts_with_ignore_ascii_case(trimmed, prefix))
}

/// Check whether a Content-Type header indicates binary content.
///
/// ~keep These are the "built-in defaults" `CrawlConfig.document_mime_types` refers to.
/// This function only decides `is_binary`/markdown-skip classification; it does not
/// gate document *downloading* — a non-empty `document_mime_types` allowlist governs
/// that decision independently in `document::build_downloaded_document`.
pub(crate) fn is_binary_content_type(ct: &str) -> bool {
    let lower = ct.to_lowercase();
    if lower.starts_with("image/")
        || lower.starts_with("video/")
        || lower.starts_with("audio/")
        || lower.starts_with("message/")
    {
        return true;
    }
    lower.starts_with("application/octet-stream")
        || lower.starts_with("application/pdf")
        || lower.starts_with("application/msword")
        || lower.starts_with("application/rtf")
        || lower.starts_with("text/rtf")
        || lower.contains("openxmlformats")
        || lower.contains("opendocument")
        || lower.contains("ms-excel")
        || lower.contains("ms-powerpoint")
        || lower.contains("ms-word")
        || lower.contains("ms-outlook")
        || lower.contains("iwork")
        || lower.contains("hwp")
        || lower.contains("epub")
        || lower.contains("fictionbook")
        || lower.contains("dbase")
        || lower.contains("x-dbf")
        || lower.contains("zip")
        || lower.contains("tar")
        || lower.contains("7z-compressed")
        || lower.contains("x-rar")
        || lower.contains("bzip")
        || lower.contains("x-xz")
        || lower.contains("zstd")
}

/// Check whether a URL has a binary file extension.
pub(crate) fn is_binary_url(url: &str) -> bool {
    let lower = url.to_lowercase();
    let path = lower.split('?').next().unwrap_or(&lower);
    let path = path.split('#').next().unwrap_or(path);
    BINARY_EXTENSIONS.iter().any(|ext| path.ends_with(ext))
}

/// Check whether content is a PDF based on Content-Type or body magic bytes.
pub(crate) fn is_pdf_content(ct: &str, body: &str) -> bool {
    ct.to_lowercase().contains("application/pdf") || body.starts_with("%PDF")
}

/// Check whether a URL has a `.pdf` extension.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn is_pdf_url(url: &str) -> bool {
    let lower = url.to_lowercase();
    let path = lower.split('?').next().unwrap_or(&lower);
    let path = path.split('#').next().unwrap_or(path);
    path.ends_with(".pdf")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Characterization cases for `is_html_content`, captured before the
    /// full-body-lowercase optimization so the refactor can be proven
    /// behavior-preserving by re-running this exact table.
    fn is_html_content_cases() -> Vec<(&'static str, &'static str, bool)> {
        vec![
            ("text/html", "", true),
            ("application/json", "not html at all", false),
            ("", "<!DOCTYPE html><html></html>", true),
            ("", "<!doctype html><html></html>", true),
            ("", "  \n\t<HTML><body>x</body></html>", true),
            ("", "<HeAd><title>t</title></head>", true),
            ("", "<BODY>text</BODY>", true),
            ("", "<div>x</div>", true),
            ("", "<DIV>x</div>", true),
            ("", "<p>paragraph</p>", true),
            ("", "<H1>Title</h1>", true),
            ("", "<script>var x = 1;</script>", true),
            ("", "<META charset=\"utf-8\">", true),
            ("", "<LINK rel=\"stylesheet\" href=\"a.css\">", true),
            ("", "<!-- a comment -->", true),
            // ~keep The trimmed body always starts with "<?xml", which never matches any of
            // the recognized prefixes below, so this is false regardless of whether "<html"
            // appears later — verified against the pre-optimization implementation.
            ("", "<?xml version=\"1.0\"?><html><body/></html>", false),
            ("", "<?xml version=\"1.0\"?><rss><channel/></rss>", false),
            ("", "<?XML version=\"1.0\"?><feed></feed>", false),
            ("", "not starting with a tag at all", false),
            ("", "<unknown-tag>x</unknown-tag>", false),
            ("", "", false),
            ("", "   ", false),
        ]
    }

    #[test]
    fn is_html_content_matches_characterized_cases() {
        for (content_type, body, expected) in is_html_content_cases() {
            assert_eq!(
                is_html_content(content_type, body),
                expected,
                "is_html_content({content_type:?}, {body:?}) expected {expected}"
            );
        }
    }

    #[test]
    fn is_html_content_handles_a_large_non_html_body_without_panicking() {
        let body = format!("<{}", "x".repeat(200_000));
        assert!(
            !is_html_content("", &body),
            "a large body starting with an unrecognized tag must not be classified as HTML"
        );
    }

    #[test]
    fn office_and_archive_content_types_are_binary() {
        for ct in [
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            "application/vnd.openxmlformats-officedocument.presentationml.presentation",
            "application/vnd.oasis.opendocument.text",
            "application/msword",
            "application/vnd.ms-excel",
            "application/vnd.ms-powerpoint",
            "application/vnd.ms-outlook",
            "application/haansofthwpx",
            "application/x-hwp",
            "application/epub+zip",
            "application/vnd.epub+zip",
            "application/x-iwork-keynote-sffkey",
            "application/rtf",
            "application/zip",
            "application/x-zip-compressed",
            "application/gzip",
            "application/x-tar",
            "application/x-7z-compressed",
            "message/rfc822",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document; charset=binary",
        ] {
            assert!(is_binary_content_type(ct), "expected binary: {ct}");
        }
    }

    #[test]
    fn text_and_html_content_types_are_not_binary() {
        for ct in [
            "text/html",
            "application/json",
            "text/csv",
            "text/plain",
            "application/xml",
            "text/markdown",
        ] {
            assert!(!is_binary_content_type(ct), "expected non-binary: {ct}");
        }
    }

    #[test]
    fn office_and_archive_urls_are_binary() {
        for url in [
            "https://example.com/report.docx",
            "https://example.com/sheet.xlsx",
            "https://example.com/deck.pptx",
            "https://example.com/doc.odt",
            "https://example.com/book.epub",
            "https://example.com/file.hwpx",
            "https://example.com/archive.zip?token=abc",
            "https://example.com/data.tar.gz#frag",
        ] {
            assert!(is_binary_url(url), "expected binary url: {url}");
        }
    }
}
