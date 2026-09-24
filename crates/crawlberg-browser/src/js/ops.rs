use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use crate::dom::{DomTree, NodeData, NodeId};
use crate::net::ssrf::{DefaultSsrfValidator, SsrfValidator};
use crate::net::{CookieJar, HttpClient};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use deno_core::Extension;
use deno_core::OpState;
use deno_core::op2;
use tokio::sync::Mutex;

pub type InterceptCallback =
    Arc<Mutex<Option<Box<dyn Fn(String, String, String) -> Option<(u16, String, String)> + Send + Sync>>>>;

#[derive(Debug)]
pub enum InterceptResolution {
    Continue {
        url: Option<String>,
        method: Option<String>,
        headers: Option<HashMap<String, String>>,
        body: Option<String>,
    },
    Fulfill {
        status: u16,
        headers: HashMap<String, String>,
        body: String,
    },
    Fail {
        reason: String,
    },
}

pub struct InterceptedRequest {
    pub request_id: String,
    pub url: String,
    pub method: String,
    pub headers: HashMap<String, String>,
    pub resource_type: String,
    pub resolver: tokio::sync::oneshot::Sender<InterceptResolution>,
}

pub struct JsOpState {
    pub dom: Option<DomTree>,
    pub url: String,
    pub title: String,
    pub blocked_urls: Vec<String>,
    pub cookie_jar: Option<Arc<CookieJar>>,
    pub http_client: Option<Arc<HttpClient>>,
    /// SSRF policy applied to page-initiated fetch/XHR. Never optional: the JS bridge
    /// builds its own reqwest client, so a missing policy here would be a silent hole.
    pub ssrf: Arc<dyn SsrfValidator>,
    pub pending_navigation: Option<(String, String, String)>,
    pub intercept_tx: Option<tokio::sync::mpsc::UnboundedSender<InterceptedRequest>>,
    pub intercept_counter: u64,
    pub intercept_enabled: bool,
}

impl JsOpState {
    pub fn new() -> Self {
        JsOpState {
            dom: None,
            url: "about:blank".to_string(),
            title: String::new(),
            blocked_urls: Vec::new(),
            cookie_jar: None,
            http_client: None,
            ssrf: Arc::new(DefaultSsrfValidator::from_env()),
            pending_navigation: None,
            intercept_tx: None,
            intercept_counter: 0,
            intercept_enabled: false,
        }
    }
}

impl Default for JsOpState {
    fn default() -> Self {
        Self::new()
    }
}

pub type SharedState = Rc<RefCell<JsOpState>>;

#[op2]
#[string]
fn op_dom(state: &OpState, #[string] cmd: String, #[string] arg1: String, #[string] arg2: String) -> String {
    let gs = state.borrow::<SharedState>().clone();
    let gs = gs.borrow();
    let dom = match &gs.dom {
        Some(d) => d,
        None => return "null".to_string(),
    };

    // ~keep Each handler returns None for a command it does not own, so the chain reproduces the
    // single flat `match cmd` it replaced: every command string belongs to exactly one handler,
    // and an unknown command falls through all of them to the same "null" default.
    document_command(dom, &gs, &cmd)
        .or_else(|| selector_command(dom, &cmd, &arg1))
        .or_else(|| node_query_command(dom, &cmd, &arg1, &arg2))
        .or_else(|| mutation_command(dom, &cmd, &arg1, &arg2))
        .or_else(|| create_command(dom, &cmd, &arg1))
        .unwrap_or_else(|| "null".to_string())
}

/// Parse a node-id argument, falling back to the document root id on malformed input.
fn node_id_arg(arg: &str) -> NodeId {
    NodeId::new(arg.parse::<u32>().unwrap_or(0))
}

/// `document.*` accessors, which read page state rather than a specific node.
fn document_command(dom: &DomTree, gs: &JsOpState, cmd: &str) -> Option<String> {
    Some(match cmd {
        "document_node_id" => dom.document().index().to_string(),
        "document_title" => serde_json::to_string(&gs.title).unwrap_or("\"\"".into()),
        "document_url" => serde_json::to_string(&gs.url).unwrap_or("\"\"".into()),
        "document_element" => document_element(dom),
        "document_doctype" => document_doctype(dom),
        _ => return None,
    })
}

fn document_element(dom: &DomTree) -> String {
    for cid in dom.children(dom.document()) {
        if let Some(n) = dom.get_node(cid)
            && n.as_element().map(|name| &*name.local == "html").unwrap_or(false)
        {
            return cid.index().to_string();
        }
    }
    "-1".into()
}

