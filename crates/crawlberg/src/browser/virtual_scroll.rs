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
const ROUND_TRIP_SLACK: Duration = Duration::from_secs(2);

const SCRIPT: &str = r#"(async (maxMs) => {
  const MAX_CONTAINERS = 8, MAX_STEPS = 200, MAX_ITEMS = 5000;
  const started = performance.now();
  const outOfTime = () => performance.now() - started > maxMs;
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
  const busiestList = (container) => {
    let best = container, count = container.children.length;
    for (const el of container.querySelectorAll('*')) {
      if (el.children.length > count) { best = el; count = el.children.length; }
    }
    return best;
  };
  const containers = Array.from(document.querySelectorAll('*')).filter(scrollable).slice(0, MAX_CONTAINERS);
  const stats = { containers: containers.length, steps: 0, items: 0, truncated: false };
  for (const container of containers) {
    if (outOfTime()) { stats.truncated = true; break; }
    const originalTop = container.scrollTop;
    let list = busiestList(container);
    const initialCount = list.children.length;
    const seen = new Map();
    let sequence = 0;
    const harvest = () => {
      if (!list.isConnected) list = busiestList(container);
      const box = container.getBoundingClientRect();
      for (const child of list.children) {
        const html = child.outerHTML;
        if (seen.has(html)) continue;
        const position = child.getBoundingClientRect().top - box.top + container.scrollTop;
        seen.set(html, { html, position, sequence: sequence++ });
      }
    };
    container.scrollTop = 0;
    await settle();
    harvest();
    while (container.scrollTop + container.clientHeight < container.scrollHeight - 1) {
      if (stats.steps >= MAX_STEPS || seen.size >= MAX_ITEMS || outOfTime()) { stats.truncated = true; break; }
      const before = container.scrollTop;
      container.scrollTop = before + Math.max(1, Math.floor(container.clientHeight * 0.8));
      if (container.scrollTop === before) break;
      stats.steps++;
      await settle();
      harvest();
    }
    container.scrollTop = originalTop;
    await settle();
    if (seen.size > initialCount && list.isConnected) {
      const ordered = Array.from(seen.values()).sort((a, b) => a.position - b.position || a.sequence - b.sequence);
      list.innerHTML = ordered.map((item) => item.html).join('');
      stats.items += ordered.length;
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
        Err(_) => tracing::warn!("scrolling containers timed out; reading the page as it is"),
    }
}
