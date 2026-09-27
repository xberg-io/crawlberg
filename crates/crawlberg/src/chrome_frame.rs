//! What the main frame of a Chromiumoxide page has committed.
//!
//! ~keep A top-level module for the same reason as `ssrf_intercept`: the interact backend
//! ~keep (`interact::chromiumoxide`, gated on `browser-chromiumoxide`) reads the committed frame,
//! ~keep and the render (`browser::navigation`, gated on `browser`) can reach it too. Only a module
//! ~keep gated on the narrower feature is reachable from both. The render keeps its own copy in
//! ~keep #331 until one of the two changes merges.

use chromiumoxide::cdp::browser_protocol::page::{Frame, GetFrameTreeParams};

use crate::error::CrawlError;

/// The main frame of `page` and the document it has committed.
///
/// When the frame shows Chrome's own error page, its URL is `chrome-error://chromewebdata/` and
/// `unreachable_url` is the URL Chrome failed to show.
pub(crate) async fn committed_frame(page: &chromiumoxide::Page) -> Result<Frame, CrawlError> {
    page.execute(GetFrameTreeParams::default())
        .await
        .map(|tree| tree.result.frame_tree.frame.clone())
        .map_err(|e| CrawlError::browser_error(format!("failed to read the committed document: {e}")))
}