fn document_doctype(dom: &DomTree) -> String {
    for cid in dom.children(dom.document()) {
        if let Some(n) = dom.get_node(cid)
            && let crate::dom::NodeData::Doctype {
                name,
                public_id,
                system_id,
            } = &n.data
        {
            return serde_json::json!({
                "name": name,
                "publicId": public_id,
                "systemId": system_id,
                "nodeId": cid.index(),
            })
            .to_string();
        }
    }
    "null".into()
}

/// Selector lookups, which take a CSS selector rather than a node id.
fn selector_command(dom: &DomTree, cmd: &str, arg1: &str) -> Option<String> {
    Some(match cmd {
        "get_element_by_id" => dom
            .get_element_by_id(arg1)
            .map(|id| id.index().to_string())
            .unwrap_or("-1".into()),
        "query_selector" => dom
            .query_selector(arg1)
            .ok()
            .flatten()
            .map(|id| id.index().to_string())
            .unwrap_or("-1".into()),
        "query_selector_all" => {
            let ids: Vec<i32> = dom
                .query_selector_all(arg1)
                .ok()
                .map(|ids| ids.iter().map(|id| id.index() as i32).collect())
                .unwrap_or_default();
            serde_json::to_string(&ids).unwrap_or("[]".into())
        }
        _ => return None,
    })
}

/// Read-only queries against a single node identified by `arg1`.
fn node_query_command(dom: &DomTree, cmd: &str, arg1: &str, arg2: &str) -> Option<String> {
    let node_id = node_id_arg(arg1);
    Some(match cmd {
        "node_type" => node_type(dom, node_id),
        "node_name" => node_name(dom, node_id),
        "text_content" => serde_json::to_string(&dom.text_content(node_id)).unwrap_or("\"\"".into()),
        "parent_node" | "first_child" | "last_child" | "next_sibling" | "prev_sibling" => dom
            .get_node(node_id)
            .and_then(|n| match cmd {
                "parent_node" => n.parent,
                "first_child" => n.first_child,
                "last_child" => n.last_child,
                "next_sibling" => n.next_sibling,
                "prev_sibling" => n.prev_sibling,
                _ => None,
            })
            .map(|id| id.index().to_string())
            .unwrap_or("-1".into()),
        "child_nodes" => {
            let ids: Vec<i32> = dom.children(node_id).iter().map(|id| id.index() as i32).collect();
            serde_json::to_string(&ids).unwrap_or("[]".into())
        }
        "tag_name" => {
            let name = dom
                .get_node(node_id)
                .and_then(|n| n.as_element().map(|name| (*name.local).to_ascii_uppercase()))
                .unwrap_or_default();
            serde_json::to_string(&name).unwrap_or("\"\"".into())
        }
        "get_attribute" => {
            let val = dom
                .get_node(node_id)
                .and_then(|n| n.get_attribute(arg2).map(|s| s.to_string()));
            serde_json::to_string(&val).unwrap_or("null".into())
        }
        "inner_html" => serde_json::to_string(&dom.inner_html(node_id)).unwrap_or("\"\"".into()),
        "outer_html" => serde_json::to_string(&dom.outer_html(node_id)).unwrap_or("\"\"".into()),
        "element_children" => {
            let ids: Vec<i32> = dom
                .children(node_id)
                .iter()
                .filter(|&&id| dom.get_node(id).map(|n| n.is_element()).unwrap_or(false))
                .map(|id| id.index() as i32)
                .collect();
            serde_json::to_string(&ids).unwrap_or("[]".into())
        }
        "has_child_nodes" => dom
            .get_node(node_id)
            .map(|n| n.first_child.is_some())
            .unwrap_or(false)
            .to_string(),
        "contains" => dom.descendants(node_id).contains(&node_id_arg(arg2)).to_string(),
        _ => return None,
    })
}

/// DOM level 1 `nodeType` code, as a decimal string; `"0"` when the node does not exist.
fn node_type(dom: &DomTree, node_id: NodeId) -> String {
    dom.get_node(node_id)
        .map(|n| match &n.data {
            NodeData::Document => "9",
            NodeData::Element { .. } => "1",
            NodeData::Text { .. } => "3",
            NodeData::Comment { .. } => "8",
            NodeData::Doctype { .. } => "10",
            NodeData::ProcessingInstruction { .. } => "7",
        })
        .unwrap_or("0")
        .into()
}

