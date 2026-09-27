//! Document download helper.
//!
//! Shared logic for materializing a [`DownloadedDocument`] from a fetched
//! non-HTML response (PDF, DOCX, image, …). Used by both the single-page
//! [`scrape`](crate::scrape) path and the multi-page crawl loop so the two
//! cannot diverge.

use std::borrow::Cow;
use std::path::Path;
#[cfg(not(target_arch = "wasm32"))]
use std::path::PathBuf;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use opentelemetry::KeyValue;
use sha2::{Digest, Sha256};
use url::Url;

use crate::telemetry::attributes::{CRAWL_MIME_TYPE, CRAWL_SIZE_BYTES, URL_FULL};
use crate::telemetry::metrics::registry;
use crate::types::{CrawlConfig, DocumentContentEncoding, DownloadedDocument};

/// Default cap on a downloaded document's size when `document_max_size` is unset.
///
/// ~keep `pub(crate)` so `engine::crawl_loop` can mirror this same default when it lifts
/// the fetch-time `max_body_size` read cap for document-shaped URLs (see
/// `run_crawl_loop`); duplicating the literal there would drift from this one.
pub(crate) const DEFAULT_DOCUMENT_MAX_SIZE: usize = 50 * 1024 * 1024;

/// A caller-supplied predicate deciding whether a response is materialized as a document.
///
/// Receives the normalized declared MIME type, at most `document_max_size` bytes of the body,
/// and what the built-in `document_mime_types`/classification decision would have been, so a
/// predicate can widen or narrow that decision rather than only replace it.
pub(crate) type DocumentFilter = dyn Fn(&str, &[u8], bool) -> bool + Send + Sync;

/// The response facts the document-materialization decision is made from.
pub(crate) struct DocumentInput<'a> {
    pub(crate) content_type: &'a str,
    pub(crate) body_bytes: &'a [u8],
    pub(crate) is_document: bool,
}

/// Fallback extension used when a document has no filename hint to derive one from.
const DEFAULT_DOCUMENT_EXTENSION: &str = "bin";

/// Strip any `;charset=...` (or other) parameter from a `Content-Type` header value.
fn normalize_mime_type(content_type: &str) -> Cow<'static, str> {
    Cow::Owned(content_type.split(';').next().unwrap_or(content_type).trim().to_owned())
}

/// Whether a document with this normalized `mime_type` should be downloaded.
///
/// An empty `document_mime_types` keeps today's built-in behavior: the decision is
/// `is_document`, the caller's `is_binary`/`is_pdf` classification. A non-empty allowlist
/// governs the decision entirely on its own instead — both restricting (a response
/// `is_document` flagged but whose MIME type is absent from the list is not downloaded)
/// and extending (a MIME type the built-in heuristics never flag as a document, e.g.
/// `application/json`, is downloaded when explicitly listed) — matching
/// `CrawlConfig.document_mime_types`'s documented contract.
fn should_download_mime(config: &CrawlConfig, mime_type: &str, is_document: bool) -> bool {
    if config.document_mime_types.is_empty() {
        is_document
    } else {
        config
            .document_mime_types
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(mime_type))
    }
}

/// SHA-256 hex digest of `bytes`.
///
/// ~keep Always hashes the full original bytes, never a truncated copy — a hash of
/// truncated content would collide for any two documents sharing a `document_max_size`
/// prefix and would not identify the real (untruncated) document.
fn hash_content(bytes: &[u8]) -> Box<str> {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
        .into()
}

/// Derive a filename hint from the final path segment of `parsed_url`.
fn derive_filename(parsed_url: &Url) -> Option<Box<str>> {
    parsed_url
        .path_segments()
        .and_then(|mut s| s.next_back())
        .filter(|s| !s.is_empty())
        .map(|s| s.into())
}

/// Filesystem extension for a streamed document, taken from its filename hint.
fn extension_from_filename(filename: Option<&str>) -> &str {
    filename
        .and_then(|f| Path::new(f).extension())
        .and_then(|ext| ext.to_str())
        .unwrap_or(DEFAULT_DOCUMENT_EXTENSION)
}

