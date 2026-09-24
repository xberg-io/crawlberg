//! Characterization tests for the navigation and script-execution pipeline.

use super::*;
use crate::net::ssrf::SsrfValidator;
use std::collections::HashMap as StdHashMap;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Debug)]
struct AllowAll;

#[async_trait::async_trait]
impl SsrfValidator for AllowAll {
    async fn validate(&self, _url: &Url) -> Result<(), String> {
        Ok(())
    }
}

/// Serves a fixed path -> (content-type, body) map over loopback.
async fn serve(routes: StdHashMap<String, (String, String)>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let routes = routes.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 8192];
                let read = socket.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..read]).to_string();
                let path = request
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("/")
                    .split('?')
                    .next()
                    .unwrap_or("/")
                    .to_string();
                let response = match routes.get(&path) {
                    Some((content_type, body)) => format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        content_type,
                        body.len(),
                        body
                    ),
                    None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
                };
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.flush().await;
            });
        }
    });
    format!("http://{addr}")
}

fn routes(entries: &[(&str, &str, &str)]) -> StdHashMap<String, (String, String)> {
    entries
        .iter()
        .map(|(path, content_type, body)| ((*path).to_string(), ((*content_type).to_string(), (*body).to_string())))
        .collect()
}

fn test_page() -> Page {
    let context = BrowserContext::with_ssrf("test".to_string(), None, false, None, Arc::new(AllowAll), false);
    Page::new("page-1".to_string(), Arc::new(context))
}

/// Reads a `globalThis` value back out of the page's realm after navigation.
fn global(page: &mut Page, expression: &str) -> serde_json::Value {
    page.evaluate(expression)
}