/// DOM `nodeName`, JSON-encoded. Element names are upper-cased, as the HTML DOM requires.
fn node_name(dom: &DomTree, node_id: NodeId) -> String {
    let name: String = dom
        .get_node(node_id)
        .map(|n| match &n.data {
            NodeData::Document => "#document".to_string(),
            NodeData::Element { name, .. } => (*name.local).to_ascii_uppercase(),
            NodeData::Text { .. } => "#text".to_string(),
            NodeData::Comment { .. } => "#comment".to_string(),
            NodeData::Doctype { name, .. } => name.clone(),
            NodeData::ProcessingInstruction { target, .. } => target.clone(),
        })
        .unwrap_or_default();
    serde_json::to_string(&name).unwrap_or("\"\"".into())
}

/// Commands that mutate an existing node. Each answers `"true"`, as the JS bridge expects.
fn mutation_command(dom: &DomTree, cmd: &str, arg1: &str, arg2: &str) -> Option<String> {
    let node_id = node_id_arg(arg1);
    match cmd {
        "set_attribute" => set_attribute(dom, node_id, arg2),
        "append_child" => dom.append_child(node_id, node_id_arg(arg2)),
        "remove_child" => dom.detach(node_id),
        // ~keep Argument order is inverted here: JS passes (newNode, refNode) but
        // `DomTree::insert_before` takes (refNode, newNode).
        "insert_before" => dom.insert_before(node_id_arg(arg2), node_id),
        "remove_attribute" => {
            dom.with_node_mut(node_id, |n| {
                if let NodeData::Element { attrs, .. } = &mut n.data {
                    attrs.retain(|a| &*a.name.local != arg2);
                }
            });
        }
        "set_inner_html" => set_inner_html(dom, node_id, arg2),
        "set_text_content" => {
            dom.with_node_mut(node_id, |n| match &mut n.data {
                NodeData::Text { contents } => *contents = arg2.to_string(),
                NodeData::Comment { contents } => *contents = arg2.to_string(),
                _ => {}
            });
        }
        _ => return None,
    }
    Some("true".into())
}

fn set_attribute(dom: &DomTree, node_id: NodeId, arg2: &str) {
    let Some((name, value)) = arg2.split_once('\0') else {
        return;
    };
    if name == "id" {
        let old_id = dom
            .get_node(node_id)
            .and_then(|n| n.get_attribute("id").map(|s| s.to_string()));
        dom.with_node_mut(node_id, |n| n.set_attribute(name, value.to_string()));
        dom.update_id_index(node_id, old_id.as_deref(), Some(value));
    } else {
        dom.with_node_mut(node_id, |n| n.set_attribute(name, value.to_string()));
    }
}

fn set_inner_html(dom: &DomTree, target: NodeId, html: &str) {
    for child in dom.children(target) {
        dom.detach(child);
    }
    if !html.is_empty() {
        let fragment = crate::dom::parse_fragment(html);
        let import_root = fragment.find_body_or_root();
        dom.import_children_from(target, &fragment, import_root);
    }
}

/// Node constructors, which answer the new node's id.
fn create_command(dom: &DomTree, cmd: &str, arg1: &str) -> Option<String> {
    let data = match cmd {
        "create_document_fragment" => NodeData::Document,
        "create_element" => NodeData::Element {
            name: html5ever::QualName::new(None, html5ever::ns!(html), html5ever::LocalName::from(arg1)),
            attrs: vec![],
            template_contents: None,
            mathml_annotation_xml_integration_point: false,
        },
        "create_text_node" => NodeData::Text {
            contents: arg1.to_string(),
        },
        "create_comment_node" => NodeData::Comment {
            contents: arg1.to_string(),
        },
        _ => return None,
    };
    Some(dom.new_node(data).index().to_string())
}

#[op2(fast)]
fn op_console_msg(state: &OpState, #[string] level: &str, #[string] msg: &str) {
    let _ = state;
    match level {
        "warn" => tracing::warn!(target: "crawlberg::console", "{}", msg),
        "error" => tracing::error!(target: "crawlberg::console", "{}", msg),
        _ => tracing::info!(target: "crawlberg::console", "{}", msg),
    }
}