/// Write `content` to `<dir>/<content_hash>.<extension>`, creating `dir` if needed.
///
/// ~keep Uses `tokio::fs`, not `std::fs`: this runs inside `build_downloaded_document`,
/// which is now itself `async` and called from the crawl loop's per-task future. A
/// synchronous write here would block the executor thread for the write's full
/// duration — up to `document_max_size` (50 MB by default) worth of bytes.
#[cfg(not(target_arch = "wasm32"))]
async fn write_document_file(
    dir: &Path,
    content_hash: &str,
    extension: &str,
    content: &[u8],
) -> std::io::Result<PathBuf> {
    tokio::fs::create_dir_all(dir).await?;
    let path = dir.join(format!("{content_hash}.{extension}"));
    tokio::fs::write(&path, content).await?;
    Ok(path)
}

/// Stream `content` to `document_output_dir` when set, returning the bytes to keep
/// in-memory (empty when the stream succeeded) and the path written, if any.
///
/// ~keep On success `content` is dropped in favor of `content_path` so a `CrawlResult`
/// holding many documents does not retain an up-to-`document_max_size` buffer per
/// document once it is already durably on disk. On write failure the original bytes
/// are kept so the caller does not silently lose the document.
#[cfg(not(target_arch = "wasm32"))]
async fn persist_document(
    dir: &Path,
    content_hash: &str,
    extension: &str,
    content: Vec<u8>,
) -> (Vec<u8>, Option<String>) {
    match write_document_file(dir, content_hash, extension, &content).await {
        Ok(path) => (Vec::new(), Some(path.to_string_lossy().into_owned())),
        Err(error) => {
            tracing::warn!(
                error = %error,
                dir = %dir.display(),
                "failed to write document_output_dir file; keeping content in memory instead"
            );
            (content, None)
        }
    }
}

/// wasm32 has no filesystem — `document_output_dir` cannot be honored there.
#[cfg(target_arch = "wasm32")]
async fn persist_document(
    dir: &Path,
    _content_hash: &str,
    _extension: &str,
    content: Vec<u8>,
) -> (Vec<u8>, Option<String>) {
    tracing::warn!(
        dir = %dir.display(),
        "document_output_dir is set but wasm32 has no filesystem; keeping content in memory instead"
    );
    (content, None)
}

/// Emit the download span, the truncation warning, and the discovery counter.
///
/// ~keep Kept out of `build_downloaded_document` because `EnteredSpan` is !Send: entering it
/// inside that async fn and holding it across the `persist_document` await makes the whole
/// future !Send, which breaks every spawned batch task and axum handler. A synchronous helper
/// cannot hold it across an await at all.
fn record_download_telemetry(url: &str, mime_type: &str, size: usize, max_size: usize, truncated: bool) {
    // ~keep `url` may carry userinfo (http://user:pass@host/); redact before it reaches
    // the span, which is shipped to logs/OTLP by default.
    let redacted_url = crate::net::redact_url_credentials(url);
    let _span = tracing::info_span!(
        "crawl.document.download",
        { URL_FULL } = %redacted_url,
        { CRAWL_MIME_TYPE } = %mime_type,
        { CRAWL_SIZE_BYTES } = size as i64,
    )
    .entered();

    if truncated {
        tracing::warn!(
            size,
            max_size,
            "document exceeded document_max_size; content truncated, size and content_hash still reflect \
             the original bytes"
        );
    }

    registry()
        .documents_discovered_total
        .add(1, &[KeyValue::new("mime_type", mime_type.to_string())]);
}

