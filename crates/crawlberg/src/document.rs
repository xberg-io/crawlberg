//! Document download helper.
//!
//! Shared logic for materializing a [`DownloadedDocument`] from a fetched
//! non-HTML response (PDF, DOCX, image, …). Used by both the single-page
//! [`scrape`](crate::scrape) path and the multi-page crawl loop so the two
//! cannot diverge.

use opentelemetry::KeyValue;
use sha2::{Digest, Sha256};
use url::Url;

use crate::telemetry::attributes::{CRAWL_MIME_TYPE, CRAWL_SIZE_BYTES, URL_FULL};
use crate::telemetry::metrics::registry;
use crate::types::{CrawlConfig, DownloadedDocument};

/// Default cap on a downloaded document's size when `document_max_size` is unset.
const DEFAULT_DOCUMENT_MAX_SIZE: usize = 50 * 1024 * 1024;

/// Build a [`DownloadedDocument`] from a fetched response body.
///
/// Returns `None` when document downloading is disabled (`download_documents`
/// is `false`) or the response is not a document (`is_document` is `false`).
/// Otherwise the raw bytes are captured — truncated to `document_max_size` —
/// and the SHA-256 digest computed.
///
/// `url` is recorded verbatim on the result; `parsed_url` is used only to
/// derive the filename hint from the final path segment.
pub(crate) fn build_downloaded_document(
    url: &str,
    parsed_url: &Url,
    content_type: &str,
    body_bytes: &[u8],
    is_document: bool,
    config: &CrawlConfig,
) -> Option<DownloadedDocument> {
    if !config.download_documents || !is_document {
        return None;
    }

    let max_size = config.document_max_size.unwrap_or(DEFAULT_DOCUMENT_MAX_SIZE);
    let content = if body_bytes.len() <= max_size {
        body_bytes.to_vec()
    } else {
        body_bytes[..max_size].to_vec()
    };

    let mut hasher = Sha256::new();
    hasher.update(&content);
    let hash_bytes = hasher.finalize();
    let content_hash: Box<str> = hash_bytes.iter().map(|b| format!("{b:02x}")).collect::<String>().into();

    let mime_type: std::borrow::Cow<'static, str> =
        std::borrow::Cow::Owned(content_type.split(';').next().unwrap_or(content_type).trim().to_owned());

    let filename = parsed_url
        .path_segments()
        .and_then(|mut s| s.next_back())
        .filter(|s| !s.is_empty())
        .map(|s| s.into());

    let size = content.len();

    let _span = tracing::info_span!(
        "crawl.document.download",
        { URL_FULL } = url,
        { CRAWL_MIME_TYPE } = %mime_type,
        { CRAWL_SIZE_BYTES } = size as i64,
    )
    .entered();

    registry()
        .documents_discovered_total
        .add(1, &[KeyValue::new("mime_type", mime_type.to_string())]);

    Some(DownloadedDocument {
        url: url.to_owned(),
        mime_type,
        size,
        content,
        filename,
        content_hash,
        headers: std::collections::HashMap::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pdf_url() -> Url {
        Url::parse("https://example.com/files/report.pdf").expect("valid url")
    }

    #[test]
    fn returns_none_when_downloads_disabled() {
        let config = CrawlConfig {
            download_documents: false,
            ..Default::default()
        };
        let doc = build_downloaded_document(
            pdf_url().as_str(),
            &pdf_url(),
            "application/pdf",
            b"%PDF-1.4",
            true,
            &config,
        );
        assert!(doc.is_none(), "disabled downloads must yield None");
    }

    #[test]
    fn returns_none_for_non_document_page() {
        let config = CrawlConfig::default();
        let html_url = Url::parse("https://example.com/page").expect("valid url");
        let doc = build_downloaded_document(
            html_url.as_str(),
            &html_url,
            "text/html",
            b"<html></html>",
            false,
            &config,
        );
        assert!(doc.is_none(), "an HTML page must yield None");
    }

    #[test]
    fn captures_bytes_mime_hash_and_filename() {
        let config = CrawlConfig::default();
        let doc = build_downloaded_document(
            pdf_url().as_str(),
            &pdf_url(),
            "application/pdf; charset=binary",
            b"%PDF-1.4 body",
            true,
            &config,
        )
        .expect("a document is expected");
        assert_eq!(doc.content.as_slice(), b"%PDF-1.4 body");
        assert_eq!(doc.size, 13);
        assert_eq!(
            &*doc.mime_type, "application/pdf",
            "mime must drop the charset parameter"
        );
        assert_eq!(doc.filename.as_deref(), Some("report.pdf"));
        assert_eq!(doc.content_hash.len(), 64, "sha-256 hex digest is 64 chars");
    }

    #[test]
    fn truncates_content_to_document_max_size() {
        let config = CrawlConfig {
            document_max_size: Some(4),
            ..Default::default()
        };
        let doc = build_downloaded_document(
            pdf_url().as_str(),
            &pdf_url(),
            "application/pdf",
            b"0123456789",
            true,
            &config,
        )
        .expect("a document is expected");
        assert_eq!(
            doc.content.as_slice(),
            b"0123",
            "content must be capped at document_max_size"
        );
        assert_eq!(doc.size, 4);
    }
}