// ~keep JS fetch/XHR must build with the page proxy each request.
// ~keep A cached client can otherwise bypass a changed proxy setting.
fn build_request_client(proxy_url: Option<&str>) -> Result<reqwest::Client, String> {
    // ~keep Manual redirects keep every hop under SSRF validation; reqwest auto-follow can cross into localhost.
    let mut builder = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none());
    if let Some(proxy) = proxy_url {
        let p = reqwest::Proxy::all(proxy).map_err(|e| format!("Invalid op_fetch_url proxy '{}': {}", proxy, e))?;
        builder = builder.proxy(p);
    }
    builder
        .build()
        .map_err(|e| format!("failed to build reqwest::Client: {}", e))
}

/// Cap on the number of redirect hops op_fetch_url will follow.
/// Matches reqwest's default policy of 10.
const FETCH_REDIRECT_LIMIT: usize = 10;

#[op2(async(lazy), fast)]
#[string]
async fn op_fetch_url(
    state: Rc<RefCell<OpState>>,
    #[string] url: String,
    #[string] method: String,
    #[string] headers_json: String,
    #[string] body: String,
    #[string] origin: String,
    #[string] mode: String,
) -> Result<String, deno_error::JsErrorBox> {
    tracing::debug!("op_fetch_url called: {} {} (intercept check pending)", method, url);

    // ~keep Clone the validator out of the RefCell before awaiting; re-entrant page JS
    // would otherwise hit a BorrowMutError while this op is suspended.
    let ssrf = {
        let state_borrow = state.borrow();
        let gs = state_borrow.borrow::<SharedState>().clone();
        let validator = gs.borrow().ssrf.clone();
        drop(gs);
        validator
    };

    if let Ok(ref parsed_url) = url::Url::parse(&url)
        && let Err(e) = validate_fetch_url(parsed_url, &ssrf).await
    {
        return Ok(blocked_response(&url, Some(e)));
    }

    let Some(context) = read_fetch_context(&state, &url) else {
        return Ok(blocked_response(&url, None));
    };

    if let Some(intercepted) = &context.intercept
        && let Some(early) = resolve_interception(intercepted, &url, &method, &headers_json).await
    {
        return Ok(early);
    }

    let client = build_request_client(context.proxy_url.as_deref()).map_err(deno_error::JsErrorBox::generic)?;
    let cors = CorsContext::new(&url, &origin, &method, &headers_json);

    if cors.needs_preflight(&mode) {
        send_preflight(&client, &url, &method, &cors).await?;
    }

    // ~keep Follow redirects manually so the SSRF policy applies to every hop.
    let response = match send_following_redirects(FetchRequest {
        client: &client,
        url: &url,
        method: cors.request_method.clone(),
        body,
        cors: &cors,
        context: &context,
        ssrf: &ssrf,
    })
    .await?
    {
        RedirectOutcome::Response(response) => response,
        RedirectOutcome::Blocked(payload) => return Ok(payload),
    };

    finish_response(response, &cors, &mode, &url, &method).await
}

/// Apply the response-side CORS check, then read the body into the op's JSON payload.
async fn finish_response(
    response: reqwest::Response,
    cors: &CorsContext,
    mode: &str,
    url: &str,
    method: &str,
) -> Result<String, deno_error::JsErrorBox> {
    let status = response.status().as_u16();
    let resp_headers: HashMap<String, String> = response
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();

    if let Some(payload) = cors.reject_response(mode, &resp_headers, url) {
        return Ok(payload);
    }

    let resp_bytes = response
        .bytes()
        .await
        .map_err(|e| deno_error::JsErrorBox::generic(e.to_string()))?;
    let resp_body = String::from_utf8_lossy(&resp_bytes).to_string();
    let resp_body_base64 = BASE64.encode(&resp_bytes);

    tracing::debug!("op_fetch_url completed: {} {} ({} bytes)", method, url, resp_body.len());

    Ok(serde_json::json!({
        "status": status,
        "body": resp_body,
        "bodyBase64": resp_body_base64,
        "url": url,
        "headers": resp_headers,
    })
    .to_string())
}

/// The payload page JS receives for a request that never reached the network, or whose
/// response must not be exposed. `bootstrap.js` turns a status of 0 into a rejected promise.
fn blocked_response(url: &str, error: Option<String>) -> String {
    let mut payload = serde_json::json!({
        "status": 0,
        "body": "",
        "url": url,
        "headers": {},
        "blocked": true,
    });
    if let Some(error) = error {
        payload["error"] = serde_json::Value::String(error);
    }
    payload.to_string()
}

