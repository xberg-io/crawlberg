use super::*;
use crate::dom::parse_html;

mod document;

fn setup_runtime(html: &str) -> BrowserJsRuntime {
    let dom = parse_html(html);
    let rt = BrowserJsRuntime::new();
    rt.set_dom(dom);
    rt.set_url("http://example.com/test");
    rt.set_title("Test Page");
    rt
}

#[tokio::test(flavor = "current_thread")]
async fn test_document_title() {
    let mut rt = setup_runtime("<html><head><title>Test</title></head><body></body></html>");
    let title = rt.evaluate("document.title").unwrap();
    assert_eq!(title, serde_json::json!("Test Page"));
}

#[tokio::test(flavor = "current_thread")]
async fn test_document_url() {
    let mut rt = setup_runtime("<html><body></body></html>");
    let url = rt.evaluate("document.URL").unwrap();
    assert_eq!(url, serde_json::json!("http://example.com/test"));
}

#[tokio::test(flavor = "current_thread")]
async fn test_query_selector() {
    let mut rt = setup_runtime("<html><body><h1>Hello</h1><p>World</p></body></html>");
    let text = rt.evaluate("document.querySelector('h1').textContent").unwrap();
    assert_eq!(text, serde_json::json!("Hello"));
}

#[tokio::test(flavor = "current_thread")]
async fn test_query_selector_all() {
    let mut rt = setup_runtime("<ul><li>A</li><li>B</li><li>C</li></ul>");
    let count = rt.evaluate("document.querySelectorAll('li').length").unwrap();
    assert_eq!(count.as_f64().unwrap() as i64, 3);
}

