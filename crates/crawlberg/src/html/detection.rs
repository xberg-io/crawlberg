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

/// Check whether content appears to be HTML based on Content-Type header or body content.
pub(crate) fn is_html_content(content_type: &str, body: &str) -> bool {
    if content_type.contains("html") {
        return true;
    }
    let trimmed = body.trim_start();
    if !trimmed.starts_with('<') {
        return false;
    }
    let lower = trimmed.to_lowercase();
    if lower.starts_with("<?xml") && !lower.contains("<html") {
        return false;
    }
    lower.starts_with("<!doctype")
        || lower.starts_with("<html")
        || lower.starts_with("<head")
        || lower.starts_with("<body")
        || lower.starts_with("<div")
        || lower.starts_with("<p")
        || lower.starts_with("<h1")
        || lower.starts_with("<script")
        || lower.starts_with("<meta")
        || lower.starts_with("<link")
        || lower.starts_with("<!")
}

/// Check whether a Content-Type header indicates binary content.
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
