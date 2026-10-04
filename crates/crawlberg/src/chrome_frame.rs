//! What the main frame of a Chromiumoxide page has committed, and reads bound to one committed
//! document.
//!
//! ~keep A top-level module for the same reason as `ssrf_intercept`: the interact backend
//! ~keep (`interact::chromiumoxide`, gated on `browser-chromiumoxide`) and the render
//! ~keep (`browser::navigation`, gated on `browser`) both read the committed frame. Only a module
//! ~keep gated on the narrower feature is reachable from both.

use std::future::Future;
use std::time::Duration;

use chromiumoxide::cdp::browser_protocol::dom::{NodeId, QuerySelectorParams};
use chromiumoxide::cdp::browser_protocol::page::GetFrameTreeParams;
use chromiumoxide::error::CdpError;

use crate::error::CrawlError;

/// How many times a page is read when a new document commits during each read.
const READ_ATTEMPTS: usize = 3;

const SELECTOR_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Wait until `selector` matches the current document.
pub(crate) async fn wait_for_selector(page: &chromiumoxide::Page, selector: &str) -> Result<(), CdpError> {
    loop {
        let root = page.get_document().await?.node_id;
        #[cfg(test)]
        navigate_after_selector_root(page).await;
        let matched = match page.execute(QuerySelectorParams::new(root, selector)).await {
            Ok(response) => response.result.node_id,
            Err(error) if is_stale_selector_root_error(&error) => {
                let current_root = page.get_document().await?.node_id;
                if current_root != root {
                    #[cfg(test)]
                    record_stale_selector_root_retry();
                    continue;
                }
                return Err(error);
            }
            Err(error) => return Err(error),
        };
        if matched != NodeId::default() {
            return Ok(());
        }
        #[cfg(test)]
        run_after_selector_miss(page).await;
        tokio::time::sleep(SELECTOR_POLL_INTERVAL).await;
    }
}

fn is_stale_selector_root_error(error: &CdpError) -> bool {
    matches!(
        error,
        CdpError::Chrome(error)
            if error.code == -32000 && error.message == "Could not find node with given id"
    )
}

#[cfg(test)]
tokio::task_local! {
    /// A URL committed once between a selector wait's root lookup and query. ~keep
    pub(crate) static NAVIGATE_AFTER_SELECTOR_ROOT: std::cell::Cell<Option<String>>;

    /// The stale-root recovery count for a selector-wait test. ~keep
    pub(crate) static STALE_SELECTOR_ROOT_RETRIES: std::cell::Cell<usize>;

    /// JavaScript run once, and its run count, after a selector's first confirmed miss. ~keep
    static AFTER_SELECTOR_MISS_SCRIPT: std::cell::Cell<Option<String>>;
    static AFTER_SELECTOR_MISS_RUNS: std::cell::Cell<usize>;
}

#[cfg(test)]
fn record_stale_selector_root_retry() {
    let _ = STALE_SELECTOR_ROOT_RETRIES.try_with(|retries| retries.set(retries.get() + 1));
}

#[cfg(test)]
async fn run_after_selector_miss(page: &chromiumoxide::Page) {
    let script = AFTER_SELECTOR_MISS_SCRIPT
        .try_with(std::cell::Cell::take)
        .ok()
        .flatten();
    if let Some(script) = script {
        let _ = AFTER_SELECTOR_MISS_RUNS.try_with(|runs| runs.set(runs.get() + 1));
        page.evaluate(script)
            .await
            .expect("the selector-miss test hook must run");
    }
}

#[cfg(test)]
pub(crate) async fn with_after_selector_miss<T>(
    script: impl Into<String>,
    future: impl Future<Output = T>,
) -> (T, usize) {
    AFTER_SELECTOR_MISS_RUNS
        .scope(std::cell::Cell::new(0), async {
            let output = AFTER_SELECTOR_MISS_SCRIPT
                .scope(std::cell::Cell::new(Some(script.into())), future)
                .await;
            let runs = AFTER_SELECTOR_MISS_RUNS.with(std::cell::Cell::get);
            (output, runs)
        })
        .await
}

#[cfg(test)]
async fn navigate_after_selector_root(page: &chromiumoxide::Page) {
    if let Some(url) = NAVIGATE_AFTER_SELECTOR_ROOT
        .try_with(std::cell::Cell::take)
        .ok()
        .flatten()
    {
        page.goto(url).await.expect("the test navigation must load");
    }
}

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
    let document = page
        .execute(GetFrameTreeParams::default())
        .await
        .map(|tree| {
            let frame = &tree.result.frame_tree.frame;
            CommittedDocument {
                loader_id: frame.loader_id.clone().into(),
                unreachable_url: frame.unreachable_url.clone(),
                url: format!("{}{}", frame.url, frame.url_fragment.as_deref().unwrap_or_default()),
            }
        })
        .map_err(|e| CrawlError::browser_error(format!("failed to read the committed document: {e}")))?;
    #[cfg(test)]
    navigate_after_document_read(page).await;
    Ok(document)
}