fn order(page: &mut Page) -> Vec<String> {
    global(page, "(globalThis.order || []).join(',')")
        .as_str()
        .unwrap_or("")
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

/// Emits a script that appends `tag` to `globalThis.order`.
fn push(tag: &str) -> String {
    format!("(globalThis.order = globalThis.order || []).push('{tag}');")
}

#[tokio::test(flavor = "current_thread")]
async fn classic_scripts_run_regular_then_deferred_then_async() {
    let html = format!(
        "<html><body>\
         <script>{}</script>\
         <script defer>{}</script>\
         <script async>{}</script>\
         <script>{}</script>\
         </body></html>",
        push("regular-1"),
        push("deferred"),
        push("async"),
        push("regular-2"),
    );
    let base = serve(routes(&[("/", "text/html", &html)])).await;

    let mut page = test_page();
    page.navigate(&base).await.expect("navigation must succeed");

    assert_eq!(
        order(&mut page),
        vec!["regular-1", "regular-2", "deferred", "async"],
        "document order within each class, then regular -> deferred -> async"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn external_scripts_are_fetched_executed_and_recorded_as_network_events() {
    let html = "<html><body><script src=\"/a.js\"></script><script src=\"/b.js\"></script></body></html>";
    let base = serve(routes(&[
        ("/", "text/html", html),
        (
            "/a.js",
            "application/javascript",
            "(globalThis.order = globalThis.order || []).push('a');",
        ),
        (
            "/b.js",
            "application/javascript",
            "(globalThis.order = globalThis.order || []).push('b');",
        ),
    ]))
    .await;

    let mut page = test_page();
    page.navigate(&base).await.expect("navigation must succeed");

    assert_eq!(order(&mut page), vec!["a", "b"]);
    let script_events: Vec<&NetworkEvent> = page
        .network_events
        .iter()
        .filter(|event| event.resource_type == "Script")
        .collect();
    assert_eq!(script_events.len(), 2, "one network event per fetched script");
    assert!(script_events.iter().all(|event| event.status == 200));
    assert!(script_events.iter().all(|event| event.body_size > 0));
}

#[tokio::test(flavor = "current_thread")]
async fn a_script_type_that_is_not_javascript_is_skipped() {
    let html = format!(
        "<html><body>\
         <script type=\"text/template\">{}</script>\
         <script type=\"application/json\">{}</script>\
         <script type=\"text/javascript\">{}</script>\
         <script type=\"application/javascript\">{}</script>\
         <script>{}</script>\
         </body></html>",
        push("template"),
        push("json"),
        push("text-js"),
        push("app-js"),
        push("bare"),
    );
    let base = serve(routes(&[("/", "text/html", &html)])).await;

    let mut page = test_page();
    page.navigate(&base).await.expect("navigation must succeed");

    assert_eq!(
        order(&mut page),
        vec!["text-js", "app-js", "bare"],
        "only javascript-typed and untyped scripts execute"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn module_scripts_run_after_every_classic_script() {
    let html = format!(
        "<html><body>\
         <script type=\"module\">{}</script>\
         <script>{}</script>\
         <script defer>{}</script>\
         </body></html>",
        push("module"),
        push("classic"),
        push("deferred"),
    );
    let base = serve(routes(&[("/", "text/html", &html)])).await;

    let mut page = test_page();
    page.navigate(&base).await.expect("navigation must succeed");

    assert_eq!(
        order(&mut page),
        vec!["classic", "deferred", "module"],
        "modules are deferred past all classic scripts regardless of document order"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_failing_script_does_not_stop_the_remaining_scripts() {
    let html = format!(
        "<html><body>\
         <script>{}</script>\
         <script>throw new Error('boom');</script>\
         <script src=\"/missing.js\"></script>\
         <script>{}</script>\
         </body></html>",
        push("before"),
        push("after"),
    );
    let base = serve(routes(&[("/", "text/html", &html)])).await;

    let mut page = test_page();
    page.navigate(&base).await.expect("navigation must still succeed");

    assert_eq!(order(&mut page), vec!["before", "after"]);
}

#[tokio::test(flavor = "current_thread")]
async fn a_script_blocked_by_interception_is_not_fetched_or_executed() {
    let html = "<html><body><script src=\"/blocked.js\"></script><script src=\"/ok.js\"></script></body></html>";
    let base = serve(routes(&[
        ("/", "text/html", html),
        (
            "/blocked.js",
            "application/javascript",
            "(globalThis.order = globalThis.order || []).push('blocked');",
        ),
        (
            "/ok.js",
            "application/javascript",
            "(globalThis.order = globalThis.order || []).push('ok');",
        ),
    ]))
    .await;

    let mut page = test_page();
    page.intercept_enabled = true;
    page.intercept_block_patterns = vec!["*blocked.js".to_string()];
    page.navigate(&base).await.expect("navigation must succeed");

    assert_eq!(order(&mut page), vec!["ok"]);
}

#[tokio::test(flavor = "current_thread")]
async fn stylesheets_are_collected_into_the_css_global() {
    let html = "<html><head>\
                <link rel=\"stylesheet\" href=\"/one.css\">\
                <link rel=\"icon\" href=\"/not-a-stylesheet.css\">\
                <link rel=\"stylesheet\" href=\"/two.css\">\
                </head><body></body></html>";
    let base = serve(routes(&[
        ("/", "text/html", html),
        ("/one.css", "text/css", "a{color:red}"),
        ("/two.css", "text/css", "b{color:blue}"),
        ("/not-a-stylesheet.css", "text/css", "SHOULD-NOT-APPEAR"),
    ]))
    .await;

    let mut page = test_page();
    page.navigate(&base).await.expect("navigation must succeed");

    let css = global(&mut page, "globalThis.__crawlberg_css");
    let css = css.as_str().expect("css global must be a string");
    assert_eq!(
        css, "a{color:red}\nb{color:blue}",
        "only rel=stylesheet, joined by newline"
    );

    let stylesheet_events = page
        .network_events
        .iter()
        .filter(|event| event.resource_type == "Stylesheet")
        .count();
    assert_eq!(stylesheet_events, 2);
}

#[tokio::test(flavor = "current_thread")]
async fn css_containing_a_template_literal_break_out_is_escaped() {
    let base = serve(routes(&[
        (
            "/",
            "text/html",
            "<html><head><link rel=\"stylesheet\" href=\"/x.css\"></head><body></body></html>",
        ),
        ("/x.css", "text/css", "a{}` + (globalThis.pwned = 1) + `"),
    ]))
    .await;

    let mut page = test_page();
    page.navigate(&base).await.expect("navigation must succeed");

    assert!(
        global(&mut page, "globalThis.pwned").is_null(),
        "CSS must not be able to break out of the template literal"
    );
    let css = global(&mut page, "globalThis.__crawlberg_css");
    assert_eq!(css.as_str(), Some("a{}` + (globalThis.pwned = 1) + `"));
}

#[tokio::test(flavor = "current_thread")]
async fn wait_until_domcontentloaded_returns_before_any_script_runs() {
    let html = format!("<html><body><script>{}</script></body></html>", push("ran"));
    let base = serve(routes(&[("/", "text/html", &html)])).await;

    let mut page = test_page();
    page.navigate_with_wait(&base, crate::lifecycle::WaitUntil::DomContentLoaded)
        .await
        .expect("navigation must succeed");

    assert_eq!(page.lifecycle, LifecycleState::DomContentLoaded);
    assert_eq!(order(&mut page), Vec::<String>::new(), "scripts must not have run");
}

#[tokio::test(flavor = "current_thread")]
async fn the_title_comes_from_the_title_element() {
    let base = serve(routes(&[(
        "/",
        "text/html",
        "<html><head><title>  Hello World  </title></head><body></body></html>",
    )]))
    .await;

    let mut page = test_page();
    page.navigate(&base).await.expect("navigation must succeed");

    assert_eq!(page.title, "  Hello World  ");
}

#[tokio::test(flavor = "current_thread")]
async fn a_document_network_event_is_recorded_with_the_response_status() {
    let base = serve(routes(&[("/", "text/html", "<html><body>hi</body></html>")])).await;

    let mut page = test_page();
    page.navigate(&base).await.expect("navigation must succeed");

    let document_events: Vec<&NetworkEvent> = page
        .network_events
        .iter()
        .filter(|event| event.resource_type == "Document")
        .collect();
    assert_eq!(document_events.len(), 1);
    assert_eq!(document_events[0].status, 200);
    assert_eq!(document_events[0].method, "GET");
}

#[tokio::test(flavor = "current_thread")]
async fn robots_txt_disallow_blocks_the_navigation() {
    let base = serve(routes(&[
        ("/", "text/html", "<html><body>ok</body></html>"),
        ("/robots.txt", "text/plain", "User-agent: *\nDisallow: /"),
    ]))
    .await;

    let context = BrowserContext::with_ssrf("test".to_string(), None, false, None, Arc::new(AllowAll), false);
    let context = BrowserContext {
        obey_robots: true,
        ..context
    };
    let mut page = Page::new("page-1".to_string(), Arc::new(context));

    let error = page.navigate(&base).await.expect_err("robots.txt must block");
    assert!(
        matches!(error, PageError::NetworkError(ref message) if message.contains("Blocked by robots.txt")),
        "expected a robots.txt block, got {error:?}"
    );
    assert_eq!(page.lifecycle, LifecycleState::Failed);
}

#[tokio::test(flavor = "current_thread")]
async fn robots_txt_allow_permits_the_navigation() {
    let base = serve(routes(&[
        (
            "/",
            "text/html",
            "<html><head><title>allowed</title></head><body></body></html>",
        ),
        ("/robots.txt", "text/plain", "User-agent: *\nDisallow: /private"),
    ]))
    .await;

    let context = BrowserContext::with_ssrf("test".to_string(), None, false, None, Arc::new(AllowAll), false);
    let context = BrowserContext {
        obey_robots: true,
        ..context
    };
    let mut page = Page::new("page-1".to_string(), Arc::new(context));

    page.navigate(&base).await.expect("navigation must be permitted");
    assert_eq!(page.title, "allowed");
}

#[tokio::test(flavor = "current_thread")]
async fn navigation_resets_the_network_events_of_the_previous_page() {
    let base = serve(routes(&[
        (
            "/",
            "text/html",
            "<html><head><link rel=\"stylesheet\" href=\"/a.css\"></head><body></body></html>",
        ),
        ("/a.css", "text/css", "a{}"),
        ("/plain", "text/html", "<html><body></body></html>"),
    ]))
    .await;

    let mut page = test_page();
    page.navigate(&base).await.expect("first navigation");
    assert!(page.network_events.len() >= 2, "document plus stylesheet");

    page.navigate(&format!("{base}/plain"))
        .await
        .expect("second navigation");
    assert_eq!(page.network_events.len(), 1, "only the second document remains");
}

/// Characterizes `js::ops::op_dom`, the 33-command DOM dispatcher, through the JS API that
/// `bootstrap.js` layers on top of it — the only place its behaviour is observable.
#[tokio::test(flavor = "current_thread")]
async fn the_dom_operation_dispatcher_answers_every_command_group() {
    let script = r#"
      const out = {};
      out.title = document.title;
      out.byId = document.getElementById('target').tagName;
      out.qs = document.querySelector('.item').tagName;
      out.qsaLen = document.querySelectorAll('.item').length;
      out.nodeType = document.getElementById('target').nodeType;
      out.nodeName = document.getElementById('target').nodeName;
      out.text = document.getElementById('target').textContent;
      out.attr = document.getElementById('target').getAttribute('data-x');
      out.childCount = document.getElementById('list').childNodes.length;
      out.elemChildren = document.getElementById('list').children.length;
      out.hasChildren = document.getElementById('list').hasChildNodes();
      out.innerBefore = document.getElementById('target').innerHTML;
      out.outer = document.getElementById('target').outerHTML;
      const created = document.createElement('span');
      created.setAttribute('id', 'made');
      created.textContent = 'hi';
      document.getElementById('list').appendChild(created);
      out.afterAppend = document.getElementById('list').children.length;
      out.foundMade = document.getElementById('made') ? 'yes' : 'no';
      out.contains = document.getElementById('list').contains(created);
      out.txtType = document.createTextNode('tnode').nodeType;
      document.getElementById('target').innerHTML = '<b>bold</b>';
      out.innerAfter = document.getElementById('target').innerHTML;
      out.firstChildName = document.getElementById('list').firstChild.nodeName;
      out.parentName = created.parentNode.nodeName;
      document.getElementById('target').removeAttribute('data-x');
      out.attrAfterRemove = document.getElementById('target').getAttribute('data-x');
      document.getElementById('list').removeChild(created);
      out.afterRemove = document.getElementById('list').children.length;
      out.docUrlIsString = typeof document.URL === 'string';
      globalThis.probe = JSON.stringify(out);
    "#;
    let html = format!(
        "<html><head><title>T</title></head><body>\
         <div id=\"target\" data-x=\"vx\" class=\"item\">hello</div>\
         <ul id=\"list\"><li class=\"item\">a</li><li class=\"item\">b</li></ul>\
         <script>{script}</script></body></html>"
    );
    let base = serve(routes(&[("/", "text/html", &html)])).await;
    let mut page = test_page();
    page.navigate(&base).await.expect("navigate");

    let probe = global(&mut page, "globalThis.probe");
    let probe: serde_json::Value =
        serde_json::from_str(probe.as_str().expect("probe must be a JSON string")).expect("probe must parse");

    let expected = serde_json::json!({
        "title": "T",
        "byId": "DIV",
        "qs": "DIV",
        "qsaLen": 3,
        "nodeType": 1,
        "nodeName": "DIV",
        "text": "hello",
        "attr": "vx",
        "childCount": 2,
        "elemChildren": 2,
        "hasChildren": true,
        "innerBefore": "hello",
        "outer": "<div id=\"target\" data-x=\"vx\" class=\"item\">hello</div>",
        "afterAppend": 3,
        "foundMade": "yes",
        "contains": true,
        "txtType": 3,
        "innerAfter": "<b>bold</b>",
        "firstChildName": "LI",
        "parentName": "UL",
        "attrAfterRemove": null,
        "afterRemove": 2,
        "docUrlIsString": true,
    });
    assert_eq!(probe, expected);
}

/// Second half of the `op_dom` characterization: sibling navigation, node constructors and
/// the doctype/documentElement accessors.
///
/// `orderAfterInsert` pins a pre-existing quirk rather than the spec behaviour: `insertBefore`
/// currently does not attach the new node, so the list is unchanged and the new node has no
/// siblings. Verified byte-identical against the pre-refactor `op_dom`, so this is the
/// behaviour as shipped, not a regression -- a fix here must update this expectation on purpose.
#[tokio::test(flavor = "current_thread")]
async fn the_dom_dispatcher_handles_siblings_constructors_and_doctype() {
    let script = r#"
      const out = {};
      const list = document.getElementById('list');
      const made = document.createElement('span');
      made.setAttribute('id', 'ins');
      list.insertBefore(made, list.children[1]);
      out.orderAfterInsert = Array.from(list.children).map(c => c.getAttribute('id') || c.tagName).join('|');
      out.lastChildName = list.lastChild.nodeName;
      out.nextSiblingId = made.nextSibling ? (made.nextSibling.getAttribute('id') || made.nextSibling.tagName) : null;
      out.prevSiblingId = made.previousSibling ? (made.previousSibling.getAttribute('id') || made.previousSibling.tagName) : null;
      out.commentType = document.createComment('note').nodeType;
      out.fragType = document.createDocumentFragment().nodeType;
      out.doctype = document.doctype ? document.doctype.name : null;
      out.docElem = document.documentElement.tagName;
      globalThis.probe2 = JSON.stringify(out);
    "#;
    let html = format!(
        "<!DOCTYPE html><html><head><title>T</title></head><body>\
         <ul id=\"list\"><li id=\"a\">a</li><li id=\"b\">b</li><li id=\"c\">c</li></ul>\
         <script>{script}</script></body></html>"
    );
    let base = serve(routes(&[("/", "text/html", &html)])).await;
    let mut page = test_page();
    page.navigate(&base).await.expect("navigate");

    let probe = global(&mut page, "globalThis.probe2");
    let probe: serde_json::Value =
        serde_json::from_str(probe.as_str().expect("probe must be a JSON string")).expect("probe must parse");

    assert_eq!(
        probe,
        serde_json::json!({
            "orderAfterInsert": "a|b|c",
            "lastChildName": "LI",
            "nextSiblingId": null,
            "prevSiblingId": null,
            "commentType": 8,
            "fragType": 11,
            "doctype": "html",
            "docElem": "HTML",
        })
    );
}

/// Serves a fixed path -> raw HTTP response map, so a test can return redirects and
/// arbitrary headers that `serve()` cannot express.
async fn serve_raw(responses: StdHashMap<String, String>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let responses = responses.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 8192];
                let read = socket.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..read]).to_string();
                let path = request.split_whitespace().nth(1).unwrap_or("/").to_string();
                let fallback = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string();
                let response = responses.get(&path).unwrap_or(&fallback);
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.flush().await;
            });
        }
    });
    format!("http://{addr}")
}