/// Page state a fetch needs, read out of the `RefCell` in one borrow so none is held
/// across an await point.
struct FetchContext {
    cookie_jar: Option<Arc<CookieJar>>,
    in_flight: Option<Arc<std::sync::atomic::AtomicU32>>,
    intercept: Option<(tokio::sync::mpsc::UnboundedSender<InterceptedRequest>, String)>,
    proxy_url: Option<String>,
}

/// `None` when `url` matches one of the page's blocked-URL patterns.
fn read_fetch_context(state: &Rc<RefCell<OpState>>, url: &str) -> Option<FetchContext> {
    let state_borrow = state.borrow();
    let gs = state_borrow.borrow::<SharedState>().clone();
    let mut gs = gs.borrow_mut();

    for pattern in &gs.blocked_urls {
        if pattern == "*" || url.contains(pattern) || glob_match(pattern, url) {
            return None;
        }
    }

    tracing::debug!(
        "op_fetch_url: intercept_enabled={}, has_tx={}",
        gs.intercept_enabled,
        gs.intercept_tx.is_some()
    );
    let intercept = if gs.intercept_enabled {
        gs.intercept_counter += 1;
        gs.intercept_tx
            .clone()
            .map(|tx| (tx, format!("intercept-{}", gs.intercept_counter)))
    } else {
        None
    };

    Some(FetchContext {
        cookie_jar: gs.cookie_jar.clone(),
        in_flight: gs.http_client.as_ref().map(|c| c.in_flight.clone()),
        intercept,
        proxy_url: gs
            .http_client
            .as_ref()
            .and_then(|c| c.proxy_url().map(|s| s.to_string())),
    })
}

/// Hand the request to the interceptor and wait for its decision.
///
/// `Some(payload)` means the interceptor answered the request itself and the op must return
/// that payload; `None` means carry on with a direct fetch.
async fn resolve_interception(
    intercepted: &(tokio::sync::mpsc::UnboundedSender<InterceptedRequest>, String),
    url: &str,
    method: &str,
    headers_json: &str,
) -> Option<String> {
    let (tx, request_id) = intercepted;
    let custom_headers: HashMap<String, String> = serde_json::from_str(headers_json).unwrap_or_default();
    let (resolve_tx, resolve_rx) = tokio::sync::oneshot::channel();
    let request = InterceptedRequest {
        request_id: request_id.clone(),
        url: url.to_string(),
        method: method.to_string(),
        headers: custom_headers,
        resource_type: "Fetch".to_string(),
        resolver: resolve_tx,
    };
    if tx.send(request).is_err() {
        return None;
    }

    match resolve_rx.await {
        Ok(InterceptResolution::Fulfill { status, headers, body }) => Some(
            serde_json::json!({
                "status": status,
                "body": body,
                "url": url,
                "headers": headers,
            })
            .to_string(),
        ),
        Ok(InterceptResolution::Fail { reason }) => Some(blocked_response(url, Some(reason))),
        Ok(InterceptResolution::Continue { .. }) => {
            tracing::debug!("Interception: continue request {}", url);
            None
        }
        Err(error) => {
            tracing::warn!(
                "Interception: resolver for {} dropped without a decision ({}); \
                 falling back to a direct fetch",
                url,
                error
            );
            None
        }
    }
}

/// Origin comparison and CORS decisions for one fetch.
struct CorsContext {
    page_origin: String,
    is_cross_origin: bool,
    request_method: reqwest::Method,
    custom_headers: HashMap<String, String>,
}

impl CorsContext {
    fn new(url: &str, origin: &str, method: &str, headers_json: &str) -> Self {
        let request_origin = url::Url::parse(url)
            .ok()
            .map(|u| {
                let host = u.host_str().unwrap_or("");
                match u.port() {
                    Some(p) => format!("{}://{}:{}", u.scheme(), host, p),
                    None => format!("{}://{}", u.scheme(), host),
                }
            })
            .unwrap_or_default();
        let page_origin = if origin.is_empty() {
            request_origin.clone()
        } else {
            origin.to_string()
        };
        CorsContext {
            is_cross_origin: !page_origin.is_empty() && request_origin != page_origin,
            page_origin,
            request_method: method.parse().unwrap_or(reqwest::Method::GET),
            custom_headers: serde_json::from_str(headers_json).unwrap_or_default(),
        }
    }

