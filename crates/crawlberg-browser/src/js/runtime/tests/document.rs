use super::*;

fn setup_runtime_with_cookies(html: &str) -> (BrowserJsRuntime, std::sync::Arc<crate::net::CookieJar>) {
    let dom = crate::dom::parse_html(html);
    let jar = std::sync::Arc::new(crate::net::CookieJar::new());
    let rt = BrowserJsRuntime::new();
    rt.set_dom(dom);
    rt.set_url("http://example.com/test");
    rt.set_title("Test Page");
    rt.set_cookie_jar(jar.clone());
    (rt, jar)
}

#[tokio::test(flavor = "current_thread")]
async fn test_document_cookie_reads_http_cookies() {
    let (mut rt, jar) = setup_runtime_with_cookies("<html><body></body></html>");
    let url = url::Url::parse("http://example.com/test").unwrap();
    jar.set_cookie("session=abc123; Path=/", &url);
    jar.set_cookie("theme=dark; Path=/", &url);
    let result = rt.evaluate("document.cookie").unwrap();
    let cookie_str = result.as_str().unwrap();
    assert!(
        cookie_str.contains("session=abc123"),
        "expected session cookie, got: {}",
        cookie_str
    );
    assert!(
        cookie_str.contains("theme=dark"),
        "expected theme cookie, got: {}",
        cookie_str
    );
}

#[tokio::test(flavor = "current_thread")]
async fn test_document_cookie_excludes_httponly() {
    let (mut rt, jar) = setup_runtime_with_cookies("<html><body></body></html>");
    let url = url::Url::parse("http://example.com/test").unwrap();
    jar.set_cookie("visible=yes; Path=/", &url);
    jar.set_cookie("secret=token; Path=/; HttpOnly", &url);
    let result = rt.evaluate("document.cookie").unwrap();
    let cookie_str = result.as_str().unwrap();
    assert!(
        cookie_str.contains("visible=yes"),
        "expected visible cookie, got: {}",
        cookie_str
    );
    assert!(
        !cookie_str.contains("secret"),
        "httpOnly cookie should not be visible to JS, got: {}",
        cookie_str
    );
}

#[tokio::test(flavor = "current_thread")]
async fn test_document_cookie_setter_stores_in_jar() {
    let (mut rt, jar) = setup_runtime_with_cookies("<html><body></body></html>");
    rt.evaluate("document.cookie = 'foo=bar; Path=/'").unwrap();
    let url = url::Url::parse("http://example.com/test").unwrap();
    let result = rt.evaluate("document.cookie").unwrap();
    assert!(result.as_str().unwrap().contains("foo=bar"));
    let header = jar.get_cookie_header(&url);
    assert!(header.contains("foo=bar"), "cookie should be in jar, got: {}", header);
}

#[tokio::test(flavor = "current_thread")]
async fn test_document_cookie_delete_via_max_age() {
    let (mut rt, jar) = setup_runtime_with_cookies("<html><body></body></html>");
    let url = url::Url::parse("http://example.com/test").unwrap();
    rt.evaluate("document.cookie = 'temp=val; Path=/'").unwrap();
    assert!(
        rt.evaluate("document.cookie")
            .unwrap()
            .as_str()
            .unwrap()
            .contains("temp=val")
    );
    rt.evaluate("document.cookie = 'temp=; Max-Age=0'").unwrap();
    let result = rt.evaluate("document.cookie").unwrap();
    assert!(
        !result.as_str().unwrap().contains("temp="),
        "cookie should be deleted, got: {}",
        result
    );
    assert!(!jar.get_cookie_header(&url).contains("temp="));
}

#[tokio::test(flavor = "current_thread")]
async fn test_document_cookie_js_and_http_merge() {
    let (mut rt, jar) = setup_runtime_with_cookies("<html><body></body></html>");
    let url = url::Url::parse("http://example.com/test").unwrap();
    jar.set_cookie("server_sid=xyz; Path=/", &url);
    rt.evaluate("document.cookie = 'client_pref=light'").unwrap();
    let result = rt.evaluate("document.cookie").unwrap();
    let cookie_str = result.as_str().unwrap();
    assert!(
        cookie_str.contains("server_sid=xyz"),
        "expected server cookie, got: {}",
        cookie_str
    );
    assert!(
        cookie_str.contains("client_pref=light"),
        "expected client cookie, got: {}",
        cookie_str
    );
}