/// Build a [`DownloadedDocument`] from a fetched response body.
///
/// Returns `None` when document downloading is disabled (`download_documents` is
/// `false`) or [`should_download_mime`] rejects this response — either because
/// `document_mime_types` is empty and the response is not a document (`is_document` is
/// `false`), or because `document_mime_types` is non-empty and does not list this
/// response's MIME type.
///
/// `size` always reports the true, original byte count — even when `content` (or the
/// file written under `document_output_dir`) was truncated to `document_max_size`.
/// Compare `content.len()`/the written file's length against `size` to detect
/// truncation. `content_hash` is always the digest of the original bytes, so it
/// identifies the real document regardless of truncation.
///
/// `url` is recorded verbatim on the result; `parsed_url` is used only to derive the
/// filename hint from the final path segment.
/// ~keep Test-only: both production callers (`engine::page_result` and
/// ~keep `scrape::scrape_from_crawl_response`) now pass the engine's document filter through
/// ~keep `build_downloaded_document_with_filter`. Kept so the unit tests below read as the
/// ~keep default-policy cases they are instead of threading a `None` each.
#[cfg(test)]
pub(crate) async fn build_downloaded_document(
    url: &str,
    parsed_url: &Url,
    content_type: &str,
    body_bytes: &[u8],
    is_document: bool,
    config: &CrawlConfig,
) -> Option<DownloadedDocument> {
    build_downloaded_document_with_filter(
        url,
        parsed_url,
        DocumentInput {
            content_type,
            body_bytes,
            is_document,
        },
        config,
        None,
    )
    .await
}