#[tokio::test(flavor = "current_thread")]
async fn test_get_element_by_id() {
    let mut rt = setup_runtime(r#"<div id="test">Content</div>"#);
    let tag = rt.evaluate("document.getElementById('test').tagName").unwrap();
    assert_eq!(tag, serde_json::json!("DIV"));
}

#[tokio::test(flavor = "current_thread")]
async fn test_inner_html() {
    let mut rt = setup_runtime(r#"<div id="x"><p>Hello</p></div>"#);
    let html = rt.evaluate("document.getElementById('x').innerHTML").unwrap();
    assert!(html.as_str().unwrap().contains("<p>"));
}

#[tokio::test(flavor = "current_thread")]
async fn test_script_execution() {
    let mut rt = setup_runtime("<ul><li>A</li><li>B</li></ul>");
    rt.execute_script(
        "test",
        r#"
        globalThis.__result = [];
        document.querySelectorAll('li').forEach(function(el) {
            globalThis.__result.push(el.textContent);
        });
    "#,
    )
    .unwrap();
    let result = rt.evaluate("globalThis.__result").unwrap();
    assert_eq!(result, serde_json::json!(["A", "B"]));
}

/// Regression: a sub-10KB script with an infinite loop must not wedge
/// the worker thread forever. `execute_script_guarded` previously skipped
/// the watchdog for scripts under 10_000 bytes, so a 13-byte
/// `while(true){}` spun V8 at 100% CPU with no recovery (observed on
/// staging: three worker threads pinned for six hours). Every script now
/// gets the wall-clock execution bound regardless of size.
#[tokio::test(flavor = "current_thread")]
async fn execute_script_guarded_kills_small_infinite_loop() {
    let mut rt = setup_runtime("<html><body></body></html>");
    let start = std::time::Instant::now();
    let result = rt.execute_script_guarded("evil", "while(true){}");
    let elapsed = start.elapsed();
    assert!(result.is_ok(), "guarded execution should recover, got {result:?}");
    assert!(
        elapsed < std::time::Duration::from_secs(15),
        "infinite loop must be killed by the watchdog, ran for {elapsed:?}"
    );
    rt.execute_script("after", "globalThis.__alive = 1;").unwrap();
    let alive = rt.evaluate("globalThis.__alive").unwrap();
    assert_eq!(alive.as_f64().unwrap() as i64, 1);
}

/// Regression test for #147: a TypeError in one script must not poison
/// the runtime so that subsequent scripts (or DOM queries) collapse to
/// empty. The reporter saw `--dump text` return 1 byte after offside.js
/// crashed; that cascade should never happen.
#[tokio::test(flavor = "current_thread")]
async fn script_typeerror_does_not_poison_subsequent_execution() {
    let mut rt = setup_runtime("<html><body><p id=hit>BODY_TEXT</p></body></html>");

    let err = rt.execute_script("buggy", "var x; x.classList.add('y');").unwrap_err();
    assert!(
        err.contains("classList") || err.contains("undefined"),
        "expected classList/undefined error, got: {}",
        err
    );

    rt.execute_script("ok", "globalThis.__after_error = 'still alive';")
        .unwrap();
    let result = rt.evaluate("globalThis.__after_error").unwrap();
    assert_eq!(result, serde_json::json!("still alive"));

    let text = rt.evaluate("document.querySelector('#hit').textContent").unwrap();
    assert_eq!(text, serde_json::json!("BODY_TEXT"));
}

#[tokio::test(flavor = "current_thread")]
async fn test_console_log() {
    let mut rt = setup_runtime("<html><body></body></html>");
    rt.execute_script("test", "console.log('Hello from V8!')").unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn test_location() {
    let mut rt = setup_runtime("<html><body></body></html>");
    let href = rt.evaluate("location.href").unwrap();
    assert_eq!(href, serde_json::json!("http://example.com/test"));
}

#[tokio::test(flavor = "current_thread")]
async fn test_button_click_dispatches_listener() {
    let mut rt = setup_runtime(r#"<button id="go">Go</button>"#);
    let result = rt
        .evaluate(
            r#"
        const button = document.getElementById('go');
        button.addEventListener('click', () => { button.dataset.clicked = 'yes'; });
        button.click();
        return button.dataset.clicked;
    "#,
        )
        .unwrap();
    assert_eq!(result, serde_json::json!("yes"));
}

#[tokio::test(flavor = "current_thread")]
async fn test_dispatch_mouse_event_runs_listener() {
    let mut rt = setup_runtime(r#"<button id="go">Go</button>"#);
    let result = rt
        .evaluate(
            r#"
        const button = document.getElementById('go');
        let count = 0;
        button.addEventListener('click', () => { count += 1; });
        button.dispatchEvent(new MouseEvent('click', { bubbles: true }));
        return count;
    "#,
        )
        .unwrap();
    assert_eq!(result.as_f64().unwrap() as i64, 1);
}

#[tokio::test(flavor = "current_thread")]
async fn test_location_href_assignment_updates_navigation_state() {
    let mut rt = setup_runtime("<html><body></body></html>");
    let href = rt
        .evaluate("const next = '/next'; location.href = next; return location.href;")
        .unwrap();
    assert_eq!(href, serde_json::json!("http://example.com/next"));
    assert_eq!(
        rt.take_pending_navigation(),
        Some(("http://example.com/next".to_string(), "GET".to_string(), "".to_string()))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn test_submit_button_click_handler_can_prevent_default_and_navigate() {
    let mut rt = setup_runtime(r#"<form><button type="submit" id="submit">Submit</button></form>"#);
    let href = rt
        .evaluate(
            r#"
        const form = document.querySelector('form');
        form.addEventListener('submit', (event) => {
            event.preventDefault();
            location.href = '/submitted';
        });
        document.getElementById('submit').click();
        return location.href;
    "#,
        )
        .unwrap();
    assert_eq!(href, serde_json::json!("http://example.com/submitted"));
    assert_eq!(
        rt.take_pending_navigation(),
        Some((
            "http://example.com/submitted".to_string(),
            "GET".to_string(),
            "".to_string()
        ))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn test_navigator() {
    let mut rt = setup_runtime("<html><body></body></html>");
    let ua = rt.evaluate("navigator.userAgent").unwrap();
    assert!(
        ua.as_str().unwrap().contains("Chrome"),
        "UA should contain Chrome: {}",
        ua
    );
    let wd = rt.evaluate("navigator.webdriver").unwrap();
    assert_eq!(wd, serde_json::Value::Null);
    let plugins = rt.evaluate("navigator.plugins.length").unwrap();
    assert!(plugins.as_f64().unwrap() > 0.0, "Should have plugins");
    let chrome = rt.evaluate("typeof window.chrome").unwrap();
    assert_eq!(chrome, serde_json::json!("object"));
}

#[tokio::test(flavor = "current_thread")]
async fn test_call_function_on_no_args() {
    let mut rt = setup_runtime("<html><head><title>Test</title></head><body></body></html>");
    let result = rt
        .call_function_on("() => document.title", None, &[], true)
        .await
        .unwrap();
    assert_eq!(result.value.unwrap(), serde_json::json!("Test Page"));
}

#[tokio::test(flavor = "current_thread")]
async fn test_call_function_on_with_args() {
    let mut rt = setup_runtime("<html><body></body></html>");
    let args = vec![serde_json::json!({"value": 10}), serde_json::json!({"value": 20})];
    let result = rt.call_function_on("(a, b) => a + b", None, &args, true).await.unwrap();
    assert_eq!(result.value.unwrap().as_f64().unwrap() as i64, 30);
}

#[tokio::test(flavor = "current_thread")]
async fn test_call_function_on_with_string_args() {
    let mut rt = setup_runtime("<html><body></body></html>");
    let args = vec![
        serde_json::json!({"value": "hello"}),
        serde_json::json!({"value": " world"}),
    ];
    let result = rt.call_function_on("(a, b) => a + b", None, &args, true).await.unwrap();
    assert_eq!(result.value.unwrap(), serde_json::json!("hello world"));
}

#[tokio::test(flavor = "current_thread")]
async fn test_call_function_on_with_object_args() {
    let mut rt = setup_runtime("<html><body></body></html>");
    let args = vec![serde_json::json!({"value": {"name": "test", "count": 5}})];
    let result = rt
        .call_function_on("(obj) => obj.name + ':' + obj.count", None, &args, true)
        .await
        .unwrap();
    assert_eq!(result.value.unwrap(), serde_json::json!("test:5"));
}

#[tokio::test(flavor = "current_thread")]
async fn test_call_function_on_return_object() {
    let mut rt = setup_runtime("<html><body></body></html>");
    let result = rt
        .call_function_on("() => ({a: 1, b: 2})", None, &[], true)
        .await
        .unwrap();
    assert_eq!(result.value.unwrap(), serde_json::json!({"a": 1, "b": 2}));
}

#[tokio::test(flavor = "current_thread")]
async fn test_call_function_on_object_ref_preserves_methods() {
    let mut rt = setup_runtime("<html><body></body></html>");
    let result = rt
        .call_function_on(
            "() => ({ items: [1,2,3], getLen: function() { return this.items.length; } })",
            None,
            &[],
            false,
        )
        .await
        .unwrap();
    let oid = result.object_id.unwrap();

    let result2 = rt
        .call_function_on("function() { return this.getLen(); }", Some(&oid), &[], true)
        .await
        .unwrap();
    assert_eq!(result2.value.unwrap().as_f64().unwrap() as i64, 3);
}

#[tokio::test(flavor = "current_thread")]
async fn test_evaluate_for_cdp_detects_node() {
    let mut rt = setup_runtime("<html><body><h1>Hello</h1></body></html>");
    let result = rt
        .evaluate_for_cdp("document.querySelector('h1')", false, false)
        .await
        .unwrap();
    assert_eq!(result.subtype.as_deref(), Some("node"));
    assert_eq!(result.js_type, "object");
    assert!(result.object_id.is_some());
}

#[tokio::test(flavor = "current_thread")]
async fn test_evaluate_for_cdp_detects_document() {
    let mut rt = setup_runtime("<html><body></body></html>");
    let result = rt.evaluate_for_cdp("document", false, false).await.unwrap();
    assert_eq!(result.subtype.as_deref(), Some("node"));
    assert_eq!(result.class_name, "HTMLDocument");
}

#[tokio::test(flavor = "current_thread")]
async fn test_evaluate_for_cdp_awaits_resolved_promise() {
    let mut rt = setup_runtime("<html><body></body></html>");
    let result = rt.evaluate_for_cdp("Promise.resolve(42)", true, true).await.unwrap();
    assert_eq!(result.value.unwrap().as_f64().unwrap() as i64, 42);
}

/// Characterization for the `await_promise && return_by_value` combinator branch in
/// `evaluate_for_cdp` with `return_by_value: false`: the result must come back as a stored
/// remote-object reference (an `object_id`) rather than an inline `value`, which the other
/// `evaluate_for_cdp` tests here do not exercise.
#[tokio::test(flavor = "current_thread")]
async fn test_evaluate_for_cdp_awaits_promise_without_return_by_value() {
    let mut rt = setup_runtime("<html><body></body></html>");
    let result = rt
        .evaluate_for_cdp("Promise.resolve({ x: 1 })", false, true)
        .await
        .unwrap();
    assert!(result.object_id.is_some(), "expected a stored object reference");
    assert_eq!(result.js_type, "object");
}

#[tokio::test(flavor = "current_thread")]
async fn test_evaluate_for_cdp_awaits_timer_promise() {
    let mut rt = setup_runtime("<html><body></body></html>");
    let result = rt
        .evaluate_for_cdp(
            "new Promise(resolve => setTimeout(() => resolve('done'), 1))",
            true,
            true,
        )
        .await
        .unwrap();
    assert_eq!(result.value.unwrap().as_str().unwrap(), "done");
}

#[tokio::test(flavor = "current_thread")]
async fn test_evaluate_for_cdp_awaits_async_function() {
    let mut rt = setup_runtime("<html><body></body></html>");
    let result = rt
        .evaluate_for_cdp("(async () => 'async-ok')()", true, true)
        .await
        .unwrap();
    assert_eq!(result.value.unwrap().as_str().unwrap(), "async-ok");
}

#[tokio::test(flavor = "current_thread")]
async fn test_evaluate_for_cdp_reports_promise_rejection() {
    let mut rt = setup_runtime("<html><body></body></html>");
    let err = rt
        .evaluate_for_cdp("Promise.reject(new Error('boom'))", true, true)
        .await
        .unwrap_err();
    assert!(err.contains("boom"));
}

#[tokio::test(flavor = "current_thread")]
async fn test_call_function_on_dom_interaction() {
    let mut rt = setup_runtime(r#"<div id="items"><span>A</span><span>B</span></div>"#);
    let args = vec![serde_json::json!({"value": "span"})];
    let result = rt
        .call_function_on("(sel) => document.querySelectorAll(sel).length", None, &args, true)
        .await
        .unwrap();
    assert_eq!(result.value.unwrap().as_f64().unwrap() as i64, 2);
}

#[tokio::test(flavor = "current_thread")]
async fn test_inner_html_setter() {
    let mut rt = setup_runtime(r#"<div id="target"><p>Old</p></div>"#);
    rt.execute_script(
        "test",
        r#"
        var el = document.getElementById('target');
        el.innerHTML = '<strong>Bold</strong><em>Italic</em>';
    "#,
    )
    .unwrap();
    let result = rt.evaluate("document.getElementById('target').innerHTML").unwrap();
    let html = result.as_str().unwrap();
    assert!(
        html.contains("<strong>"),
        "innerHTML should contain <strong>, got: {}",
        html
    );
    assert!(html.contains("<em>"), "innerHTML should contain <em>, got: {}", html);
    assert!(
        !html.contains("Old"),
        "innerHTML should not contain old content, got: {}",
        html
    );
}

#[tokio::test(flavor = "current_thread")]
async fn test_inner_html_with_nested() {
    let mut rt = setup_runtime(r#"<div id="root"></div>"#);
    rt.execute_script(
        "test",
        r#"
        var el = document.getElementById('root');
        el.innerHTML = '<ul><li>A</li><li>B</li><li>C</li></ul>';
    "#,
    )
    .unwrap();
    let count = rt.evaluate("document.querySelectorAll('li').length").unwrap();
    assert_eq!(
        count.as_f64().unwrap() as i64,
        3,
        "Should find 3 li elements after innerHTML set"
    );

    let text = rt.evaluate("document.querySelector('li').textContent").unwrap();
    assert_eq!(text, serde_json::json!("A"));
}

#[tokio::test(flavor = "current_thread")]
async fn test_input_value() {
    let mut rt = setup_runtime(
        r#"<form><input id="name" type="text" value="initial"><textarea id="bio">old text</textarea></form>"#,
    );
    let val = rt.evaluate("document.getElementById('name').value").unwrap();
    assert_eq!(val, serde_json::json!("initial"));
    rt.execute_script("test", "document.getElementById('name').value = 'new value';")
        .unwrap();
    let val2 = rt.evaluate("document.getElementById('name').value").unwrap();
    assert_eq!(val2, serde_json::json!("new value"));
    let bio = rt.evaluate("document.getElementById('bio').value").unwrap();
    assert_eq!(bio, serde_json::json!("old text"));
}

#[tokio::test(flavor = "current_thread")]
async fn test_sequential_runtime_swap() {
    let mut rt1 = setup_runtime("<html><body><h1>Page1</h1></body></html>");
    let title1 = rt1.evaluate("document.querySelector('h1').textContent").unwrap();
    assert_eq!(title1, serde_json::json!("Page1"));

    let dom1 = rt1.take_dom();
    drop(rt1);

    let mut rt2 = setup_runtime("<html><body><h1>Page2</h1></body></html>");
    let title2 = rt2.evaluate("document.querySelector('h1').textContent").unwrap();
    assert_eq!(title2, serde_json::json!("Page2"));
    drop(rt2);

    if let Some(dom) = dom1 {
        let rt1b = BrowserJsRuntime::new();
        rt1b.set_dom(dom);
        rt1b.set_url("http://example.com");
        rt1b.set_title("Page1");
        let mut rt1b = rt1b;
        let title1b = rt1b.evaluate("document.querySelector('h1').textContent").unwrap();
        assert_eq!(title1b, serde_json::json!("Page1"));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn test_checkbox_checked() {
    let mut rt = setup_runtime(r#"<input id="cb" type="checkbox" checked>"#);
    let checked = rt.evaluate("document.getElementById('cb').checked").unwrap();
    assert_eq!(checked, serde_json::json!(true));
    rt.execute_script("test", "document.getElementById('cb').checked = false;")
        .unwrap();
    let checked2 = rt.evaluate("document.getElementById('cb').checked").unwrap();
    assert_eq!(checked2, serde_json::json!(false));
}

#[tokio::test(flavor = "current_thread")]
async fn test_matches_and_closest() {
    let mut rt = setup_runtime(r#"<div class="outer"><div class="inner"><span id="target">Hi</span></div></div>"#);
    let matches = rt
        .evaluate("document.getElementById('target').matches('span')")
        .unwrap();
    assert_eq!(matches, serde_json::json!(true));
    let closest = rt
        .evaluate("document.getElementById('target').closest('.outer').className")
        .unwrap();
    assert_eq!(closest, serde_json::json!("outer"));
    let no_match = rt
        .evaluate("document.getElementById('target').closest('.nonexistent')")
        .unwrap();
    assert_eq!(no_match, serde_json::Value::Null);
}

#[tokio::test(flavor = "current_thread")]
async fn test_clone_node_deep() {
    let mut rt = setup_runtime(r#"<div id="src"><p>A</p><p>B</p></div>"#);
    rt.execute_script(
        "test",
        r#"
        var src = document.getElementById('src');
        var clone = src.cloneNode(true);
        document.body.appendChild(clone);
    "#,
    )
    .unwrap();
    let count = rt.evaluate("document.querySelectorAll('p').length").unwrap();
    assert!(
        count.as_f64().unwrap() as i64 >= 4,
        "Deep clone should duplicate <p> children, got: {}",
        count
    );
}

#[tokio::test(flavor = "current_thread")]
async fn test_evaluate_multistatement() {
    let mut rt = setup_runtime("<html><body></body></html>");
    let result = rt.evaluate("var x = 5; var y = 10; return x + y;").unwrap();
    assert_eq!(result.as_f64().unwrap() as i64, 15);
}

#[tokio::test(flavor = "current_thread")]
async fn test_object_ref_as_argument() {
    let mut rt = setup_runtime("<html><body></body></html>");
    let obj = rt
        .call_function_on("() => ({ x: 42 })", None, &[], false)
        .await
        .unwrap();
    let oid = obj.object_id.unwrap();

    let args = vec![serde_json::json!({"objectId": oid})];
    let result = rt
        .call_function_on("(obj) => obj.x * 2", None, &args, true)
        .await
        .unwrap();
    assert_eq!(result.value.unwrap().as_f64().unwrap() as i64, 84);
}