    /// A cross-origin CORS request needs a preflight unless it is a simple request: a
    /// GET/HEAD/POST carrying only CORS-safelisted headers.
    fn needs_preflight(&self, mode: &str) -> bool {
        self.is_cross_origin
            && mode == "cors"
            && (self.request_method != reqwest::Method::GET
                && self.request_method != reqwest::Method::HEAD
                && self.request_method != reqwest::Method::POST
                || self.custom_headers.keys().any(|k| {
                    let kl = k.to_lowercase();
                    kl != "accept" && kl != "accept-language" && kl != "content-language" && kl != "content-type"
                }))
    }

    /// `Some(payload)` when the response's `Access-Control-Allow-Origin` does not admit this page.
    fn reject_response(&self, mode: &str, resp_headers: &HashMap<String, String>, url: &str) -> Option<String> {
        if !self.is_cross_origin || mode != "cors" {
            return None;
        }
        let allowed = resp_headers
            .get("access-control-allow-origin")
            .map(|s| s.as_str())
            .unwrap_or("");
        if allowed == "*" || allowed == self.page_origin {
            return None;
        }
        Some(
            serde_json::json!({
                "status": 0,
                "body": "",
                "url": url,
                "headers": {},
                "corsBlocked": true,
                "corsError": format!("CORS error: Origin '{}' not in Access-Control-Allow-Origin '{}'", self.page_origin, allowed),
            })
            .to_string(),
        )
    }
}

async fn send_preflight(
    client: &reqwest::Client,
    url: &str,
    method: &str,
    cors: &CorsContext,
) -> Result<(), deno_error::JsErrorBox> {
    let preflight = client
        .request(reqwest::Method::OPTIONS, url)
        .header("Origin", &cors.page_origin)
        .header("Access-Control-Request-Method", method)
        .header(
            "Access-Control-Request-Headers",
            cors.custom_headers.keys().cloned().collect::<Vec<_>>().join(", "),
        )
        .send()
        .await
        .map_err(|e| deno_error::JsErrorBox::generic(format!("CORS preflight failed: {}", e)))?;

    let allowed_origin = preflight
        .headers()
        .get("access-control-allow-origin")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if allowed_origin != "*" && allowed_origin != cors.page_origin {
        return Err(deno_error::JsErrorBox::generic(format!(
            "CORS preflight: Origin '{}' not allowed by Access-Control-Allow-Origin '{}'",
            cors.page_origin, allowed_origin
        )));
    }
    Ok(())
}

struct FetchRequest<'a> {
    client: &'a reqwest::Client,
    url: &'a str,
    method: reqwest::Method,
    body: String,
    cors: &'a CorsContext,
    context: &'a FetchContext,
    ssrf: &'a Arc<dyn SsrfValidator>,
}

enum RedirectOutcome {
    Response(reqwest::Response),
    /// A JSON payload the op must return instead of a response.
    Blocked(String),
}

/// Statuses that rewrite a redirected request to a bodyless GET.
const REDIRECT_STATUSES_FORCING_GET: [u16; 3] = [301, 302, 303];

async fn send_following_redirects(request: FetchRequest<'_>) -> Result<RedirectOutcome, deno_error::JsErrorBox> {
    let FetchRequest {
        client,
        url,
        mut method,
        mut body,
        cors,
        context,
        ssrf,
    } = request;
    let mut current_url = url.to_string();
    let mut redirects_followed: usize = 0;

    loop {
        let response = send_one_hop(client, &current_url, &method, &body, cors, context).await?;
        store_response_cookies(context, &current_url, &response);

        if !response.status().is_redirection() {
            return Ok(RedirectOutcome::Response(response));
        }

        let Some(next_url) = redirect_target(&current_url, &response) else {
            return Ok(RedirectOutcome::Response(response));
        };

        // ~keep Re-validate every redirect target against the SSRF policy.
        if let Err(reason) = validate_fetch_url(&next_url, ssrf).await {
            return Ok(RedirectOutcome::Blocked(blocked_response(
                next_url.as_str(),
                Some(format!("Redirect to forbidden URL blocked: {}", reason)),
            )));
        }

        redirects_followed += 1;
        if redirects_followed > FETCH_REDIRECT_LIMIT {
            return Ok(RedirectOutcome::Blocked(blocked_response(
                next_url.as_str(),
                Some(format!("Too many redirects (>{})", FETCH_REDIRECT_LIMIT)),
            )));
        }

        if REDIRECT_STATUSES_FORCING_GET.contains(&response.status().as_u16()) {
            method = reqwest::Method::GET;
            body.clear();
        }
        current_url = next_url.to_string();
    }
}

