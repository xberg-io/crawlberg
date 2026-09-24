//! A single document navigation: robots policy, document fetch, sub-resources and lifecycle.

use url::Url;

use super::security::{escape_for_js_template_literal, subresource_allowed};
use super::{Page, PageError};
use crate::dom::{DomTree, parse_html};
use crate::lifecycle::LifecycleState;
use crate::net::Response;

const ROBOTS_OK_STATUS: u16 = 200;

/// Overall ceiling on the `networkidle` wait, independent of how quiet the page gets.
const NETWORK_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
/// How long the request count must stay at or below the threshold to count as idle.
const NETWORK_IDLE_SETTLE: std::time::Duration = std::time::Duration::from_millis(500);
/// Bound on one event-loop turn while waiting for network idle.
const NETWORK_IDLE_TICK: std::time::Duration = std::time::Duration::from_millis(50);
/// Poll interval while waiting for network idle on a page that has no JS realm to pump.
const NETWORK_IDLE_POLL_WITHOUT_JS: std::time::Duration = std::time::Duration::from_millis(100);
/// In-flight request count treated as idle by `WaitUntil::NetworkIdle0` / `NetworkIdle2`.
const NETWORK_IDLE0_THRESHOLD: u32 = 0;
const NETWORK_IDLE2_THRESHOLD: u32 = 2;
impl Page {
    pub(super) async fn navigate_single(
        &mut self,
        url_str: &str,
        wait_until: crate::lifecycle::WaitUntil,
        method: &str,
        body: &str,
    ) -> Result<(), PageError> {
        let url = Url::parse(url_str).map_err(|e| PageError::InvalidUrl(e.to_string()))?;

        self.lifecycle = LifecycleState::Loading;
        self.url = Some(url.clone());
        self.network_events.clear();

        self.enforce_robots(&url).await?;
        let response = self.fetch_document(&url, method, body).await?;

        self.record_network_event(
            url.as_str(),
            "GET",
            "Document",
            response.status,
            &response.headers,
            response.body.len(),
        );

        if !response.redirected_from.is_empty() {
            self.url = Some(response.url.clone());
        }

        let body_text = String::from_utf8_lossy(&response.body).to_string();
        let dom = parse_html(&body_text);
        self.title = dom
            .query_selector("title")
            .ok()
            .flatten()
            .map(|title_id| dom.text_content(title_id))
            .unwrap_or_default();

        let css_sources = self.load_stylesheets(&dom).await;

        self.dom = Some(dom);
        self.lifecycle = LifecycleState::DomContentLoaded;

        if wait_until == crate::lifecycle::WaitUntil::DomContentLoaded {
            self.init_js();
            return Ok(());
        }

        self.init_js();
        self.inject_stylesheets(&css_sources);
        self.load_iframe_sources();

        self.execute_scripts().await;

        if let Some(js) = &mut self.js
            && let Ok(new_title) = js.evaluate("document.title")
            && let Some(t) = new_title.as_str()
        {
            self.title = t.to_string();
        }

        self.lifecycle = LifecycleState::Loaded;
        self.wait_for_network_idle(wait_until).await;

        Ok(())
    }

    /// Apply the context's `robots.txt` policy, fetching and caching the file on first use.
    ///
    /// Does nothing when `obey_robots` is off or the URL has no host.
    async fn enforce_robots(&mut self, url: &Url) -> Result<(), PageError> {
        if !self.context.obey_robots || url.host_str().is_none() {
            return Ok(());
        }

        // ~keep Keyed and fetched by origin, not by host: RFC 9309 section 2.3 scopes a
        // ~keep robots.txt file to a scheme, a host and a port, so two ports on one host are
        // ~keep two files. A host key asks the wrong port and shares one answer between them.
        let origin = url.origin().ascii_serialization();
        if self.context.robots_cache.is_allowed(&origin, "/robots.txt") {
            let robots_url = format!("{origin}/robots.txt");
            if let Ok(robots_url) = Url::parse(&robots_url)
                && let Ok(resp) = self.http_client.fetch(&robots_url).await
                && resp.status == ROBOTS_OK_STATUS
            {
                let body = String::from_utf8_lossy(&resp.body);
                self.context
                    .robots_cache
                    .parse_and_store(&origin, &body, &self.context.user_agent);
            }
        }

        if !self.context.robots_cache.is_allowed(&origin, url.path()) {
            self.lifecycle = LifecycleState::Failed;
            return Err(PageError::NetworkError(format!("Blocked by robots.txt: {}", url)));
        }
        Ok(())
    }

    async fn fetch_document(&mut self, url: &Url, method: &str, body: &str) -> Result<Response, PageError> {
        let result = if method == "POST" {
            self.http_client.post_form(url, body).await
        } else {
            self.do_fetch(url).await
        };
        result.map_err(|e| {
            self.lifecycle = LifecycleState::Failed;
            PageError::NetworkError(e.to_string())
        })
    }