#[tokio::test(flavor = "current_thread")]
async fn test_document_cookie_empty_when_no_cookies() {
    let (mut rt, _jar) = setup_runtime_with_cookies("<html><body></body></html>");
    let result = rt.evaluate("document.cookie").unwrap();
    assert_eq!(result.as_str().unwrap(), "");
}

#[tokio::test(flavor = "current_thread")]
async fn test_document_cookie_no_jar_returns_empty() {
    let mut rt = setup_runtime("<html><body></body></html>");
    let result = rt.evaluate("document.cookie").unwrap();
    assert_eq!(result.as_str().unwrap(), "");
}

#[tokio::test(flavor = "current_thread")]
async fn test_document_write_appends_to_body() {
    let mut rt = setup_runtime("<html><body><p>Existing</p></body></html>");
    rt.evaluate("document.write('<div>Added</div>')").unwrap();
    let html = rt.evaluate("document.body.innerHTML").unwrap();
    let body = html.as_str().unwrap();
    assert!(
        body.contains("Existing"),
        "existing content should remain, got: {}",
        body
    );
    assert!(body.contains("Added"), "written content should appear, got: {}", body);
}

#[tokio::test(flavor = "current_thread")]
async fn test_document_writeln() {
    let mut rt = setup_runtime("<html><body></body></html>");
    rt.evaluate("document.writeln('Hello')").unwrap();
    let html = rt.evaluate("document.body.innerHTML").unwrap();
    assert!(html.as_str().unwrap().contains("Hello"));
}

#[tokio::test(flavor = "current_thread")]
async fn test_document_write_multiple_args() {
    let mut rt = setup_runtime("<html><body></body></html>");
    rt.evaluate("document.write('Hello', ' ', 'World')").unwrap();
    let text = rt.evaluate("document.body.textContent").unwrap();
    assert_eq!(text.as_str().unwrap().trim(), "Hello World");
}

#[tokio::test(flavor = "current_thread")]
async fn test_document_open_clears_body() {
    let mut rt = setup_runtime("<html><body><p>Old content</p></body></html>");
    rt.evaluate("document.open()").unwrap();
    let html = rt.evaluate("document.body.innerHTML").unwrap();
    assert_eq!(html.as_str().unwrap(), "");
}