/// The HTML of `page`. `what` names the read in its error.
pub(crate) async fn page_content(page: &chromiumoxide::Page, what: &str) -> Result<String, CrawlError> {
    let html = page
        .content()
        .await
        .map_err(|e| CrawlError::browser_error(format!("failed to {what}: {e}")))?;
    #[cfg(test)]
    navigate_after_content(page).await;
    Ok(html)
}

#[cfg(test)]
tokio::task_local! {
    /// A URL the page navigates to once, right after the next HTML read in this task. A test sets
    /// it to commit a new document between a read of the HTML and a read of the document.
    pub(crate) static NAVIGATE_AFTER_CONTENT: std::cell::Cell<Option<String>>;
}

#[cfg(test)]
async fn navigate_after_content(page: &chromiumoxide::Page) {
    if let Some(url) = NAVIGATE_AFTER_CONTENT.try_with(std::cell::Cell::take).ok().flatten() {
        page.goto(url).await.expect("the test navigation must load");
    }
}

#[cfg(test)]
tokio::task_local! {
    /// A count and a URL: the page navigates to the URL once, right after that many more reads of
    /// its committed document in this task. A test sets it to commit a new document after a read
    /// bound to one document has closed.
    pub(crate) static NAVIGATE_AFTER_DOCUMENT_READS: std::cell::Cell<Option<(usize, String)>>;
}

#[cfg(test)]
async fn navigate_after_document_read(page: &chromiumoxide::Page) {
    let Ok(Some((reads, url))) = NAVIGATE_AFTER_DOCUMENT_READS.try_with(std::cell::Cell::take) else {
        return;
    };
    if reads > 1 {
        let _ = NAVIGATE_AFTER_DOCUMENT_READS.try_with(|hook| hook.set(Some((reads - 1, url))));
        return;
    }
    page.goto(url).await.expect("the test navigation must load");
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

/// [`read_one_document`] with one `budget` for all of its reads.
///
/// ~keep Each read runs a script in the page or asks the renderer for the frame tree, so the
/// ~keep renderer answers it only when its main thread is free. A page that keeps that thread busy
/// ~keep holds each read for as long as it runs, up to chromiumoxide's fixed 30 s CDP timeout, and
/// ~keep `read_one_document` makes up to seven: four of the committed document and three of the
/// ~keep page. One budget bounds them all, retries included.
/// ~keep xberg-io/crawlberg#567.
pub(crate) async fn read_one_document_within<T, D, R>(
    budget: Duration,
    read_document: impl FnMut() -> D,
    read: impl FnMut() -> R,
) -> Result<(T, CommittedDocument), CrawlError>
where
    D: Future<Output = Result<CommittedDocument, CrawlError>>,
    R: Future<Output = Result<T, CrawlError>>,
{
    tokio::time::timeout(budget, read_one_document(read_document, read))
        .await
        .unwrap_or_else(|_| {
            Err(CrawlError::browser_timeout(format!(
                "browser timed out after {budget:?} reading the committed document"
            )))
        })
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

    #[test]
    fn only_the_concrete_stale_node_protocol_error_is_retryable() {
        assert!(is_stale_selector_root_error(&CdpError::Chrome(
            chromiumoxide::types::Error {
                code: -32000,
                message: "Could not find node with given id".to_owned(),
            }
        )));
        assert!(!is_stale_selector_root_error(&CdpError::Chrome(
            chromiumoxide::types::Error {
                code: -32000,
                message: "DOM Error while querying".to_owned(),
            }
        )));
        assert!(!is_stale_selector_root_error(&CdpError::Chrome(
            chromiumoxide::types::Error {
                code: -32602,
                message: "Could not find node with given id".to_owned(),
            }
        )));
    }

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

    #[tokio::test]
    async fn a_read_past_the_budget_is_a_browser_timeout_naming_the_budget() {
        let read = read_one_document_within(
            Duration::from_millis(50),
            || std::future::ready(Ok(document("a"))),
            std::future::pending,
        );
        let result: Result<(String, CommittedDocument), CrawlError> =
            tokio::time::timeout(Duration::from_secs(5), read)
                .await
                .expect("the budget must end a read that never answers");
        let error = result.expect_err("a read that never answers must end at the budget");
        let CrawlError::BrowserTimeout { message, .. } = &error else {
            panic!("expected a browser timeout, got {error:?}");
        };
        assert!(
            message.contains("50ms") && message.contains("reading the committed document"),
            "{message}"
        );
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