fn raw(entries: &[(&str, &str)]) -> StdHashMap<String, String> {
    entries
        .iter()
        .map(|(path, response)| ((*path).to_string(), (*response).to_string()))
        .collect()
}

fn ok_response(content_type: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        content_type,
        body.len(),
        body
    )
}

/// Runs `script` in a page served from `base_routes`, then returns the JSON its fetch stored.
async fn fetch_result(page: &mut Page, base: &str) -> serde_json::Value {
    let raw = global(page, "globalThis.fr");
    let raw = raw.as_str().unwrap_or("<<missing>>");
    let _ = base;
    serde_json::from_str(raw).unwrap_or_else(|_| serde_json::Value::String(raw.to_string()))
}

/// Script body that stores the outcome of one `fetch` into `globalThis.fr` as JSON.
fn fetch_script(target: &str, init: &str) -> String {
    format!(
        r#"globalThis.fr = '"pending"';
           fetch('{target}'{init}).then(r => r.text().then(t => {{
             globalThis.fr = JSON.stringify({{
               status: r.status, text: t, blocked: !!r.__blocked, cors: !!r.__cors
             }});
           }})).catch(e => {{ globalThis.fr = JSON.stringify({{error: String(e)}}); }});"#
    )
}

#[tokio::test(flavor = "current_thread")]
async fn a_same_origin_fetch_returns_the_status_and_body() {
    let script = fetch_script("/data.json", "");
    let html = format!("<html><body><script>{script}</script></body></html>");
    let base = serve(routes(&[
        ("/", "text/html", &html),
        ("/data.json", "application/json", "{\"k\":1}"),
    ]))
    .await;
    let mut page = test_page();
    page.navigate(&base).await.expect("navigate");

    let result = fetch_result(&mut page, &base).await;
    assert_eq!(result["status"], 200);
    assert_eq!(result["text"], "{\"k\":1}");
}