async fn send_one_hop(
    client: &reqwest::Client,
    current_url: &str,
    method: &reqwest::Method,
    body: &str,
    cors: &CorsContext,
    context: &FetchContext,
) -> Result<reqwest::Response, deno_error::JsErrorBox> {
    let mut req = client.request(method.clone(), current_url);

    if cors.is_cross_origin {
        req = req.header("Origin", &cors.page_origin);
    }

    // ~keep Same-origin only: sending the jar's cookies on a cross-origin fetch would leak
    // them to a third party, which `credentials: "omit"` semantics forbid.
    if !cors.is_cross_origin
        && let Some(ref jar) = context.cookie_jar
        && let Ok(parsed_url) = url::Url::parse(current_url)
    {
        let cookie_header = jar.get_cookie_header(&parsed_url);
        if !cookie_header.is_empty() {
            req = req.header("Cookie", &cookie_header);
        }
    }

    for (k, v) in &cors.custom_headers {
        req = req.header(k.as_str(), v.as_str());
    }

    if !body.is_empty() {
        req = req.body(body.to_string());
    }

    if let Some(ref counter) = context.in_flight {
        counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    let response = req.send().await.map_err(|e| {
        if let Some(ref counter) = context.in_flight {
            counter.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        }
        deno_error::JsErrorBox::generic(e.to_string())
    })?;
    if let Some(ref counter) = context.in_flight {
        counter.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
    Ok(response)
}

fn store_response_cookies(context: &FetchContext, current_url: &str, response: &reqwest::Response) {
    if let Some(ref jar) = context.cookie_jar
        && let Ok(parsed_url) = url::Url::parse(current_url)
    {
        for val in response.headers().get_all(reqwest::header::SET_COOKIE) {
            if let Ok(s) = val.to_str() {
                jar.set_cookie(s, &parsed_url);
            }
        }
    }
}

fn redirect_target(current_url: &str, response: &reqwest::Response) -> Option<url::Url> {
    let location = response
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())?;
    url::Url::parse(current_url).ok()?.join(location).ok()
}

fn glob_match(pattern: &str, url: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if pattern.starts_with('*') && pattern.ends_with('*') {
        return url.contains(&pattern[1..pattern.len() - 1]);
    }
    if let Some(suffix) = pattern.strip_prefix('*') {
        return url.ends_with(suffix);
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return url.starts_with(prefix);
    }
    url == pattern
}

async fn validate_fetch_url(url: &url::Url, ssrf: &Arc<dyn SsrfValidator>) -> Result<(), String> {
    ssrf.validate(url).await
}

#[op2]
#[string]
fn op_get_cookies(state: &OpState) -> String {
    let gs = state.borrow::<SharedState>().clone();
    let gs = gs.borrow();
    let jar = match &gs.cookie_jar {
        Some(j) => j,
        None => return String::new(),
    };
    let url = match url::Url::parse(&gs.url) {
        Ok(u) => u,
        Err(_) => return String::new(),
    };
    jar.get_js_visible_cookies(&url)
}

#[op2(fast)]
fn op_set_cookie(state: &OpState, #[string] cookie_str: &str) {
    let gs = state.borrow::<SharedState>().clone();
    let gs = gs.borrow();
    let jar = match &gs.cookie_jar {
        Some(j) => j,
        None => return,
    };
    let url = match url::Url::parse(&gs.url) {
        Ok(u) => u,
        Err(_) => return,
    };
    jar.set_cookie_from_js(cookie_str, &url);
}

#[op2(fast)]
fn op_navigate(state: &OpState, #[string] url: &str, #[string] method: &str, #[string] body: &str) {
    let gs = state.borrow::<SharedState>().clone();
    let mut gs = gs.borrow_mut();
    gs.url = url.to_string();
    gs.pending_navigation = Some((url.to_string(), method.to_string(), body.to_string()));
}

pub fn build_extension() -> Extension {
    Extension {
        name: "crawlberg_browser_dom",
        ops: std::borrow::Cow::Owned(vec![
            op_dom(),
            op_console_msg(),
            op_fetch_url(),
            op_get_cookies(),
            op_set_cookie(),
            op_navigate(),
        ]),
        ..Default::default()
    }
}
