//! What the main frame of a Chromiumoxide page has committed, and reads bound to one committed
//! document.
//!
//! ~keep A top-level module for the same reason as `ssrf_intercept`: the interact backend
//! ~keep (`interact::chromiumoxide`, gated on `browser-chromiumoxide`) and the render
//! ~keep (`browser::navigation`, gated on `browser`) both read the committed frame. Only a module
//! ~keep gated on the narrower feature is reachable from both.

use std::future::Future;

use chromiumoxide::cdp::browser_protocol::page::GetFrameTreeParams;

use crate::error::CrawlError;

/// How many times a page is read when a new document commits during each read.
const READ_ATTEMPTS: usize = 3;

/// The document the main frame of a page has committed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommittedDocument {
    /// Identifies the committed document. A new document, Chrome's error page included, has a new
    /// loader id.
    pub(crate) loader_id: String,
    /// The URL Chrome failed to show, when the document is Chrome's own error page.
    pub(crate) unreachable_url: Option<String>,
    /// The URL of the document, with its fragment.
    pub(crate) url: String,
}

/// The document the main frame of `page` has committed.
pub(crate) async fn committed_document(page: &chromiumoxide::Page) -> Result<CommittedDocument, CrawlError> {
    page.execute(GetFrameTreeParams::default())
        .await
        .map(|tree| {
            let frame = &tree.result.frame_tree.frame;
            CommittedDocument {
                loader_id: frame.loader_id.clone().into(),
                unreachable_url: frame.unreachable_url.clone(),
                url: format!("{}{}", frame.url, frame.url_fragment.as_deref().unwrap_or_default()),
            }
        })
        .map_err(|e| CrawlError::browser_error(format!("failed to read the committed document: {e}")))
}

/// Read the page with `read` and pair the result with the document `read_document` reports.
///
/// ~keep `read` runs in whatever document is committed when it arrives, so a navigation that
/// ~keep commits during `read` leaves no way to tell which document the result came from. The
/// ~keep committed document is read before and after `read`: when both loader ids agree, the result
/// ~keep is that document's. When they differ, `read` is repeated, whether or not it succeeded: a
/// ~keep read sent to the document the commit replaced can fail with an unknown script context. A
/// ~keep page that commits a new document during every read is an error. Because `read` can run
/// ~keep more than once, it must have no side effects: a script that navigates would run twice.
pub(crate) async fn read_one_document<T, D, R>(
    mut read_document: impl FnMut() -> D,
    mut read: impl FnMut() -> R,
) -> Result<(T, CommittedDocument), CrawlError>
where
    D: Future<Output = Result<CommittedDocument, CrawlError>>,
    R: Future<Output = Result<T, CrawlError>>,
{
    let mut document = read_document().await?;
    for _ in 0..READ_ATTEMPTS {
        let value = read().await;
        let committed = read_document().await?;
        if committed.loader_id == document.loader_id {
            return value.map(|value| (value, committed));
        }
        document = committed;
    }
    Err(CrawlError::browser_error(format!(
        "the page navigated to a new document during each of {READ_ATTEMPTS} reads of its content"
    )))
}