pub(crate) async fn build_downloaded_document_with_filter(
    url: &str,
    parsed_url: &Url,
    input: DocumentInput<'_>,
    config: &CrawlConfig,
    document_filter: Option<&DocumentFilter>,
) -> Option<DownloadedDocument> {
    if !config.download_documents {
        return None;
    }

    let mime_type = normalize_mime_type(input.content_type);
    let max_size = config.document_max_size.unwrap_or(DEFAULT_DOCUMENT_MAX_SIZE);
    let filter_bytes = &input.body_bytes[..input.body_bytes.len().min(max_size)];
    let by_declared_mime = should_download_mime(config, &mime_type, input.is_document);
    // ~keep The predicate is handed `by_declared_mime` rather than replacing it blind, so
    // ~keep "the built-in decision OR my byte check" is expressible; without it a caller had to
    // ~keep reimplement `document_mime_types` matching to widen the default.
    let accepted = match document_filter {
        Some(filter) => filter(&mime_type, filter_bytes, by_declared_mime),
        None => by_declared_mime,
    };
    if !accepted {
        tracing::debug!(
            { CRAWL_MIME_TYPE } = %mime_type,
            is_document = input.is_document,
            by_declared_mime,
            filtered = document_filter.is_some(),
            "response rejected by document materialization policy"
        );
        return None;
    }

    let size = input.body_bytes.len();
    let truncated = size > max_size;
    let content = if truncated {
        input.body_bytes[..max_size].to_vec()
    } else {
        input.body_bytes.to_vec()
    };
    let content_hash = hash_content(input.body_bytes);
    let filename = derive_filename(parsed_url);

    record_download_telemetry(url, &mime_type, size, max_size, truncated);

    let content_base64 = matches!(config.document_content_encoding, Some(DocumentContentEncoding::Base64))
        .then(|| BASE64.encode(&content));

    let (content, content_path) = match config.document_output_dir.as_ref() {
        Some(dir) => {
            let extension = extension_from_filename(filename.as_deref());
            persist_document(dir, &content_hash, extension, content).await
        }
        None => (content, None),
    };

    Some(DownloadedDocument {
        url: url.to_owned(),
        mime_type,
        size,
        content,
        filename,
        content_hash,
        headers: std::collections::HashMap::new(),
        truncated,
        content_path,
        content_base64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn pdf_url() -> Url {
        Url::parse("https://example.com/files/report.pdf").expect("valid url")
    }

    #[tokio::test]
    async fn returns_none_when_downloads_disabled() {
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
        )
        .await;
        assert!(doc.is_none(), "disabled downloads must yield None");
    }

    #[tokio::test]
    async fn byte_aware_filter_sees_only_document_max_size_prefix() {
        let config = CrawlConfig {
            document_max_size: Some(4),
            ..Default::default()
        };
        let observed = Arc::new(Mutex::new(Vec::new()));
        let observed_by_filter = Arc::clone(&observed);
        let filter = move |declared: &str, bytes: &[u8], by_declared_mime: bool| {
            assert_eq!(declared, "text/plain");
            assert!(
                by_declared_mime,
                "an empty document_mime_types falls back to the built-in classification, which \
                 this input sets to true"
            );
            *observed_by_filter.lock().expect("filter lock") = bytes.to_vec();
            true
        };
        let document = build_downloaded_document_with_filter(
            pdf_url().as_str(),
            &pdf_url(),
            DocumentInput {
                content_type: "text/plain; charset=utf-8",
                body_bytes: b"%PDF-1.7 body",
                is_document: true,
            },
            &config,
            Some(&filter),
        )
        .await
        .expect("filter admits document");

        assert_eq!(*observed.lock().expect("filter lock"), b"%PDF");
        assert_eq!(document.content.as_slice(), b"%PDF");
        assert!(document.truncated);
    }

    #[tokio::test]
    async fn returns_none_for_non_document_page() {
        let config = CrawlConfig::default();
        let html_url = Url::parse("https://example.com/page").expect("valid url");
        let doc = build_downloaded_document(
            html_url.as_str(),
            &html_url,
            "text/html",
            b"<html></html>",
            false,
            &config,
        )
        .await;
        assert!(doc.is_none(), "an HTML page must yield None");
    }

    #[tokio::test]
    async fn captures_bytes_mime_hash_and_filename() {
        let config = CrawlConfig::default();
        let doc = build_downloaded_document(
            pdf_url().as_str(),
            &pdf_url(),
            "application/pdf; charset=binary",
            b"%PDF-1.4 body",
            true,
            &config,
        )
        .await
        .expect("a document is expected");
        assert_eq!(doc.content.as_slice(), b"%PDF-1.4 body");
        assert_eq!(doc.size, 13);
        assert!(!doc.truncated, "an under-cap document must not be flagged truncated");
        assert_eq!(
            &*doc.mime_type, "application/pdf",
            "mime must drop the charset parameter"
        );
        assert_eq!(doc.filename.as_deref(), Some("report.pdf"));
        assert_eq!(doc.content_hash.len(), 64, "sha-256 hex digest is 64 chars");
    }

    #[tokio::test]
    async fn truncates_content_but_reports_true_size_and_flag() {
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
        .await
        .expect("a document is expected");
        assert_eq!(
            doc.content.as_slice(),
            b"0123",
            "content must be capped at document_max_size"
        );
        assert_eq!(
            doc.size, 10,
            "size must report the true original length, not the truncated length"
        );
        assert!(
            doc.truncated,
            "a document over document_max_size must be flagged truncated"
        );
    }

    #[tokio::test]
    async fn content_hash_identifies_the_original_bytes_not_the_truncated_prefix() {
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
        .await
        .expect("a document is expected");

        let mut hasher = Sha256::new();
        hasher.update(b"0123456789");
        let expected: Box<str> = hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
            .into();
        assert_eq!(
            doc.content_hash, expected,
            "content_hash must hash the full original bytes, not the truncated content"
        );

        let mut prefix_hasher = Sha256::new();
        prefix_hasher.update(b"0123");
        let prefix_hash: Box<str> = prefix_hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
            .into();
        assert_ne!(
            doc.content_hash, prefix_hash,
            "content_hash must not collide with a document whose full content equals this truncated prefix"
        );
    }

    #[tokio::test]
    async fn document_mime_types_allowlist_restricts_downloads() {
        let config = CrawlConfig {
            document_mime_types: vec!["application/pdf".to_owned()],
            ..Default::default()
        };
        let doc = build_downloaded_document(
            pdf_url().as_str(),
            &pdf_url(),
            "application/msword",
            b"not a pdf",
            true,
            &config,
        )
        .await;
        assert!(
            doc.is_none(),
            "a mime type absent from a non-empty document_mime_types allowlist must not be downloaded"
        );
    }

    #[tokio::test]
    async fn document_mime_types_allowlist_permits_listed_mime_case_insensitively() {
        let config = CrawlConfig {
            document_mime_types: vec!["APPLICATION/PDF".to_owned()],
            ..Default::default()
        };
        let doc = build_downloaded_document(
            pdf_url().as_str(),
            &pdf_url(),
            "application/pdf",
            b"%PDF-1.4",
            true,
            &config,
        )
        .await;
        assert!(doc.is_some(), "an allowlisted mime type must still be downloaded");
    }

    #[tokio::test]
    async fn document_mime_types_allowlist_extends_beyond_builtin_classification() {
        let config = CrawlConfig {
            document_mime_types: vec!["application/json".to_owned()],
            ..Default::default()
        };
        let json_url = Url::parse("https://example.com/data.json").expect("valid url");
        let doc =
            build_downloaded_document(json_url.as_str(), &json_url, "application/json", b"{}", false, &config).await;
        assert!(
            doc.is_some(),
            "a mime type listed in a non-empty document_mime_types must be downloaded \
             even when the built-in is_binary/is_pdf classifier never flags it as a document"
        );
    }

    #[tokio::test]
    async fn empty_document_mime_types_keeps_built_in_behavior() {
        let config = CrawlConfig::default();
        let doc = build_downloaded_document(
            pdf_url().as_str(),
            &pdf_url(),
            "application/octet-stream",
            b"binary body",
            true,
            &config,
        )
        .await;
        assert!(
            doc.is_some(),
            "an empty document_mime_types allowlist must not restrict downloads beyond is_document"
        );
    }

    #[tokio::test]
    async fn document_content_encoding_base64_populates_content_base64() {
        let config = CrawlConfig {
            document_content_encoding: Some(DocumentContentEncoding::Base64),
            ..Default::default()
        };
        let doc = build_downloaded_document(
            pdf_url().as_str(),
            &pdf_url(),
            "application/pdf",
            b"%PDF-1.4 body",
            true,
            &config,
        )
        .await
        .expect("a document is expected");
        assert_eq!(
            doc.content_base64.as_deref(),
            Some(BASE64.encode(b"%PDF-1.4 body")).as_deref(),
            "content_base64 must hold the base64-encoded content"
        );
    }

    #[tokio::test]
    async fn no_document_content_encoding_leaves_content_base64_none() {
        let config = CrawlConfig::default();
        let doc = build_downloaded_document(
            pdf_url().as_str(),
            &pdf_url(),
            "application/pdf",
            b"%PDF-1.4 body",
            true,
            &config,
        )
        .await
        .expect("a document is expected");
        assert!(
            doc.content_base64.is_none(),
            "content_base64 must stay None when document_content_encoding is unset"
        );
    }

    #[tokio::test]
    async fn document_output_dir_streams_bytes_to_disk_and_clears_in_memory_content() {
        let dir = std::env::temp_dir().join(format!("crawlberg-doc-test-{}", std::process::id()));
        let config = CrawlConfig {
            document_output_dir: Some(dir.clone()),
            ..Default::default()
        };
        let doc = build_downloaded_document(
            pdf_url().as_str(),
            &pdf_url(),
            "application/pdf",
            b"%PDF-1.4 body",
            true,
            &config,
        )
        .await
        .expect("a document is expected");

        assert!(
            doc.content.is_empty(),
            "content must be cleared in memory once streamed to document_output_dir"
        );
        let path = doc.content_path.as_deref().expect("content_path must be populated");
        let written = std::fs::read(path).expect("the streamed file must exist");
        assert_eq!(
            written, b"%PDF-1.4 body",
            "the streamed file must hold the document bytes"
        );
        assert!(
            path.ends_with(".pdf"),
            "the extension must come from the filename hint, got {path}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn no_document_output_dir_leaves_content_path_none() {
        let config = CrawlConfig::default();
        let doc = build_downloaded_document(
            pdf_url().as_str(),
            &pdf_url(),
            "application/pdf",
            b"%PDF-1.4 body",
            true,
            &config,
        )
        .await
        .expect("a document is expected");
        assert!(doc.content_path.is_none(), "content_path must stay None by default");
        assert_eq!(
            doc.content.as_slice(),
            b"%PDF-1.4 body",
            "content must stay in memory by default"
        );
    }
}