#[tokio::test(flavor = "current_thread")]
async fn test_document_write_html_elements() {
    let mut rt = setup_runtime("<html><body></body></html>");
    rt.evaluate(r#"document.write('<h1 id="title">Test</h1><p>Para</p>')"#)
        .unwrap();
    let h1 = rt.evaluate("document.querySelector('h1').textContent").unwrap();
    assert_eq!(h1.as_str().unwrap(), "Test");
    let p = rt.evaluate("document.querySelector('p').textContent").unwrap();
    assert_eq!(p.as_str().unwrap(), "Para");
}

#[tokio::test(flavor = "current_thread")]
async fn test_url_relative_resolution() {
    let mut rt = setup_runtime("<html><body></body></html>");
    let result = rt
        .evaluate("new URL('data.json', 'http://example.com/path/page.html').href")
        .unwrap();
    assert_eq!(result.as_str().unwrap(), "http://example.com/path/data.json");

    let result = rt
        .evaluate("new URL('/api/data', 'http://example.com/path/page.html').href")
        .unwrap();
    assert_eq!(result.as_str().unwrap(), "http://example.com/api/data");

    let result = rt
        .evaluate("new URL('https://other.com/foo', 'http://example.com/bar').href")
        .unwrap();
    assert_eq!(result.as_str().unwrap(), "https://other.com/foo");

    let result = rt
        .evaluate("new URL('sub/file.js', 'http://example.com/a/b/c.html').href")
        .unwrap();
    assert_eq!(result.as_str().unwrap(), "http://example.com/a/b/sub/file.js");

    let result = rt
        .evaluate("new URL('api.json', 'http://localhost:8080/dir/index.html').href")
        .unwrap();
    assert_eq!(result.as_str().unwrap(), "http://localhost:8080/dir/api.json");
}

#[tokio::test(flavor = "current_thread")]
async fn test_fetch_url_input_decodes_binary_body_base64() {
    let mut rt = setup_runtime("<html><body></body></html>");
    let result = rt
        .call_function_on_for_cdp(
            r#"async () => {
            const originalFetchOp = Deno.core.ops.op_fetch_url;
            try {
                Deno.core.ops.op_fetch_url = (url) => {
                    globalThis.__capturedFetchUrl = url;
                    return JSON.stringify({
                        status: 200,
                        headers: { "content-type": "application/wasm" },
                        bodyBase64: "AGFzbQEAAAA=",
                        url,
                    });
                };
                const response = await fetch(new URL("/pkg/app_bg.wasm", document.URL));
                const bytes = Array.from(new Uint8Array(await response.arrayBuffer()));
                return { url: globalThis.__capturedFetchUrl, bytes };
            } finally {
                Deno.core.ops.op_fetch_url = originalFetchOp;
            }
        }"#,
            None,
            &[],
            true,
            true,
        )
        .await
        .unwrap();

    assert_eq!(
        result.value.unwrap(),
        serde_json::json!({
            "url": "http://example.com/pkg/app_bg.wasm",
            "bytes": [0, 97, 115, 109, 1, 0, 0, 0],
        })
    );
}

#[tokio::test(flavor = "current_thread")]
async fn test_response_array_buffer_preserves_typed_array_view() {
    let mut rt = setup_runtime("<html><body></body></html>");
    let result = rt
        .call_function_on_for_cdp(
            r#"async () => {
            const bytes = new Uint8Array([9, 0, 97, 115, 109, 1, 8]);
            const response = new Response(bytes.subarray(1, 6));
            return Array.from(new Uint8Array(await response.arrayBuffer()));
        }"#,
            None,
            &[],
            true,
            true,
        )
        .await
        .unwrap();

    assert_eq!(result.value.unwrap(), serde_json::json!([0, 97, 115, 109, 1]));
}

#[tokio::test(flavor = "current_thread")]
async fn test_wasm_instantiate_streaming_uses_response_array_buffer() {
    let mut rt = setup_runtime("<html><body></body></html>");
    let result = rt
        .call_function_on_for_cdp(
            r#"async () => {
            const bytes = new Uint8Array([0, 97, 115, 109, 1, 0, 0, 0]);
            const result = await WebAssembly.instantiateStreaming(
                Promise.resolve(new Response(bytes)),
                {},
            );
            return result.instance instanceof WebAssembly.Instance;
        }"#,
            None,
            &[],
            true,
            true,
        )
        .await
        .unwrap();

    assert_eq!(result.value.unwrap(), serde_json::json!(true));
}

#[tokio::test(flavor = "current_thread")]
async fn test_text_decoder_respects_typed_array_view() {
    let mut rt = setup_runtime("<html><body></body></html>");
    let result = rt
        .evaluate("new TextDecoder().decode(new Uint8Array([65, 66, 67]).subarray(1, 2))")
        .unwrap();
    assert_eq!(result.as_str().unwrap(), "B");
}

#[tokio::test(flavor = "current_thread")]
async fn test_document_doctype() {
    let mut rt = setup_runtime("<!DOCTYPE html><html><body></body></html>");
    let result = rt.evaluate("document.doctype !== null").unwrap();
    assert_eq!(result, serde_json::json!(true));

    let name = rt.evaluate("document.doctype.name").unwrap();
    assert_eq!(name, serde_json::json!("html"));

    let node_type = rt.evaluate("document.doctype.nodeType").unwrap();
    assert_eq!(node_type.as_f64().unwrap() as i64, 10);
}