/// The error for a main frame that shows Chrome's own error page for `failed_url`. It names the
/// URL with its credentials redacted.
pub(crate) fn error_page_error(failed_url: &str) -> CrawlError {
    CrawlError::browser_error(format!(
        "Chrome could not load {} and showed its own error page",
        crate::net::redact_url_credentials(failed_url)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(loader_id: &str) -> CommittedDocument {
        CommittedDocument {
            loader_id: loader_id.to_owned(),
            unreachable_url: None,
            url: format!("http://127.0.0.1/{loader_id}"),
        }
    }

    /// Read one document from scripted reads: `documents` answers each committed-document read in
    /// order, and `reads` each page read. Returns the result, as the content and the document's
    /// loader id, and the number of page reads.
    async fn read_scripted(
        documents: &[Result<&str, &str>],
        reads: &[Result<&str, &str>],
    ) -> (Result<(String, String), String>, usize) {
        let mut documents = documents.iter();
        let mut reads = reads.iter();
        let mut page_reads = 0;
        let result = read_one_document(
            || {
                std::future::ready(match documents.next().expect("a scripted document read") {
                    Ok(loader_id) => Ok(document(loader_id)),
                    Err(message) => Err(CrawlError::browser_error(*message)),
                })
            },
            || {
                page_reads += 1;
                std::future::ready(match reads.next().expect("a scripted page read") {
                    Ok(content) => Ok(content.to_string()),
                    Err(message) => Err(CrawlError::browser_error(*message)),
                })
            },
        )
        .await;
        (
            result
                .map(|(value, document)| (value, document.loader_id))
                .map_err(|error| error.to_string()),
            page_reads,
        )
    }

    #[tokio::test]
    async fn the_read_is_paired_with_the_document_read_before_and_after_it() {
        let (result, page_reads) = read_scripted(&[Ok("a"), Ok("a")], &[Ok("<p>a</p>")]).await;
        assert_eq!(result, Ok(("<p>a</p>".to_owned(), "a".to_owned())));
        assert_eq!(page_reads, 1);
    }

    #[tokio::test]
    async fn a_document_committed_during_the_read_is_read_again() {
        let (result, page_reads) = read_scripted(&[Ok("a"), Ok("b"), Ok("b")], &[Ok("<p>a</p>"), Ok("<p>b</p>")]).await;
        assert_eq!(
            result,
            Ok(("<p>b</p>".to_owned(), "b".to_owned())),
            "the content of document a must not be paired with document b"
        );
        assert_eq!(page_reads, 2);
    }

    #[tokio::test]
    async fn a_failed_read_of_a_replaced_document_is_read_again() {
        let (result, page_reads) = read_scripted(
            &[Ok("a"), Ok("b"), Ok("b")],
            &[Err("Cannot find context with specified id"), Ok("<p>b</p>")],
        )
        .await;
        assert_eq!(result, Ok(("<p>b</p>".to_owned(), "b".to_owned())));
        assert_eq!(page_reads, 2);
    }

    #[tokio::test]
    async fn a_failed_read_of_the_committed_document_is_an_error() {
        let (result, page_reads) = read_scripted(&[Ok("a"), Ok("a")], &[Err("boom")]).await;
        let error = result.expect_err("a failed read of an unchanged document is an error");
        assert!(error.contains("boom"), "the read error must be preserved, got: {error}");
        assert_eq!(page_reads, 1);
    }

    #[tokio::test]
    async fn a_page_that_commits_during_every_read_is_an_error() {
        let (result, page_reads) = read_scripted(
            &[Ok("a"), Ok("b"), Ok("c"), Ok("d")],
            &[Ok("<p>a</p>"), Ok("<p>b</p>"), Ok("<p>c</p>")],
        )
        .await;
        let error = result.expect_err("no read belongs to one document");
        assert!(
            error.contains("navigated to a new document during each of 3 reads"),
            "got: {error}"
        );
        assert_eq!(page_reads, READ_ATTEMPTS);
    }

    #[tokio::test]
    async fn a_failed_document_read_before_the_read_is_an_error() {
        let (result, page_reads) = read_scripted(&[Err("closed")], &[]).await;
        let error = result.expect_err("a page whose document cannot be read is not known to be one document");
        assert!(error.contains("closed"), "got: {error}");
        assert_eq!(page_reads, 0);
    }

    #[tokio::test]
    async fn a_failed_document_read_after_the_read_is_an_error() {
        let (result, page_reads) = read_scripted(&[Ok("a"), Err("closed")], &[Ok("<p>a</p>")]).await;
        let error = result.expect_err("a read not bound to a document must not be returned");
        assert!(error.contains("closed"), "got: {error}");
        assert_eq!(page_reads, 1);
    }

    #[test]
    fn the_error_page_error_names_the_url_without_its_credentials() {
        let error = error_page_error("http://user:s3cretpw@127.0.0.1/dl");
        let CrawlError::BrowserError { message, .. } = &error else {
            panic!("got {error:?}");
        };
        assert!(
            message.contains("127.0.0.1/dl") && message.contains("error page") && !message.contains("s3cretpw"),
            "{message}"
        );
    }
}