    /// Resolved hrefs of the `<link rel=stylesheet>` elements policy and interception permit.
    fn allowed_stylesheet_urls(&self, dom: &DomTree) -> Vec<String> {
        let hrefs: Vec<String> = dom
            .query_selector_all("link")
            .unwrap_or_default()
            .iter()
            .filter_map(|&nid| {
                let node = dom.get_node(nid)?;
                let rel = node.get_attribute("rel")?;
                if rel.to_lowercase() != "stylesheet" {
                    return None;
                }
                node.get_attribute("href").map(|s| s.to_string())
            })
            .collect();

        let mut allowed = Vec::new();
        for href in &hrefs {
            let full_url = self.resolve_subresource_url(href);
            if !subresource_allowed(self.url.as_ref(), &full_url) {
                tracing::warn!(
                    "blocking cross-scheme <link rel=stylesheet href>: page={} href={}",
                    self.url_string(),
                    full_url,
                );
                continue;
            }
            if self.should_block_url(&full_url) {
                tracing::info!("Blocked stylesheet by interception: {}", full_url);
                continue;
            }
            allowed.push(full_url);
        }
        allowed
    }

    /// Fetch every permitted stylesheet concurrently and record a network event for each.
    async fn load_stylesheets(&mut self, dom: &DomTree) -> Vec<String> {
        let client = self.http_client.clone();
        let css_futures: Vec<_> = self
            .allowed_stylesheet_urls(dom)
            .into_iter()
            .map(|url_str| {
                let client = client.clone();
                async move {
                    let parsed = Url::parse(&url_str).unwrap_or_else(|_| Url::parse("about:blank").unwrap());
                    match client.fetch(&parsed).await {
                        Ok(resp) => Some((url_str, resp)),
                        Err(e) => {
                            tracing::debug!("Failed to fetch stylesheet {}: {}", url_str, e);
                            None
                        }
                    }
                }
            })
            .collect();

        let mut css_sources = Vec::new();
        for (url_str, resp) in futures::future::join_all(css_futures).await.into_iter().flatten() {
            let css = String::from_utf8_lossy(&resp.body).to_string();
            self.record_network_event(
                &url_str,
                "GET",
                "Stylesheet",
                resp.status,
                &resp.headers,
                resp.body.len(),
            );
            css_sources.push(css);
        }
        css_sources
    }

    fn inject_stylesheets(&mut self, css_sources: &[String]) {
        if css_sources.is_empty() {
            return;
        }
        let Some(js) = &mut self.js else {
            return;
        };
        let combined_css = css_sources.join("\n");
        // ~keep Escape JS template literals fully so attacker-controlled CSS cannot break into executable JS.
        let escaped = escape_for_js_template_literal(&combined_css);
        let code = format!("globalThis.__crawlberg_css = `{}`;", escaped);
        let _ = js.execute_script("<css>", &code);
    }

    fn load_iframe_sources(&mut self) {
        if let Some(js) = &mut self.js {
            let _ = js.execute_script("<iframe-load>",
                "(function() { var iframes = document.querySelectorAll('iframe[src]'); for (var i = 0; i < iframes.length; i++) { var src = iframes[i].getAttribute('src'); if (src && src !== 'about:blank') iframes[i]._loadIframeSrc(src); } })()");
        }
    }

    /// Block until in-flight requests stay at or below the `wait_until` threshold for
    /// [`NETWORK_IDLE_SETTLE`], or until [`NETWORK_IDLE_TIMEOUT`] elapses. A no-op for the
    /// non-`networkidle` wait conditions.
    async fn wait_for_network_idle(&mut self, wait_until: crate::lifecycle::WaitUntil) {
        let threshold = match wait_until {
            crate::lifecycle::WaitUntil::NetworkIdle0 => NETWORK_IDLE0_THRESHOLD,
            crate::lifecycle::WaitUntil::NetworkIdle2 => NETWORK_IDLE2_THRESHOLD,
            _ => return,
        };

        let deadline = tokio::time::Instant::now() + NETWORK_IDLE_TIMEOUT;
        let mut idle_since: Option<tokio::time::Instant> = None;

        loop {
            let active = self.http_client.active_requests();
            let now = tokio::time::Instant::now();

            if active <= threshold {
                let since = *idle_since.get_or_insert(now);
                if now.duration_since(since) >= NETWORK_IDLE_SETTLE {
                    break;
                }
            } else {
                idle_since = None;
            }

            if now >= deadline {
                tracing::debug!("Network idle timeout reached with {} active requests", active);
                break;
            }

            if let Some(js) = &mut self.js {
                let _ = tokio::time::timeout(NETWORK_IDLE_TICK, js.run_event_loop()).await;
            } else {
                tokio::time::sleep(NETWORK_IDLE_POLL_WITHOUT_JS).await;
            }
        }

        self.lifecycle = LifecycleState::NetworkIdle;
    }
}