#[tokio::test(flavor = "current_thread")]
async fn test_document_doctype_null_when_missing() {
    let mut rt = setup_runtime("<html><body></body></html>");
    let result = rt.evaluate("document.doctype === null").unwrap();
    assert_eq!(result, serde_json::json!(true));
}

#[tokio::test(flavor = "current_thread")]
async fn test_xml_serializer_doctype() {
    let mut rt = setup_runtime("<!DOCTYPE html><html><body></body></html>");
    let result = rt
        .evaluate("new XMLSerializer().serializeToString(document.doctype)")
        .unwrap();
    assert_eq!(result.as_str().unwrap(), "<!DOCTYPE html>");
}

#[tokio::test(flavor = "current_thread")]
async fn test_xml_serializer_element() {
    let mut rt = setup_runtime(r#"<html><body><div id="x">Hello</div></body></html>"#);
    let result = rt
        .evaluate("new XMLSerializer().serializeToString(document.getElementById('x'))")
        .unwrap();
    let html = result.as_str().unwrap();
    assert!(html.contains("<div"));
    assert!(html.contains("Hello"));
}

#[tokio::test(flavor = "current_thread")]
async fn test_create_event_custom_event_has_init_method() {
    let mut rt = setup_runtime("<html><body></body></html>");
    let kind = rt
        .evaluate("typeof document.createEvent('CustomEvent').initCustomEvent")
        .unwrap();
    assert_eq!(kind, serde_json::json!("function"));
}

#[tokio::test(flavor = "current_thread")]
async fn test_init_custom_event_sets_fields() {
    let mut rt = setup_runtime("<html><body></body></html>");
    rt.execute_script(
        "test",
        r#"
        globalThis.__e = document.createEvent('CustomEvent');
        globalThis.__e.initCustomEvent('myevent', true, false, {hello: 'world'});
    "#,
    )
    .unwrap();
    let t = rt.evaluate("globalThis.__e.type").unwrap();
    assert_eq!(t, serde_json::json!("myevent"));
    let b = rt.evaluate("globalThis.__e.bubbles").unwrap();
    assert_eq!(b, serde_json::json!(true));
    let c = rt.evaluate("globalThis.__e.cancelable").unwrap();
    assert_eq!(c, serde_json::json!(false));
    let d = rt.evaluate("globalThis.__e.detail.hello").unwrap();
    assert_eq!(d, serde_json::json!("world"));
}

#[tokio::test(flavor = "current_thread")]
async fn test_create_event_returns_correct_class() {
    let mut rt = setup_runtime("<html><body></body></html>");
    let cust = rt
        .evaluate("document.createEvent('CustomEvent') instanceof CustomEvent")
        .unwrap();
    assert_eq!(cust, serde_json::json!(true));
    let mouse = rt
        .evaluate("document.createEvent('MouseEvent') instanceof MouseEvent")
        .unwrap();
    assert_eq!(mouse, serde_json::json!(true));
    let mouses = rt
        .evaluate("document.createEvent('MouseEvents') instanceof MouseEvent")
        .unwrap();
    assert_eq!(mouses, serde_json::json!(true));
    let kb = rt
        .evaluate("document.createEvent('KeyboardEvent') instanceof KeyboardEvent")
        .unwrap();
    assert_eq!(kb, serde_json::json!(true));
}

#[tokio::test(flavor = "current_thread")]
async fn test_create_event_unknown_type_returns_event() {
    let mut rt = setup_runtime("<html><body></body></html>");
    let kind = rt
        .evaluate("document.createEvent('NotARealType') instanceof Event")
        .unwrap();
    assert_eq!(kind, serde_json::json!(true));
}

#[tokio::test(flavor = "current_thread")]
async fn test_page_content_puppeteer_pattern() {
    let mut rt = setup_runtime("<!DOCTYPE html><html><head></head><body><p>Test</p></body></html>");
    let result = rt.evaluate(
        "(function() { let retVal = ''; if (document.doctype) retVal = new XMLSerializer().serializeToString(document.doctype); if (document.documentElement) retVal += document.documentElement.outerHTML; return retVal; })()"
    ).unwrap();
    let html = result.as_str().unwrap();
    assert!(html.starts_with("<!DOCTYPE html>"));
    assert!(html.contains("<html>"));
    assert!(html.contains("<p>Test</p>"));
}

#[tokio::test(flavor = "current_thread")]
async fn test_element_from_point_is_function() {
    let mut rt = setup_runtime("<html><body></body></html>");
    let kind = rt.evaluate("typeof document.elementFromPoint").unwrap();
    assert_eq!(kind, serde_json::json!("function"));
    let kind2 = rt.evaluate("typeof document.elementsFromPoint").unwrap();
    assert_eq!(kind2, serde_json::json!("function"));
}

#[tokio::test(flavor = "current_thread")]
async fn test_element_from_point_in_viewport_returns_body() {
    let mut rt = setup_runtime("<html><body><h1>Hi</h1></body></html>");
    let tag = rt.evaluate("document.elementFromPoint(10, 10)?.tagName").unwrap();
    assert_eq!(tag, serde_json::json!("BODY"));
}

#[tokio::test(flavor = "current_thread")]
async fn test_element_from_point_out_of_viewport_returns_null() {
    let mut rt = setup_runtime("<html><body></body></html>");
    let neg_x = rt.evaluate("document.elementFromPoint(-1, 10)").unwrap();
    assert_eq!(neg_x, serde_json::Value::Null);
    let neg_y = rt.evaluate("document.elementFromPoint(10, -1)").unwrap();
    assert_eq!(neg_y, serde_json::Value::Null);
    let huge = rt.evaluate("document.elementFromPoint(99999, 99999)").unwrap();
    assert_eq!(huge, serde_json::Value::Null);
}

#[tokio::test(flavor = "current_thread")]
async fn test_elements_from_point_returns_array() {
    let mut rt = setup_runtime("<html><body></body></html>");
    let len_in = rt.evaluate("document.elementsFromPoint(10, 10).length").unwrap();
    assert_eq!(len_in.as_f64().unwrap() as i64, 1);
    let len_out = rt.evaluate("document.elementsFromPoint(-1, -1).length").unwrap();
    assert_eq!(len_out.as_f64().unwrap() as i64, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn test_element_from_point_non_numeric_returns_null() {
    let mut rt = setup_runtime("<html><body></body></html>");
    let nan = rt.evaluate("document.elementFromPoint(NaN, 10)").unwrap();
    assert_eq!(nan, serde_json::Value::Null);
    let inf = rt.evaluate("document.elementFromPoint(Infinity, 10)").unwrap();
    assert_eq!(inf, serde_json::Value::Null);
}

// ~keep `proxy_url` must thread through ES-module loading and JS fetch/XHR, or page JS bypasses the proxy.
#[test]
fn http_client_round_trips_proxy_url() {
    use crate::net::{CookieJar, HttpClient};
    let jar = std::sync::Arc::new(CookieJar::new());
    let configured = HttpClient::with_options(jar.clone(), Some("http://proxy.test:8080"));
    assert_eq!(
        configured.proxy_url(),
        Some("http://proxy.test:8080"),
        "proxy_url() must expose the value passed to with_options"
    );

    let direct = HttpClient::with_options(jar, None);
    assert_eq!(
        direct.proxy_url(),
        None,
        "proxy_url() must return None when no proxy was configured"
    );
}

#[test]
fn module_loader_stores_proxy_for_dynamic_imports() {
    use crate::js::module_loader::BrowserModuleLoader;
    let loader = BrowserModuleLoader::with_proxy("https://example.com/", Some("http://proxy.test:8080".to_string()));
    assert_eq!(loader.proxy_url.as_deref(), Some("http://proxy.test:8080"));
    assert_eq!(loader.base_url, "https://example.com/");

    let direct = BrowserModuleLoader::new("https://example.com/");
    assert_eq!(direct.proxy_url, None);
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_with_base_url_and_proxy_constructs_successfully() {
    let _direct = BrowserJsRuntime::with_base_url_and_proxy("https://example.com/", None);
    let _proxied =
        BrowserJsRuntime::with_base_url_and_proxy("https://example.com/", Some("http://proxy.test:8080".to_string()));
}
