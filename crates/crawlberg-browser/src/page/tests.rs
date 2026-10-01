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
    serve_on(TcpListener::bind("127.0.0.1:0").await.expect("bind"), routes)
}

/// Serves `routes` on an already bound listener, so a page can name its own absolute address.
fn serve_on(listener: TcpListener, routes: StdHashMap<String, (String, String)>) -> String {
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
    let context = BrowserContext::with_ssrf("test".to_string(), None, false, None, Arc::new(AllowAll), false)
        .expect("no proxy, so the context must build");
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

fn rendered_html(page: &Page) -> String {
    page.with_dom(|dom| dom.outer_html(dom.document()))
        .expect("the page must have a DOM")
}

#[tokio::test(flavor = "current_thread")]
async fn a_module_script_with_a_src_is_fetched_and_run() {
    let html = "<html><body><script type=\"module\" src=\"app.js\"></script></body></html>";
    let app = "const p = document.createElement('p');\
               p.setAttribute('id', 'from-module');\
               p.textContent = 'module ran';\
               document.body.appendChild(p);";
    let base = serve(routes(&[("/", "text/html", html), ("/app.js", "text/javascript", app)])).await;

    let mut page = test_page();
    page.navigate(&base).await.expect("navigation must succeed");

    let rendered = rendered_html(&page);
    assert!(
        rendered.contains("<p id=\"from-module\">module ran</p>"),
        "the module's code must run and add its element: {rendered}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_module_script_with_a_src_runs_the_modules_it_imports() {
    let html = "<html><body><script type=\"module\" src=\"/js/app.js\"></script></body></html>";
    let app = format!(
        "import {{ tag }} from './dep.js';\n{}\nglobalThis.imported = tag;",
        push("app")
    );
    let dep = format!("{}\nexport const tag = 'from-dep';", push("dep"));
    let base = serve(routes(&[
        ("/", "text/html", html),
        ("/js/app.js", "text/javascript", &app),
        ("/js/dep.js", "text/javascript", &dep),
    ]))
    .await;

    let mut page = test_page();
    page.navigate(&base).await.expect("navigation must succeed");

    assert_eq!(
        order(&mut page),
        vec!["dep", "app"],
        "the imported module runs first, resolved against the importing module's address"
    );
    assert_eq!(global(&mut page, "globalThis.imported"), serde_json::json!("from-dep"));
}

#[tokio::test(flavor = "current_thread")]
async fn a_module_that_fails_to_load_does_not_stop_the_page() {
    let html = format!(
        "<html><body><h1>still here</h1>\
         <script type=\"module\" src=\"/missing.js\"></script>\
         <script type=\"module\" src=\"/imports-missing.js\"></script>\
         <script type=\"module\">{}</script>\
         <script type=\"module\" src=\"/ok.js\"></script>\
         </body></html>",
        push("inline"),
    );
    let imports_missing = format!("import './gone.js';\n{}", push("imports-missing"));
    let ok = push("ok");
    let base = serve(routes(&[
        ("/", "text/html", &html),
        ("/imports-missing.js", "text/javascript", &imports_missing),
        ("/ok.js", "text/javascript", &ok),
    ]))
    .await;

    let mut page = test_page();
    page.navigate(&base).await.expect("navigation must still succeed");

    assert_eq!(
        order(&mut page),
        vec!["inline", "ok"],
        "a module that is missing, or imports one that is, must not run and must not stop the others"
    );
    assert!(rendered_html(&page).contains("<h1>still here</h1>"));
    assert_eq!(
        event_urls(&page, "Script"),
        vec![format!("{base}/ok.js")],
        "only the module that loaded is recorded as a script"
    );
}

/// Refuses every address whose path ends in `refused.js`, and allows the rest.
#[derive(Debug)]
struct RefuseRefusedJs;

#[async_trait::async_trait]
impl SsrfValidator for RefuseRefusedJs {
    async fn validate(&self, url: &Url) -> Result<(), String> {
        if url.path().ends_with("refused.js") {
            Err("refused by the test policy".to_string())
        } else {
            Ok(())
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_module_src_the_ssrf_policy_refuses_is_not_run() {
    let html = "<html><body>\
                <script type=\"module\" src=\"/refused.js\"></script>\
                <script type=\"module\" src=\"/ok.js\"></script>\
                </body></html>";
    let refused = push("refused");
    let ok = push("ok");
    let base = serve(routes(&[
        ("/", "text/html", html),
        ("/refused.js", "text/javascript", &refused),
        ("/ok.js", "text/javascript", &ok),
    ]))
    .await;

    let context = BrowserContext::with_ssrf("test".to_string(), None, false, None, Arc::new(RefuseRefusedJs), false)
        .expect("no proxy, so the context must build");
    let mut page = Page::new("page-1".to_string(), Arc::new(context));
    page.navigate(&base).await.expect("navigation must succeed");

    assert_eq!(order(&mut page), vec!["ok"]);
}

#[tokio::test(flavor = "current_thread")]
async fn a_module_src_blocked_by_interception_is_not_run() {
    let blocked = push("blocked");
    let ok = push("ok");
    let (origin, mut page) = navigate_intercepted(
        |_| {
            "<html><body><script type=\"module\" src=\"/blocked.js\"></script>\
             <script type=\"module\" src=\"/ok.js\"></script></body></html>"
                .to_string()
        },
        &[
            ("/blocked.js", "text/javascript", &blocked),
            ("/ok.js", "text/javascript", &ok),
        ],
    )
    .await;

    assert_eq!(order(&mut page), vec!["ok"]);
    assert_eq!(event_urls(&page, "Script"), vec![format!("{origin}/ok.js")]);
}

/// Navigates to `html` with interception blocking every address that ends in `blocked.js`, serving
/// each `(path, body)` as JavaScript, and returns the page with the request lines the server saw.
async fn navigate_intercepted_recording(html: &str, extra: &[(&str, &str)]) -> (Page, Vec<String>) {
    let responses: Vec<(&str, String)> = extra
        .iter()
        .map(|(path, body)| (*path, ok_response("text/javascript", body)))
        .collect();
    let responses: Vec<(&str, &str)> = responses
        .iter()
        .map(|(path, response)| (*path, response.as_str()))
        .collect();
    navigate_intercepted_raw(html, &responses).await
}

fn requested(requests: &[String], path: &str) -> bool {
    requests
        .iter()
        .any(|request| request.starts_with(&format!("GET {path} ")))
}

#[tokio::test(flavor = "current_thread")]
async fn a_module_script_import_blocked_by_interception_is_not_fetched() {
    let html = "<html><body><script type=\"module\" src=\"/app.js\"></script>\
                <script type=\"module\" src=\"/ok.js\"></script></body></html>";
    let app = format!("import './blocked.js';\n{}", push("app"));
    let (mut page, requests) = navigate_intercepted_recording(
        html,
        &[
            ("/app.js", &app),
            ("/blocked.js", &push("blocked")),
            ("/ok.js", &push("ok")),
        ],
    )
    .await;

    assert_eq!(
        order(&mut page),
        vec!["ok"],
        "a module that imports a blocked address must not run, and must not stop the others"
    );
    assert!(requested(&requests, "/app.js"), "{requests:?}");
    assert!(
        !requested(&requests, "/blocked.js"),
        "a blocked import is never requested: {requests:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn an_inline_module_import_blocked_by_interception_is_not_fetched() {
    let html = format!(
        "<html><body><script type=\"module\">import './blocked.js';\n{}</script>\
         <script type=\"module\" src=\"/ok.js\"></script></body></html>",
        push("inline"),
    );
    let (mut page, requests) =
        navigate_intercepted_recording(&html, &[("/blocked.js", &push("blocked")), ("/ok.js", &push("ok"))]).await;

    assert_eq!(order(&mut page), vec!["ok"]);
    assert!(
        !requested(&requests, "/blocked.js"),
        "a blocked import is never requested: {requests:?}"
    );
}

/// Serves `responses` beside the page `html`, navigates with interception blocking every address
/// that ends in `blocked.js`, and returns the page with the request lines the server saw.
async fn navigate_intercepted_raw(html: &str, responses: &[(&str, &str)]) -> (Page, Vec<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let page_response = ok_response("text/html", html);
    let mut entries = vec![("/", page_response.as_str())];
    entries.extend_from_slice(responses);
    let requests = serve_raw_recording(listener, raw(&entries));

    let mut page = test_page();
    page.intercept_enabled = true;
    page.intercept_block_patterns = vec!["*blocked.js".to_string()];
    page.navigate(&format!("http://{addr}/"))
        .await
        .expect("navigation must succeed");
    let requests = requests.lock().expect("lock").clone();
    (page, requests)
}

#[tokio::test(flavor = "current_thread")]
async fn a_module_redirect_to_a_blocked_address_is_not_fetched() {
    let html = "<html><body><script type=\"module\" src=\"/hop.js\"></script>\
                <script type=\"module\" src=\"/ok.js\"></script></body></html>";
    let redirect = "HTTP/1.1 302 Found\r\nLocation: /x/blocked.js\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    let (mut page, requests) = navigate_intercepted_raw(
        html,
        &[
            ("/hop.js", redirect),
            ("/x/blocked.js", &ok_response("text/javascript", &push("blocked"))),
            ("/ok.js", &ok_response("text/javascript", &push("ok"))),
        ],
    )
    .await;

    assert_eq!(order(&mut page), vec!["ok"]);
    assert!(requested(&requests, "/hop.js"), "{requests:?}");
    assert!(
        !requested(&requests, "/x/blocked.js"),
        "a redirect to a blocked address is never followed: {requests:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_dynamic_import_of_a_blocked_address_is_not_fetched() {
    let html = "<html><body><script>\
                import('/fine.js').then(() => { globalThis.fine = true; });\
                import('/blocked.js').then(() => { globalThis.blocked = 'loaded'; }, () => { globalThis.blocked = 'refused'; });\
                </script></body></html>";
    let (mut page, requests) = navigate_intercepted_raw(
        html,
        &[
            ("/fine.js", &ok_response("text/javascript", "export {};")),
            ("/blocked.js", &ok_response("text/javascript", "export {};")),
        ],
    )
    .await;

    assert_eq!(
        global(
            &mut page,
            "JSON.stringify([!!globalThis.fine, globalThis.blocked || null])"
        ),
        serde_json::json!("[true,\"refused\"]"),
        "the allowed import loads and the blocked one is refused"
    );
    assert!(requested(&requests, "/fine.js"), "{requests:?}");
    assert!(
        !requested(&requests, "/blocked.js"),
        "a blocked import() is never requested: {requests:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_module_request_carries_the_page_user_agent() {
    let html = "<html><body><script src=\"/classic.js\"></script>\
                <script type=\"module\" src=\"/app.js\"></script></body></html>";
    let app = format!("import './dep.js';\n{}", push("app"));
    let (mut page, requests) = navigate_intercepted_recording(
        html,
        &[("/classic.js", &push("classic")), ("/app.js", &app), ("/dep.js", "")],
    )
    .await;

    assert_eq!(order(&mut page), vec!["classic", "app"]);
    let user_agent = |path: &str| {
        requests
            .iter()
            .find(|request| request.starts_with(&format!("GET {path} ")))
            .unwrap_or_else(|| panic!("{path} must have been requested: {requests:?}"))
            .lines()
            .find_map(|line| {
                line.split_once(':')
                    .filter(|(name, _)| name.eq_ignore_ascii_case("user-agent"))
                    .map(|(_, value)| value.trim().to_string())
            })
    };
    let page_user_agent = user_agent("/classic.js").expect("the page client sends a User-Agent");
    assert_eq!(user_agent("/app.js").as_deref(), Some(page_user_agent.as_str()));
    assert_eq!(user_agent("/dep.js").as_deref(), Some(page_user_agent.as_str()));
}

#[tokio::test(flavor = "current_thread")]
async fn a_module_src_named_twice_runs_once() {
    let html = "<html><body><script type=\"module\" src=\"/app.js\"></script>\
                <script type=\"module\" src=\"/app.js\"></script>\
                <script type=\"module\" src=\"/ok.js\"></script></body></html>";
    let app = push("app");
    let ok = push("ok");
    let mut page = navigate_bounded(
        html,
        &[("/app.js", "text/javascript", &app), ("/ok.js", "text/javascript", &ok)],
    )
    .await;

    assert_eq!(
        order(&mut page),
        vec!["app", "ok"],
        "a module runs once per page, as in a browser"
    );
}

/// Accepts connections and never answers them, so a fetch from it stalls until its caller gives up.
async fn stalling_origin() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((socket, _)) = listener.accept().await {
            held.push(socket);
        }
    });
    format!("http://{addr}")
}

/// Navigates to `html` with a bound on the whole render, so a stalled module fails the test rather than hanging it.
async fn navigate_bounded(html: &str, extra: &[(&str, &str, &str)]) -> Page {
    let mut entries = vec![("/", "text/html", html)];
    entries.extend_from_slice(extra);
    let base = serve(routes(&entries)).await;
    let mut page = test_page();
    tokio::time::timeout(std::time::Duration::from_secs(40), page.navigate(&base))
        .await
        .expect("the render must finish although a module server never answers")
        .expect("navigation must succeed");
    page
}

#[tokio::test(flavor = "current_thread")]
async fn a_module_src_whose_server_never_answers_does_not_hold_the_page() {
    let stall = stalling_origin().await;
    let html = format!(
        "<html><body><script type=\"module\" src=\"{stall}/app.js\"></script>\
         <script type=\"module\" src=\"/ok.js\"></script></body></html>"
    );
    let ok = push("ok");
    let mut page = navigate_bounded(&html, &[("/ok.js", "text/javascript", &ok)]).await;

    assert_eq!(order(&mut page), vec!["ok"]);
}

#[tokio::test(flavor = "current_thread")]
async fn an_inline_module_whose_import_never_answers_does_not_hold_the_page() {
    let stall = stalling_origin().await;
    let html = format!(
        "<html><body><script type=\"module\">import '{stall}/dep.js';\n{}</script>\
         <script type=\"module\" src=\"/ok.js\"></script></body></html>",
        push("stalled"),
    );
    let ok = push("ok");
    let mut page = navigate_bounded(&html, &[("/ok.js", "text/javascript", &ok)]).await;

    assert_eq!(order(&mut page), vec!["ok"]);
}

#[tokio::test(flavor = "current_thread")]
async fn a_module_src_whose_top_level_await_never_settles_does_not_hold_the_page() {
    let stall = stalling_origin().await;
    let html = "<html><body><script type=\"module\" src=\"/tla.js\"></script>\
                <script type=\"module\" src=\"/ok.js\"></script></body></html>";
    let tla = format!(
        "{}\nawait fetch('{stall}/never');\n{}",
        push("tla-before"),
        push("tla-after")
    );
    let ok = push("ok");
    let mut page = navigate_bounded(
        html,
        &[("/tla.js", "text/javascript", &tla), ("/ok.js", "text/javascript", &ok)],
    )
    .await;

    assert_eq!(order(&mut page), vec!["tla-before", "ok"]);
}

#[tokio::test(flavor = "current_thread")]
async fn an_inline_module_whose_top_level_await_never_settles_does_not_hold_the_page() {
    let stall = stalling_origin().await;
    let html = format!(
        "<html><body><script type=\"module\">{}\nawait fetch('{stall}/never');\n{}</script>\
         <script type=\"module\" src=\"/ok.js\"></script></body></html>",
        push("tla-before"),
        push("tla-after"),
    );
    let ok = push("ok");
    let mut page = navigate_bounded(&html, &[("/ok.js", "text/javascript", &ok)]).await;

    assert_eq!(order(&mut page), vec!["tla-before", "ok"]);
}

#[tokio::test(flavor = "current_thread")]
async fn an_inline_module_still_runs_the_modules_it_imports() {
    let html = format!(
        "<html><body><script type=\"module\">import {{ tag }} from './dep.js';\n{}\nglobalThis.imported = tag;</script></body></html>",
        push("inline"),
    );
    let dep = format!("{}\nexport const tag = 'from-dep';", push("dep"));
    let base = serve(routes(&[
        ("/", "text/html", &html),
        ("/dep.js", "text/javascript", &dep),
    ]))
    .await;

    let mut page = test_page();
    page.navigate(&base).await.expect("navigation must succeed");

    assert_eq!(order(&mut page), vec!["dep", "inline"]);
    assert_eq!(global(&mut page, "globalThis.imported"), serde_json::json!("from-dep"));
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

/// Navigates to a page built from its own origin, served beside `extra` routes, with interception
/// blocking every address that ends in `blocked.js`.
async fn navigate_intercepted(page_html: impl Fn(&str) -> String, extra: &[(&str, &str, &str)]) -> (String, Page) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let origin = format!("http://{}", listener.local_addr().expect("addr"));
    let html = page_html(&origin);
    let mut entries = vec![("/", "text/html", html.as_str())];
    entries.extend_from_slice(extra);
    let base = serve_on(listener, routes(&entries));

    let mut page = test_page();
    page.intercept_enabled = true;
    page.intercept_block_patterns = vec!["*blocked.js".to_string()];
    page.navigate(&base).await.expect("navigation must succeed");
    (origin, page)
}

fn event_urls(page: &Page, resource_type: &str) -> Vec<String> {
    page.network_events
        .iter()
        .filter(|event| event.resource_type == resource_type)
        .map(|event| event.url.clone())
        .collect()
}

#[tokio::test(flavor = "current_thread")]
async fn an_absolute_script_src_with_trailing_spaces_is_still_caught_by_interception() {
    let blocked = push("blocked");
    let ok = push("ok");
    let (origin, mut page) = navigate_intercepted(
        |origin| {
            format!(
                "<html><body><script src=\"{origin}/blocked.js  \"></script>\
                 <script src=\"{origin}/ok.js\"></script></body></html>"
            )
        },
        &[
            ("/blocked.js", "application/javascript", &blocked),
            ("/ok.js", "application/javascript", &ok),
        ],
    )
    .await;

    assert_eq!(
        order(&mut page),
        vec!["ok"],
        "the interception pattern must see the parsed address, not the raw attribute with trailing spaces"
    );
    assert_eq!(event_urls(&page, "Script"), vec![format!("{origin}/ok.js")]);
}

#[tokio::test(flavor = "current_thread")]
async fn an_absolute_script_src_with_an_inner_tab_is_still_caught_by_interception() {
    let blocked = push("blocked");
    let ok = push("ok");
    let (_, mut page) = navigate_intercepted(
        |origin| {
            format!(
                "<html><body><script src=\"{origin}/bl&#9;ocked.js\"></script>\
                 <script src=\"{origin}/ok.js\"></script></body></html>"
            )
        },
        &[
            ("/blocked.js", "application/javascript", &blocked),
            ("/ok.js", "application/javascript", &ok),
        ],
    )
    .await;

    assert_eq!(
        order(&mut page),
        vec!["ok"],
        "the URL parser removes an inner tab, so the parsed address ends in blocked.js and is blocked"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn an_absolute_stylesheet_href_is_recorded_under_its_parsed_address() {
    let (origin, page) = navigate_intercepted(
        |origin| {
            format!(
                "<html><head><link rel=\"stylesheet\" href=\"{origin}/o&#10;ne.css \"></head>\
                 <body></body></html>"
            )
        },
        &[("/one.css", "text/css", "a{color:red}")],
    )
    .await;

    assert_eq!(
        event_urls(&page, "Stylesheet"),
        vec![format!("{origin}/one.css")],
        "the network event must carry the parsed address, without the newline or the trailing space"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn an_absolute_module_src_is_recorded_under_its_parsed_address() {
    let (origin, page) = navigate_intercepted(
        |origin| format!("<html><body><script type=\"module\" src=\"{origin}/mod.js \"></script></body></html>"),
        &[("/mod.js", "application/javascript", "export {};")],
    )
    .await;

    assert_eq!(
        event_urls(&page, "Script"),
        vec![format!("{origin}/mod.js")],
        "the module's network event must carry the parsed address, without the trailing space"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_subresource_address_with_userinfo_is_skipped_when_it_resolves() {
    let ok = push("ok");
    let (origin, mut page) = navigate_intercepted(
        |origin| {
            let with_userinfo = origin.replacen("http://", "http://user:s3cret@", 1);
            format!(
                "<html><head><link rel=\"stylesheet\" href=\"{with_userinfo}/x.css\"></head><body>\
                 <script src=\"{with_userinfo}/blocked.js\"></script>\
                 <script src=\"{origin}/ok.js\"></script></body></html>"
            )
        },
        &[("/ok.js", "application/javascript", &ok), ("/x.css", "text/css", "p{}")],
    )
    .await;

    assert_eq!(order(&mut page), vec!["ok"]);
    assert_eq!(event_urls(&page, "Script"), vec![format!("{origin}/ok.js")]);
    assert!(event_urls(&page, "Stylesheet").is_empty());
    let with_userinfo = origin.replacen("http://", "http://user:s3cret@", 1);
    assert_eq!(page.resolve_subresource_url(&format!("{with_userinfo}/a.js")), None);
    assert_eq!(
        page.resolve_subresource_url("/a.js"),
        Some(format!("{origin}/a.js")),
        "a reference without userinfo still resolves"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_subresource_address_that_does_not_parse_is_skipped() {
    let ok = push("ok");
    let (origin, mut page) = navigate_intercepted(
        |origin| {
            format!(
                "<html><head><link rel=\"stylesheet\" href=\"http://[::1/x.css\"></head><body>\
                 <script src=\"http://[::1/x.js\"></script>\
                 <script type=\"module\" src=\"http://[::1/m.js\"></script>\
                 <script src=\"{origin}/ok.js\"></script></body></html>"
            )
        },
        &[("/ok.js", "application/javascript", &ok)],
    )
    .await;

    assert_eq!(order(&mut page), vec!["ok"]);
    assert_eq!(event_urls(&page, "Script"), vec![format!("{origin}/ok.js")]);
    assert!(event_urls(&page, "Stylesheet").is_empty());
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

    let context = BrowserContext::with_ssrf("test".to_string(), None, false, None, Arc::new(AllowAll), false)
        .expect("no proxy, so the context must build");
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
async fn a_navigation_to_a_url_with_userinfo_is_refused_before_robots_txt_is_read() {
    const URL_PASSWORD: &str = "s3cret";
    let base = serve(routes(&[
        ("/", "text/html", "<html><body>ok</body></html>"),
        ("/robots.txt", "text/plain", "User-agent: *\nDisallow: /"),
    ]))
    .await;
    let credentialed = base.replacen("http://", &format!("http://user:{URL_PASSWORD}@"), 1);

    let context = BrowserContext::with_ssrf("test".to_string(), None, false, None, Arc::new(AllowAll), false)
        .expect("no proxy, so the context must build");
    let context = BrowserContext {
        obey_robots: true,
        ..context
    };
    let mut page = Page::new("page-1".to_string(), Arc::new(context));

    let error = page
        .navigate(&credentialed)
        .await
        .expect_err("a URL with userinfo must be refused");
    let PageError::NetworkError(message) = &error else {
        panic!("expected a network error, got {error:?}");
    };
    assert!(
        !message.contains(URL_PASSWORD),
        "the password must not be named, got '{message}'"
    );
    // ~keep Positive twin: the refusal, not the robots.txt block, stopped the navigation, and
    // ~keep it names the URL without its userinfo.
    assert!(
        message.contains("credentials") && !message.contains("robots.txt"),
        "the userinfo refusal must come first, got '{message}'"
    );
    assert!(page.url.is_none(), "a refused URL is never recorded as the page's own");
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

    let context = BrowserContext::with_ssrf("test".to_string(), None, false, None, Arc::new(AllowAll), false)
        .expect("no proxy, so the context must build");
    let context = BrowserContext {
        obey_robots: true,
        ..context
    };
    let mut page = Page::new("page-1".to_string(), Arc::new(context));

    page.navigate(&base).await.expect("navigation must be permitted");
    assert_eq!(page.title, "allowed");
}

/// A robots.txt that opens with a UTF-8 byte-order mark still blocks what it disallows
/// (crawlberg#540): the mark stayed on the first line, so its group was dropped.
#[tokio::test(flavor = "current_thread")]
async fn robots_txt_with_a_leading_byte_order_mark_still_blocks_the_navigation() {
    let base = serve(routes(&[
        (
            "/",
            "text/html",
            "<html><head><title>open</title></head><body></body></html>",
        ),
        (
            "/private",
            "text/html",
            "<html><head><title>secret</title></head><body></body></html>",
        ),
        (
            "/robots.txt",
            "text/plain",
            "\u{feff}User-agent: *\r\nDisallow: /private\r\n",
        ),
    ]))
    .await;

    let context = BrowserContext::with_ssrf("test".to_string(), None, false, None, Arc::new(AllowAll), false)
        .expect("no proxy, so the context must build");
    let context = BrowserContext {
        obey_robots: true,
        ..context
    };
    let mut page = Page::new("page-1".to_string(), Arc::new(context));

    // ~keep Positive twin: the file is read and a path it does not name stays open.
    page.navigate(&base).await.expect("/ is not disallowed");
    assert_eq!(page.title, "open");

    let private = format!("{}/private", base.trim_end_matches('/'));
    let error = page
        .navigate(&private)
        .await
        .expect_err("the leading byte-order mark must not hide the Disallow rule");
    assert!(
        matches!(error, PageError::NetworkError(ref message) if message.contains("Blocked by robots.txt")),
        "expected a robots.txt block, got {error:?}"
    );
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
    let script = fetch_script("/api/setcookie", "");
    let html = format!("<html><body><script>{script}</script></body></html>");
    let with_cookie = "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nSet-Cookie: jsfetch=1\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
    let base = serve_raw(raw(&[
        ("/", &ok_response("text/html", &html)),
        ("/api/setcookie", with_cookie),
    ]))
    .await;
    let mut page = test_page();
    page.navigate(&base).await.expect("navigate");

    // The cookie takes the default path of the fetched URL, so it is not returned for the page path.
    let stored = page.context.cookie_jar.snapshot();
    assert_eq!(
        stored,
        vec![(
            "jsfetch".to_string(),
            "1".to_string(),
            "127.0.0.1".to_string(),
            "/api".to_string(),
            false,
            false,
            true
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

#[cfg(feature = "stealth")]
#[tokio::test(flavor = "current_thread")]
async fn a_stealth_page_fetches_through_the_context_proxy() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let proxy = format!("http://{}", listener.local_addr().expect("addr"));
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut buf = [0u8; 4096];
        let read = socket.read(&mut buf).await.unwrap_or(0);
        log.lock()
            .expect("lock")
            .push(String::from_utf8_lossy(&buf[..read]).to_string());
        let _ = socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\nvia-proxy")
            .await;
    });

    let context = BrowserContext::with_ssrf(
        "test".to_string(),
        Some(crate::net::proxy::test_proxy(&proxy).expect("an http proxy")),
        true,
        None,
        Arc::new(AllowAll),
        false,
    )
    .expect("an http proxy must build the context");
    let context = Arc::new(context);
    let page = Page::new("page-1".to_string(), context.clone());
    let (Some(from_page), Some(from_context)) = (&page.stealth_client, &context.stealth_client) else {
        panic!("a stealth context must give its pages the stealth client");
    };
    assert!(
        Arc::ptr_eq(from_page, from_context),
        "the page must use the context's stealth client"
    );

    let response = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        page.do_fetch(&"http://origin.test/page".parse::<Url>().expect("valid URL"), None),
    )
    .await
    .expect("the fetch must finish")
    .expect("the proxy answers, so the fetch must succeed");

    assert_eq!(response.body, b"via-proxy");
    let seen = seen.lock().expect("lock");
    assert!(
        seen.first()
            .is_some_and(|r| r.starts_with("GET http://origin.test/page ")),
        "the stealth client must send through the context proxy, got {seen:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_fetch_to_a_url_with_userinfo_is_refused_without_it() {
    let script = fetch_script("http://user:s3cret@127.0.0.1:9/data.json", "");
    let html = format!("<html><body><script>{script}</script></body></html>");
    let base = serve(routes(&[("/", "text/html", &html)])).await;
    let mut page = test_page();
    page.navigate(&base).await.expect("navigate");

    let result = fetch_result(&mut page, &base).await;
    let error = result["error"].as_str().unwrap_or_default().to_owned();
    assert!(
        error.contains("credentials") && error.contains("http://127.0.0.1:9/data.json"),
        "the fetch must be refused and name the URL without its userinfo, got {result}"
    );
    assert!(
        !error.contains("s3cret"),
        "the password must not be named, got {result}"
    );
}

/// Serves `responses` on `listener`, recording each raw request head.
fn serve_raw_recording(
    listener: TcpListener,
    responses: StdHashMap<String, String>,
) -> Arc<std::sync::Mutex<Vec<String>>> {
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = requests.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let responses = responses.clone();
            let log = log.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 8192];
                let read = socket.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..read]).to_string();
                let path = request.split_whitespace().nth(1).unwrap_or("/").to_string();
                log.lock().expect("lock").push(request);
                let fallback = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string();
                let response = responses.get(&path).unwrap_or(&fallback);
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.flush().await;
            });
        }
    });
    requests
}

#[tokio::test(flavor = "current_thread")]
async fn a_fetch_redirect_whose_location_has_userinfo_is_followed_without_it() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let script = fetch_script("/start", "");
    let html = format!("<html><body><script>{script}</script></body></html>");
    let redirect = format!(
        "HTTP/1.1 302 Found\r\nLocation: http://user:s3cret@{addr}/end\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    let requests = serve_raw_recording(
        listener,
        raw(&[
            ("/", &ok_response("text/html", &html)),
            ("/start", &redirect),
            ("/end", &ok_response("text/plain", "arrived")),
        ]),
    );
    let base = format!("http://{addr}");
    let mut page = test_page();
    page.navigate(&base).await.expect("navigate");

    let result = fetch_result(&mut page, &base).await;
    assert_eq!(result["text"], "arrived", "the redirect must be followed, got {result}");
    let requests = requests.lock().expect("lock");
    let end = requests
        .iter()
        .find(|request| request.starts_with("GET /end "))
        .expect("the redirect target must have been requested");
    assert!(
        !end.to_lowercase().contains("authorization:"),
        "the Location's userinfo must not become credentials: {end}"
    );
}

#[cfg(feature = "stealth")]
#[tokio::test(flavor = "current_thread")]
async fn a_stealth_page_scopes_the_context_credential_like_the_plain_client() {
    let context = BrowserContext::with_ssrf("test".to_string(), None, true, None, Arc::new(AllowAll), false)
        .expect("no proxy, so the context must build");
    let credential = crate::net::OriginHeaders {
        host: "example.com".to_owned(),
        headers: vec![("Authorization".to_owned(), "Basic dXNlcjpwdw==".to_owned())],
    };
    context.http_client.set_origin_headers(Some(credential.clone())).await;

    let page = Page::new("page-1".to_string(), Arc::new(context));

    let stealth = page
        .stealth_client
        .as_ref()
        .expect("a stealth context gives the page a stealth client");
    assert_eq!(stealth.origin_headers.read().await.as_ref(), Some(&credential));
}

#[tokio::test(flavor = "current_thread")]
async fn script_fetches_and_module_imports_carry_the_credential_only_on_its_host() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let other = format!("http://localhost:{}", addr.port());
    let html = format!(
        "<html><body><script>globalThis.fetched = 0;\
         fetch('/data', {{headers: {{'X-Api-Key': 'page-value'}}}}).finally(() => globalThis.fetched++);\
         fetch('{other}/elsewhere').finally(() => globalThis.fetched++);</script>\
         <script type=\"module\">import '/near.js'; import '/hop.js';</script></body></html>"
    );
    // ~keep A cross-host module redirect: reqwest keeps a custom-named header across hosts,
    // ~keep so only the loader's own per-hop check keeps it off the other host. The Location's
    // ~keep userinfo must not become a Basic header either.
    let redirect = format!(
        "HTTP/1.1 302 Found\r\nLocation: http://user:s3cret@localhost:{}/far.js\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        addr.port()
    );
    let requests = serve_raw_recording(
        listener,
        raw(&[
            ("/", &ok_response("text/html", &html)),
            ("/data", &ok_response("application/json", "{}")),
            ("/elsewhere", &ok_response("application/json", "{}")),
            ("/near.js", &ok_response("text/javascript", "globalThis.near = true;")),
            ("/hop.js", &redirect),
            ("/far.js", &ok_response("text/javascript", "globalThis.far = true;")),
        ]),
    );
    let context = BrowserContext::with_ssrf("test".to_string(), None, false, None, Arc::new(AllowAll), false)
        .expect("no proxy, so the context must build");
    context
        .http_client
        .set_origin_headers(Some(crate::net::OriginHeaders {
            host: "127.0.0.1".to_owned(),
            headers: vec![("X-Api-Key".to_owned(), "k3y".to_owned())],
        }))
        .await;
    let mut page = Page::new("page-1".to_string(), Arc::new(context));

    page.navigate(&format!("http://{addr}/")).await.expect("navigate");

    assert_eq!(
        global(
            &mut page,
            "JSON.stringify([globalThis.fetched, !!globalThis.near, !!globalThis.far])"
        ),
        serde_json::json!("[2,true,true]"),
        "both fetches must settle and both modules must run"
    );
    let requests = requests.lock().expect("lock");
    let request_for = |path: &str| {
        requests
            .iter()
            .find(|request| request.starts_with(&format!("GET {path} ")))
            .unwrap_or_else(|| panic!("{path} must have been requested: {requests:?}"))
            .to_lowercase()
    };
    for path in ["/data", "/near.js", "/hop.js"] {
        assert_eq!(
            request_for(path).matches("x-api-key:").collect::<Vec<_>>().len(),
            1,
            "{path} carries the header once: {}",
            request_for(path)
        );
        assert!(
            request_for(path).contains("x-api-key: k3y"),
            "{path} is on the credential's host and must carry it: {}",
            request_for(path)
        );
    }
    for path in ["/elsewhere", "/far.js"] {
        assert!(
            !request_for(path).contains("x-api-key") && !request_for(path).contains("authorization:"),
            "{path} is on another host and must carry no credential: {}",
            request_for(path)
        );
    }
}

/// Refuses every URL on one host.
#[derive(Debug)]
struct RefuseHost(&'static str);

#[async_trait::async_trait]
impl SsrfValidator for RefuseHost {
    async fn validate(&self, url: &Url) -> Result<(), String> {
        if url.host_str() == Some(self.0) {
            return Err(format!("{} is refused", self.0));
        }
        Ok(())
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_module_redirect_is_checked_against_the_ssrf_policy() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let html = "<html><body><script type=\"module\">import '/near.js'; import '/hop.js';</script></body></html>";
    let redirect = format!(
        "HTTP/1.1 302 Found\r\nLocation: http://localhost:{}/far.js\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        addr.port()
    );
    let requests = serve_raw_recording(
        listener,
        raw(&[
            ("/", &ok_response("text/html", html)),
            ("/near.js", &ok_response("text/javascript", "globalThis.near = true;")),
            ("/hop.js", &redirect),
            ("/far.js", &ok_response("text/javascript", "globalThis.far = true;")),
        ]),
    );
    let context = BrowserContext::with_ssrf(
        "test".to_string(),
        None,
        false,
        None,
        Arc::new(RefuseHost("localhost")),
        false,
    )
    .expect("no proxy, so the context must build");
    let mut page = Page::new("page-1".to_string(), Arc::new(context));

    page.navigate(&format!("http://{addr}/")).await.expect("navigate");

    let requests = requests.lock().expect("lock");
    assert!(
        requests.iter().any(|request| request.starts_with("GET /hop.js ")),
        "the redirecting module must have been requested: {requests:?}"
    );
    assert!(
        !requests.iter().any(|request| request.starts_with("GET /far.js ")),
        "a module redirect to a refused host must never reach the network: {requests:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_module_redirect_loop_stops_after_ten_redirects() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let html = "<html><body><script type=\"module\">import '/loop.js';</script></body></html>";
    let redirect = "HTTP/1.1 302 Found\r\nLocation: /loop.js\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    let requests = serve_raw_recording(
        listener,
        raw(&[("/", &ok_response("text/html", html)), ("/loop.js", redirect)]),
    );
    let mut page = test_page();

    page.navigate(&format!("http://{addr}/")).await.expect("navigate");

    let loops = requests
        .lock()
        .expect("lock")
        .iter()
        .filter(|request| request.starts_with("GET /loop.js "))
        .count();
    assert_eq!(loops, 11, "the first request and ten redirects, then the load stops");
}

/// A page on 127.0.0.1 whose context scopes `X-Api-Key: k3y` to that host, served by `listener`.
async fn credentialed_page(
    listener: TcpListener,
    html: &str,
    extra: &[(&str, &str)],
) -> (Page, Arc<std::sync::Mutex<Vec<String>>>) {
    let addr = listener.local_addr().expect("addr");
    let mut entries = vec![("/", ok_response("text/html", html))];
    entries.extend(extra.iter().map(|(path, response)| (*path, (*response).to_string())));
    let responses = entries
        .iter()
        .map(|(path, response)| ((*path).to_string(), response.clone()))
        .collect();
    let requests = serve_raw_recording(listener, responses);
    let context = BrowserContext::with_ssrf("test".to_string(), None, false, None, Arc::new(AllowAll), false)
        .expect("no proxy, so the context must build");
    context
        .http_client
        .set_origin_headers(Some(crate::net::OriginHeaders {
            host: "127.0.0.1".to_owned(),
            headers: vec![("X-Api-Key".to_owned(), "k3y".to_owned())],
        }))
        .await;
    let mut page = Page::new("page-1".to_string(), Arc::new(context));
    page.navigate(&format!("http://{addr}/")).await.expect("navigate");
    (page, requests)
}

#[tokio::test(flavor = "current_thread")]
async fn a_module_src_carries_the_credential_only_on_its_host() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let other = format!("http://localhost:{}", listener.local_addr().expect("addr").port());
    let html = format!(
        "<html><body><script type=\"module\" src=\"/near.js\"></script>\
         <script type=\"module\" src=\"{other}/far.js\"></script></body></html>"
    );
    let (mut page, requests) = credentialed_page(
        listener,
        &html,
        &[
            ("/near.js", &ok_response("text/javascript", "globalThis.near = true;")),
            ("/far.js", &ok_response("text/javascript", "globalThis.far = true;")),
        ],
    )
    .await;

    assert_eq!(
        global(&mut page, "JSON.stringify([!!globalThis.near, !!globalThis.far])"),
        serde_json::json!("[true,true]"),
        "both module scripts must run"
    );
    let requests = requests.lock().expect("lock");
    let request_for = |path: &str| {
        requests
            .iter()
            .find(|request| request.starts_with(&format!("GET {path} ")))
            .unwrap_or_else(|| panic!("{path} must have been requested: {requests:?}"))
            .to_lowercase()
    };
    assert!(
        request_for("/near.js").contains("x-api-key: k3y"),
        "a module on the credential's host carries it: {}",
        request_for("/near.js")
    );
    assert!(
        !request_for("/far.js").contains("x-api-key"),
        "a module on another host carries no credential: {}",
        request_for("/far.js")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_module_that_fails_to_load_names_no_credential_in_its_error() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (mut page, requests) = credentialed_page(listener, "<html><body><p>page</p></body></html>", &[]).await;
    let js = page.js.as_mut().expect("the page has a JS realm");

    let missing = js
        .load_module(&format!("http://{addr}/missing.js"))
        .await
        .expect_err("a 404 module must fail to load");
    assert!(missing.contains("404"), "{missing}");
    assert!(
        !missing.contains("k3y"),
        "the scoped credential must not reach the error: {missing}"
    );

    let with_userinfo = js
        .load_module(&format!("http://user:s3cret@{addr}/secret.js"))
        .await
        .expect_err("a module address with userinfo must be refused");
    assert!(!with_userinfo.contains("s3cret"), "{with_userinfo}");
    assert!(
        with_userinfo.contains(&format!("http://{addr}/secret.js")),
        "{with_userinfo}"
    );

    let requests = requests.lock().expect("lock");
    assert!(
        requests.iter().any(|request| request.starts_with("GET /missing.js ")),
        "the 404 module was requested: {requests:?}"
    );
    assert!(
        !requests.iter().any(|request| request.starts_with("GET /secret.js ")),
        "a module address with userinfo is never requested: {requests:?}"
    );
    assert!(rendered_html(&page).contains("<p>page</p>"));
}

#[tokio::test(flavor = "current_thread")]
async fn script_fetches_and_module_imports_never_reach_a_rebinding_hosts_denied_address() {
    use crate::net::resolver::tests::{RebindingPolicy, denied_server};

    let (port, seen) = denied_server(
        "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nContent-Length: 22\r\nConnection: close\r\n\r\nglobalThis.far = true;",
    )
    .await;
    let target = format!("http://localhost:{port}");
    let html = format!(
        "<html><body><script>globalThis.fetched = 'pending';\
         fetch('{target}/data').then(() => globalThis.fetched = 'ok', e => globalThis.fetched = String(e));</script>\
         <script type=\"module\">import '{target}/far.js';</script><p>page</p></body></html>"
    );
    let base = serve(routes(&[("/", "text/html", &html)])).await;
    let policy = Arc::new(RebindingPolicy::default());
    let context = BrowserContext::with_ssrf("test".to_string(), None, false, None, policy.clone(), false)
        .expect("no proxy, so the context must build");
    let mut page = Page::new("page-1".to_string(), Arc::new(context));

    page.navigate(&base).await.expect("navigate");

    assert!(rendered_html(&page).contains("<p>page</p>"), "the page itself renders");
    let fetched = global(&mut page, "String(globalThis.fetched)");
    assert!(
        fetched
            .as_str()
            .is_some_and(|error| error.contains("denied by the test policy: 127.0.0.1")),
        "the fetch must fail with the policy's reason, got {fetched}"
    );
    assert_eq!(
        global(&mut page, "!!globalThis.far"),
        serde_json::json!(false),
        "the module must not run"
    );
    assert!(
        seen.lock().expect("lock").is_empty(),
        "the denied address must receive no connection: {:?}",
        seen.lock().expect("lock")
    );
    assert_eq!(
        *policy.resolved.lock().expect("lock"),
        vec!["localhost", "localhost"],
        "the fetch and the module import must each connect through the policy's lookup"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_proxied_page_leaves_its_fetches_and_module_imports_to_the_proxy() {
    use crate::net::resolver::tests::RebindingPolicy;

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let html = "<html><body><script>globalThis.fetched = 'pending';\
                fetch('http://example.invalid/data').then(() => globalThis.fetched = 'ok', e => globalThis.fetched = String(e));</script>\
                <script type=\"module\">import 'http://example.invalid/m.js';</script></body></html>";
    let requests = serve_raw_recording(
        listener,
        raw(&[
            ("http://example.invalid/", &ok_response("text/html", html)),
            ("http://example.invalid/data", &ok_response("application/json", "{}")),
            (
                "http://example.invalid/m.js",
                &ok_response("text/javascript", "globalThis.far = true;"),
            ),
        ]),
    );
    let policy = Arc::new(RebindingPolicy::default());
    // ~keep A proxy named by host: a client that asked the policy for it would be refused.
    let context = BrowserContext::with_ssrf(
        "test".to_string(),
        Some(crate::net::proxy::test_proxy(&format!("http://localhost:{port}")).expect("an http proxy")),
        false,
        None,
        policy.clone(),
        false,
    )
    .expect("an http proxy must build the context");
    let mut page = Page::new("page-1".to_string(), Arc::new(context));

    page.navigate("http://example.invalid/").await.expect("navigate");

    assert_eq!(
        global(&mut page, "JSON.stringify([globalThis.fetched, !!globalThis.far])"),
        serde_json::json!("[\"ok\",true]"),
        "the fetch and the module must go through the proxy: {:?}",
        requests.lock().expect("lock")
    );
    assert!(
        policy.resolved.lock().expect("lock").is_empty(),
        "the proxy resolves the target, so no client may ask the policy to"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_navigation_refused_at_connect_time_names_the_policy_reason_once() {
    use crate::net::resolver::tests::{RebindingPolicy, denied_server};

    let (port, seen) = denied_server("HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
    let context = BrowserContext::with_ssrf(
        "test".to_string(),
        None,
        false,
        None,
        Arc::new(RebindingPolicy::default()),
        false,
    )
    .expect("no proxy, so the context must build");
    let mut page = Page::new("page-1".to_string(), Arc::new(context));

    let error = page
        .navigate(&format!("http://localhost:{port}/"))
        .await
        .expect_err("the connection's lookup answers a denied address")
        .to_string();

    assert!(
        error.contains("denied by the test policy: 127.0.0.1"),
        "the refusal must carry the policy's reason: {error}"
    );
    assert_eq!(error.matches("Network error").count(), 1, "{error}");
    assert!(
        seen.lock().expect("lock").is_empty(),
        "the denied address must receive no connection"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_preflight_refused_at_connect_time_names_the_policy_reason() {
    use crate::net::resolver::tests::{RebindingPolicy, denied_server};

    let (port, seen) = denied_server("HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n").await;
    let html = format!(
        "<html><body><script>globalThis.fetched = 'pending';\
         fetch('http://localhost:{port}/data', {{headers: {{'X-Custom': '1'}}}})\
         .then(() => globalThis.fetched = 'ok', e => globalThis.fetched = String(e));</script></body></html>"
    );
    let base = serve(routes(&[("/", "text/html", &html)])).await;
    let context = BrowserContext::with_ssrf(
        "test".to_string(),
        None,
        false,
        None,
        Arc::new(RebindingPolicy::default()),
        false,
    )
    .expect("no proxy, so the context must build");
    let mut page = Page::new("page-1".to_string(), Arc::new(context));

    page.navigate(&base).await.expect("navigate");

    let fetched = global(&mut page, "String(globalThis.fetched)");
    assert!(
        fetched
            .as_str()
            .is_some_and(|error| error.contains("CORS preflight failed")
                && error.contains("denied by the test policy: 127.0.0.1")),
        "the preflight must fail with the policy's reason, got {fetched}"
    );
    assert!(
        seen.lock().expect("lock").is_empty(),
        "the denied address must receive no connection"
    );
}

fn moved_to(location: &str) -> String {
    format!("HTTP/1.1 301 Moved Permanently\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
}

#[tokio::test(flavor = "current_thread")]
async fn a_counted_navigation_ends_on_the_redirect_at_the_limit() {
    let base = serve_raw(raw(&[
        ("/", &moved_to("/a")),
        ("/a", &moved_to("/b")),
        ("/b", &ok_response("text/html", "<p>b</p>")),
    ]))
    .await;
    let mut page = test_page();
    let followed = page
        .navigate_counting(&format!("{base}/"), crate::lifecycle::WaitUntil::Load, 1)
        .await
        .expect("navigate");

    assert_eq!(followed, 1);
    assert_eq!(page.url_string(), format!("{base}/a"));
    let status = page
        .network_events
        .iter()
        .rev()
        .find(|event| event.resource_type == "Document")
        .map(|event| event.status);
    assert_eq!(status, Some(301), "the page ends on the redirect at the limit");

    let mut uncounted = test_page();
    uncounted.navigate(&format!("{base}/")).await.expect("navigate");
    assert_eq!(
        uncounted.url_string(),
        format!("{base}/b"),
        "an uncounted navigation follows the chain"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_counted_navigation_takes_a_script_navigation_only_within_the_limit() {
    let base = serve_raw(raw(&[
        (
            "/",
            &ok_response(
                "text/html",
                "<html><body><script>location.replace('/next')</script></body></html>",
            ),
        ),
        ("/next", &ok_response("text/html", "<p>next</p>")),
    ]))
    .await;
    for (limit, expected_path, expected_followed) in [(0, "/", 0), (1, "/next", 1)] {
        let mut page = test_page();
        let followed = page
            .navigate_counting(&format!("{base}/"), crate::lifecycle::WaitUntil::Load, limit)
            .await
            .expect("navigate");
        assert_eq!(
            (followed, page.url_string()),
            (expected_followed, format!("{base}{expected_path}")),
            "max_redirects={limit}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_counted_navigation_bounds_the_redirects_of_a_form_post() {
    let base = serve_raw(raw(&[
        (
            "/",
            &ok_response(
                "text/html",
                r#"<html><body><form id="f" method="post" action="/post"></form><script>document.getElementById('f').submit()</script></body></html>"#,
            ),
        ),
        ("/post", &moved_to("/a")),
        ("/a", &ok_response("text/html", "<p>a</p>")),
    ]))
    .await;
    for (limit, expected_path, expected_followed) in [(1, "/post", 1), (2, "/a", 2)] {
        let mut page = test_page();
        let followed = page
            .navigate_counting(&format!("{base}/"), crate::lifecycle::WaitUntil::Load, limit)
            .await
            .expect("navigate");
        assert_eq!(
            (followed, page.url_string()),
            (expected_followed, format!("{base}{expected_path}")),
            "max_redirects={limit}: the form post counts one and its redirect one more"
        );
    }
}

#[cfg(feature = "stealth")]
#[tokio::test(flavor = "current_thread")]
async fn a_counted_stealth_navigation_ends_on_the_redirect_at_the_limit() {
    let base = serve_raw(raw(&[
        ("/", &moved_to("/a")),
        ("/a", &moved_to("/b")),
        ("/b", &ok_response("text/html", "<p>b</p>")),
    ]))
    .await;
    let context = BrowserContext::with_ssrf("test".to_string(), None, true, None, Arc::new(AllowAll), false)
        .expect("no proxy, so the context must build");
    let mut page = Page::new("page-1".to_string(), Arc::new(context));
    assert!(
        page.stealth_client.is_some(),
        "a stealth context fetches through the stealth client"
    );
    let followed = page
        .navigate_counting(&format!("{base}/"), crate::lifecycle::WaitUntil::Load, 1)
        .await
        .expect("navigate");

    assert_eq!((followed, page.url_string()), (1, format!("{base}/a")));
}