#[tokio::test(flavor = "current_thread")]
async fn a_fetch_follows_a_redirect_to_the_final_response() {
    let script = fetch_script("/start", "");
    let html = format!("<html><body><script>{script}</script></body></html>");
    let base = serve_raw(raw(&[
        ("/", &ok_response("text/html", &html)),
        (
            "/start",
            "HTTP/1.1 302 Found\r\nLocation: /end\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        ),
        ("/end", &ok_response("text/plain", "arrived")),
    ]))
    .await;
    let mut page = test_page();
    page.navigate(&base).await.expect("navigate");

    let result = fetch_result(&mut page, &base).await;
    assert_eq!(result["status"], 200);
    assert_eq!(result["text"], "arrived");
}

#[tokio::test(flavor = "current_thread")]
async fn a_fetch_redirect_loop_stops_at_the_hop_limit() {
    let script = fetch_script("/loop", "");
    let html = format!("<html><body><script>{script}</script></body></html>");
    let base = serve_raw(raw(&[
        ("/", &ok_response("text/html", &html)),
        (
            "/loop",
            "HTTP/1.1 302 Found\r\nLocation: /loop\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        ),
    ]))
    .await;
    let mut page = test_page();
    page.navigate(&base).await.expect("navigate");

    // ~keep A status-0 "blocked" payload is surfaced to page JS as a rejected promise by
    // bootstrap.js, not as a Response, so the hop limit is only observable as this rejection.
    let result = fetch_result(&mut page, &base).await;
    let error = result["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("ERR_FAILED"),
        "the redirect hop limit must reject the fetch, got {result:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_cross_origin_fetch_without_an_allow_origin_header_is_cors_blocked() {
    let other = serve(routes(&[("/x", "text/plain", "secret")])).await;
    let script = fetch_script(&format!("{other}/x"), "");
    let html = format!("<html><body><script>{script}</script></body></html>");
    let base = serve(routes(&[("/", "text/html", &html)])).await;
    let mut page = test_page();
    page.navigate(&base).await.expect("navigate");

    let result = fetch_result(&mut page, &base).await;
    let error = result["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("CORS error") && error.contains("not in Access-Control-Allow-Origin"),
        "the cross-origin response must be refused with the op's CORS message, got {result:?}"
    );
    assert!(!error.contains("secret"), "a CORS-blocked fetch never exposes the body");
}

#[tokio::test(flavor = "current_thread")]
async fn a_cross_origin_fetch_with_a_wildcard_allow_origin_header_succeeds() {
    let body = "shared";
    let allowed = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let other = serve_raw(raw(&[("/x", &allowed)])).await;
    let script = fetch_script(&format!("{other}/x"), "");
    let html = format!("<html><body><script>{script}</script></body></html>");
    let base = serve(routes(&[("/", "text/html", &html)])).await;
    let mut page = test_page();
    page.navigate(&base).await.expect("navigate");

    let result = fetch_result(&mut page, &base).await;
    assert_eq!(result["status"], 200);
    assert_eq!(result["text"], "shared");
}

#[tokio::test(flavor = "current_thread")]
async fn a_set_cookie_on_a_js_fetch_response_reaches_the_shared_jar() {
    let script = fetch_script("/setcookie", "");
    let html = format!("<html><body><script>{script}</script></body></html>");
    let with_cookie = "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nSet-Cookie: jsfetch=1\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
    let base = serve_raw(raw(&[
        ("/", &ok_response("text/html", &html)),
        ("/setcookie", with_cookie),
    ]))
    .await;
    let mut page = test_page();
    page.navigate(&base).await.expect("navigate");

    // The cookie is stored against the fetched path, so it is not returned for the page path.
    let stored = page.context.cookie_jar.snapshot();
    assert_eq!(
        stored,
        vec![(
            "jsfetch".to_string(),
            "1".to_string(),
            "127.0.0.1".to_string(),
            "/setcookie".to_string(),
            false,
            false
        )],
        "the fetch op must store Set-Cookie in the jar the page shares"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn wait_until_networkidle_settles_and_reports_the_networkidle_lifecycle() {
    let base = serve(routes(&[("/", "text/html", "<html><body>quiet</body></html>")])).await;

    let mut page = test_page();
    page.navigate_with_wait(&base, crate::lifecycle::WaitUntil::NetworkIdle0)
        .await
        .expect("navigation must succeed");

    assert_eq!(page.lifecycle, LifecycleState::NetworkIdle);
}

#[tokio::test(flavor = "current_thread")]
async fn a_non_networkidle_wait_leaves_the_lifecycle_at_loaded() {
    let base = serve(routes(&[("/", "text/html", "<html><body>quiet</body></html>")])).await;

    let mut page = test_page();
    page.navigate_with_wait(&base, crate::lifecycle::WaitUntil::Load)
        .await
        .expect("navigation must succeed");

    assert_eq!(page.lifecycle, LifecycleState::Loaded);
}
