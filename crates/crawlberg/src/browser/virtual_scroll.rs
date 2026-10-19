//! Collect the items of virtualised lists before the page is read.
//!
//! A virtual list mounts only the items near the viewport of its scroll container and unmounts
//! the rest, so the HTML read after load holds the first screen only (#768). This step scrolls each
//! overflowing container in turn, remembers every item it sees, and writes the union back into the
//! list in position order. Opt-in through `browser.scroll_containers`.

use std::time::Duration;

/// Wall-clock budget of the script, also enforced inside it.
const SCRIPT_BUDGET: Duration = Duration::from_secs(5);
/// Slack for the CDP round trip on top of the script's own budget.
const ABORT_TIMEOUT: Duration = Duration::from_millis(500);
const ROUND_TRIP_SLACK: Duration = Duration::from_secs(2);

const SCRIPT: &str = r#"(async (maxMs) => {
  const MAX_CONTAINERS = 8, MAX_STEPS = 200, MAX_ITEMS = 5000;
  const KEY_ATTRIBUTES = ['id', 'data-index', 'data-key', 'data-id', 'aria-posinset', 'aria-rowindex'];
  window.__crawlbergScrollAbort = false;
  const started = performance.now();
  const outOfTime = () => window.__crawlbergScrollAbort || performance.now() - started > maxMs;
  const settle = async () => {
    await new Promise((r) => setTimeout(r, 60));
    await new Promise((r) => { requestAnimationFrame(r); setTimeout(r, 40); });
  };
  const scrollable = (el) => {
    if (el === document.body || el === document.documentElement) return false;
    if (el.clientHeight < 1 || el.scrollHeight <= el.clientHeight + 1) return false;
    const overflow = getComputedStyle(el).overflowY;
    return overflow === 'auto' || overflow === 'scroll' || overflow === 'overlay';
  };
  const repetition = (el) => {
    const tags = new Map();
    let best = 0;
    for (const child of el.children) {
      const n = (tags.get(child.tagName) || 0) + 1;
      tags.set(child.tagName, n);
      if (n > best) best = n;
    }
    return best;
  };
  const busiestList = (container) => {
    let best = container, count = repetition(container);
    for (const el of container.querySelectorAll('*')) {
      const n = repetition(el);
      if (n > count) { best = el; count = n; }
    }
    return best;
  };
  const containers = Array.from(document.querySelectorAll('*')).filter(scrollable).slice(0, MAX_CONTAINERS);
  const stats = { containers: containers.length, steps: 0, items: 0, truncated: false, rewritten: 0 };
  for (const container of containers) {
    if (!container.isConnected) continue;
    if (outOfTime()) { stats.truncated = true; break; }
    const originalTop = container.scrollTop;
    const originalBehavior = container.style.scrollBehavior;
    const hadStyleAttribute = container.hasAttribute('style');
    container.style.scrollBehavior = 'auto';
    try {
      let list = busiestList(container);
      const seen = new Map();
      const initialKeys = new Set();
      let sequence = 0;
      const keyOf = (child, position) => {
        for (const name of KEY_ATTRIBUTES) {
          const value = child.getAttribute(name);
          if (value) return name + '=' + value;
        }
        return Math.round(position) + '|' + child.textContent;
      };
      const harvest = (initial) => {
        if (!list.isConnected) list = busiestList(container);
        const box = container.getBoundingClientRect();
        for (const child of list.children) {
          const position = child.getBoundingClientRect().top - box.top + container.scrollTop;
          const key = keyOf(child, position);
          if (initial) initialKeys.add(key);
          const entry = seen.get(key);
          if (entry) { entry.element = child; continue; }
          seen.set(key, { html: child.outerHTML, position, sequence: sequence++, element: child });
        }
      };
      harvest(true);
      container.scrollTop = 0;
      await settle();
      harvest(false);
      while (container.scrollTop + container.clientHeight < container.scrollHeight - 1) {
        if (stats.steps >= MAX_STEPS || seen.size >= MAX_ITEMS || outOfTime()) { stats.truncated = true; break; }
        const before = container.scrollTop;
        container.scrollTop = before + Math.max(1, Math.floor(container.clientHeight * 0.8));
        if (container.scrollTop === before) break;
        stats.steps++;
        await settle();
        harvest(false);
      }
      const entries = Array.from(seen.values());
      const virtualised = entries.some((e) => !e.element.isConnected);
      const gained = Array.from(seen.keys()).some((k) => !initialKeys.has(k));
      container.scrollTop = originalTop;
      await settle();
      if (virtualised && gained && !outOfTime() && container.isConnected && list.isConnected) {
        entries.sort((a, b) => a.position - b.position || a.sequence - b.sequence);
        for (const child of Array.from(list.children)) child.remove();
        list.insertAdjacentHTML('beforeend', entries.map((e) => e.html).join(''));
        stats.items += entries.length;
        stats.rewritten++;
      }
    } finally {
      container.style.scrollBehavior = originalBehavior;
      if (!hadStyleAttribute && container.getAttribute('style') === '') container.removeAttribute('style');
      container.scrollTop = originalTop;
    }
  }
  return stats;
})"#;

/// Scroll the overflowing containers of `page` and write the items they mounted back into their
/// lists. Never fails the page: a script error or timeout is logged and the page is read as it is.
pub(super) async fn harvest_scroll_containers(page: &chromiumoxide::Page) {
    let expression = format!("{SCRIPT}({})", SCRIPT_BUDGET.as_millis());
    match tokio::time::timeout(SCRIPT_BUDGET + ROUND_TRIP_SLACK, page.evaluate(expression)).await {
        Ok(Ok(result)) => {
            let stats: serde_json::Value = result.into_value().unwrap_or_default();
            tracing::debug!(%stats, "scrolled containers for virtualised content");
        }
        Ok(Err(error)) => tracing::warn!(%error, "scrolling containers failed; reading the page as it is"),
        Err(_) => {
            tracing::warn!("scrolling containers timed out; reading the page as it is");
            // ~keep Dropping the evaluate future does not stop the script; the flag makes it skip
            // ~keep its write-back so the DOM is not rewritten during or after the page read.
            let _ = tokio::time::timeout(ABORT_TIMEOUT, page.evaluate("window.__crawlbergScrollAbort = true")).await;
        }
    }
}
