use std::time::Duration;

use crawlberg_browser::adapter::{
    NativeActionResult, NativeBrowserConfig, NativeBrowserExecutor, NativeBrowserWait, NativeCookie,
    NativeInteractionResult, NativePageAction, NativeScrollDirection,
};

use super::{DEFAULT_ACTION_TIMEOUT, PageAction, ScrollDirection};
use crate::error::CrawlError;
use crate::types::{ActionResult, AuthConfig, BrowserWait, CrawlConfig, InteractionResult, ProxyConfig};

pub(super) async fn run(
    url: &str,
    actions: &[PageAction],
    config: &CrawlConfig,
    native_executor: &NativeBrowserExecutor,
) -> Result<InteractionResult, CrawlError> {
    if config.browser.endpoint.is_some() {
        return Err(CrawlError::InvalidConfig(
            "browser.endpoint is only supported by the chromiumoxide backend".into(),
        ));
    }

    let native_config = build_native_config(config)?;
    let native_actions = actions.iter().map(map_action).collect::<Vec<_>>();
    let post_navigation_wait = post_navigation_wait(config);
    let timeout = config.browser.timeout;

    // ~keep The native worker executes navigation + all actions (including ExecuteJs, which has
    // ~keep no per-action timeout in the worker) as one job on a dedicated OS thread; a hung
    // ~keep script blocks that thread indefinitely. This wraps the whole call in a budget scaled
    // ~keep by action count so callers get a bounded error instead of hanging forever. It cannot
    // ~keep reclaim the stuck worker thread itself — see interact/native.rs task notes.
    let action_budget = actions
        .iter()
        .map(PageAction::timeout)
        .fold(DEFAULT_ACTION_TIMEOUT, |total, budget| total.saturating_add(budget));
    let overall_timeout = timeout.saturating_add(action_budget);

    let native_result = tokio::time::timeout(
        overall_timeout,
        native_executor.interact_url(url, &native_config, &native_actions, post_navigation_wait),
    )
    .await
    .map_err(|_| {
        CrawlError::BrowserTimeout(format!(
            "browser timed out after {overall_timeout:?} ({} actions)",
            actions.len()
        ))
    })?
    .map_err(|e| {
        let message = e.to_string();
        if message.contains("timed out") {
            CrawlError::BrowserTimeout(format!("browser timed out after {timeout:?}"))
        } else {
            CrawlError::BrowserError(format!("native browser interact failed: {message}"))
        }
    })?;

    Ok(map_result(native_result))
}

fn build_native_config(config: &CrawlConfig) -> Result<NativeBrowserConfig, CrawlError> {
    let mut extra_headers = config.custom_headers.clone();
    match config.auth {
        Some(AuthConfig::Bearer { ref token }) => {
            extra_headers.insert("Authorization".to_owned(), format!("Bearer {token}"));
        }
        Some(AuthConfig::Header { ref name, ref value }) => {
            extra_headers.insert(name.clone(), value.clone());
        }
        _ => {}
    }

    let wait_until = match config.browser.wait {
        BrowserWait::NetworkIdle => NativeBrowserWait::NetworkIdle,
        BrowserWait::Selector => NativeBrowserWait::Selector,
        BrowserWait::Fixed => NativeBrowserWait::Load,
    };

    Ok(NativeBrowserConfig {
        user_agent: config.user_agent.clone(),
        timeout: config.browser.timeout,
        wait_until,
        extra_headers,
        respect_robots_txt: config.respect_robots_txt,
        stealth: matches!(config.browser.mode, crate::types::BrowserMode::Stealth),
        proxy_url: resolved_proxy(config)?,
        prior_cookies: Vec::<NativeCookie>::new(),
        block_url_patterns: config.browser.block_url_patterns.clone(),
        eval_script: config.browser.eval_script.clone(),
        wait_selector: config.browser.wait_selector.clone(),
        robots_user_agent: config.browser.robots_user_agent.clone(),
        capture_network_events: config.browser.capture_network_events,
        ssrf: Some(crate::net::browser_policy::validator_for(&config.ssrf)),
        allow_file_access: false,
    })
}

/// Resolve the proxy URL string handed to the native browser worker.
///
/// Credentials are embedded via `url::Url::set_username`/`set_password`,
/// which percent-encodes the userinfo component — the previous
/// `format!("{scheme}://{user}:{pass}@{rest}")` splice let a `:`, `@`, or
/// `/` in a credential corrupt the authority (e.g. terminate it early and
/// smuggle a different host in, or misdirect the connection to an
/// unintended proxy). `Url::set_username`/`set_password` work for any
/// scheme with an authority component (http, https, socks5, socks5h), so
/// this also fixes SOCKS5 credentials being silently dropped by the old
/// code's `http://`/`https://`-only prefix check.
fn resolved_proxy(config: &CrawlConfig) -> Result<Option<String>, CrawlError> {
    let Some(proxy) = config.browser.proxy.as_ref().or(config.proxy.as_ref()) else {
        return Ok(None);
    };
    apply_proxy_credentials(proxy).map(Some)
}

fn apply_proxy_credentials(proxy: &ProxyConfig) -> Result<String, CrawlError> {
    if proxy.username.is_none() && proxy.password.is_none() {
        return Ok(proxy.url.clone());
    }

    let mut parsed =
        url::Url::parse(&proxy.url).map_err(|e| CrawlError::InvalidConfig(format!("invalid proxy URL: {e}")))?;

    parsed
        .set_username(proxy.username.as_deref().unwrap_or(""))
        .map_err(|()| {
            CrawlError::InvalidConfig(format!(
                "proxy scheme {:?} does not support embedded credentials",
                parsed.scheme()
            ))
        })?;
    parsed.set_password(proxy.password.as_deref()).map_err(|()| {
        CrawlError::InvalidConfig(format!(
            "proxy scheme {:?} does not support embedded credentials",
            parsed.scheme()
        ))
    })?;

    Ok(parsed.to_string())
}

fn post_navigation_wait(config: &CrawlConfig) -> Option<Duration> {
    let fixed_wait = if config.browser.wait == BrowserWait::Fixed {
        Some(Duration::from_secs(2))
    } else {
        None
    };
    match (fixed_wait, config.browser.extra_wait) {
        (Some(base), Some(extra)) => Some(base + extra),
        (Some(base), None) => Some(base),
        (None, extra) => extra,
    }
}

fn map_action(action: &PageAction) -> NativePageAction {
    match action {
        PageAction::Click { selector } => NativePageAction::Click {
            selector: selector.clone(),
        },
        PageAction::TypeText { selector, text } => NativePageAction::TypeText {
            selector: selector.clone(),
            text: text.clone(),
        },
        PageAction::Press { key } => NativePageAction::Press { key: key.clone() },
        PageAction::Scroll {
            direction,
            selector,
            amount,
        } => NativePageAction::Scroll {
            direction: map_scroll_direction(*direction),
            selector: selector.clone(),
            amount: *amount,
        },
        PageAction::Wait { milliseconds, selector } => NativePageAction::Wait {
            milliseconds: *milliseconds,
            selector: selector.clone(),
        },
        PageAction::Screenshot { full_page } => NativePageAction::Screenshot { full_page: *full_page },
        PageAction::ExecuteJs { script } => NativePageAction::ExecuteJs { script: script.clone() },
        PageAction::Scrape => NativePageAction::Scrape,
    }
}

fn map_scroll_direction(direction: ScrollDirection) -> NativeScrollDirection {
    match direction {
        ScrollDirection::Up => NativeScrollDirection::Up,
        ScrollDirection::Down => NativeScrollDirection::Down,
    }
}

fn map_result(result: NativeInteractionResult) -> InteractionResult {
    InteractionResult {
        action_results: result.action_results.into_iter().map(map_action_result).collect(),
        final_html: result.final_html,
        final_url: result.final_url,
        screenshot: result.screenshot,
    }
}

fn map_action_result(result: NativeActionResult) -> ActionResult {
    ActionResult {
        action_index: result.action_index,
        action_type: result.action_type.into(),
        success: result.success,
        data: result.data,
        error: result.error,
    }
}

#[cfg(test)]
mod proxy_credential_tests {
    use super::apply_proxy_credentials;
    use crate::types::ProxyConfig;

    fn proxy(url: &str, username: Option<&str>, password: Option<&str>) -> ProxyConfig {
        ProxyConfig {
            url: url.to_owned(),
            username: username.map(str::to_owned),
            password: password.map(str::to_owned),
            ..ProxyConfig::default()
        }
    }

    #[test]
    fn http_credentials_are_embedded_and_percent_encoded() {
        let resolved = apply_proxy_credentials(&proxy("http://proxy.test:8080", Some("alice"), Some("s3cr3t")))
            .expect("http proxy with plain credentials must resolve");
        assert_eq!(
            resolved, "http://alice:s3cr3t@proxy.test:8080/",
            "plain alphanumeric credentials must round-trip unchanged"
        );
    }

    #[test]
    fn socks5_credentials_are_no_longer_silently_dropped() {
        let resolved = apply_proxy_credentials(&proxy("socks5://proxy.test:1080", Some("alice"), Some("s3cr3t")))
            .expect("socks5 proxy with credentials must resolve");
        assert_eq!(
            resolved, "socks5://alice:s3cr3t@proxy.test:1080",
            "SOCKS5 credentials must be embedded, not dropped"
        );
    }

    #[test]
    fn socks5h_credentials_are_embedded() {
        let resolved = apply_proxy_credentials(&proxy("socks5h://proxy.test:1080", Some("bob"), Some("hunter2")))
            .expect("socks5h proxy with credentials must resolve");
        assert_eq!(resolved, "socks5h://bob:hunter2@proxy.test:1080");
    }

    #[test]
    fn special_characters_in_credentials_are_percent_encoded_not_spliced() {
        // A `:`/`@`/`/` in a credential must not be able to terminate the userinfo early and
        // smuggle in a different host, or split a single credential into `user:pass` pairs.
        let resolved = apply_proxy_credentials(&proxy(
            "http://proxy.test:8080",
            Some("weird:user@name"),
            Some("p/a:s@s"),
        ))
        .expect("proxy with special-character credentials must still resolve");

        // The credentials must decode back to the exact original values, and the host must
        // still be `proxy.test:8080` — not hijacked by a `@` or `:` inside a credential.
        let parsed = url::Url::parse(&resolved).expect("resolved proxy URL must itself be valid");
        assert_eq!(parsed.host_str(), Some("proxy.test"));
        assert_eq!(parsed.port(), Some(8080));
        assert_eq!(parsed.username(), "weird%3Auser%40name");
        assert_eq!(
            urlencoding_decode(parsed.username()),
            "weird:user@name",
            "username must decode back to the exact original value"
        );
        assert_eq!(
            urlencoding_decode(parsed.password().expect("password must be present")),
            "p/a:s@s",
            "password must decode back to the exact original value"
        );
    }

    #[test]
    fn credential_free_proxy_url_is_passed_through_unchanged() {
        let resolved = apply_proxy_credentials(&proxy("http://proxy.test:8080", None, None))
            .expect("credential-free proxy must resolve");
        assert_eq!(
            resolved, "http://proxy.test:8080",
            "URL must be passed through verbatim when there are no credentials to embed"
        );
    }

    #[test]
    fn invalid_proxy_url_returns_invalid_config_error() {
        let result = apply_proxy_credentials(&proxy("not a url", Some("alice"), Some("s3cr3t")));
        assert!(
            matches!(result, Err(crate::error::CrawlError::InvalidConfig(_))),
            "malformed proxy URL with credentials must return InvalidConfig, got {result:?}"
        );
    }

    /// Minimal percent-decoder sufficient for the ASCII userinfo characters this module encodes.
    fn urlencoding_decode(input: &str) -> String {
        let bytes = input.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' && i + 2 < bytes.len() {
                if let Ok(value) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap(), 16) {
                    out.push(value);
                    i += 3;
                    continue;
                }
            }
            out.push(bytes[i]);
            i += 1;
        }
        String::from_utf8(out).expect("decoded bytes must be valid UTF-8")
    }
}

#[cfg(test)]
mod native_worker_hang_tests {
    use std::sync::OnceLock;
    use std::time::Duration;

    use crawlberg_browser::adapter::{NativeBrowserExecutor, NativeBrowserExecutorConfig};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::run;
    use crate::interact::actions::PageAction;
    use crate::types::{BrowserBackend, BrowserConfig, BrowserMode, CrawlConfig};

    static ALLOW_PRIVATE: OnceLock<()> = OnceLock::new();

    fn allow_private_network() {
        ALLOW_PRIVATE.get_or_init(|| {
            // ~keep SAFETY: OnceLock writes this env var once before any network call is made.
            #[allow(unsafe_code)]
            unsafe {
                std::env::set_var("CRAWLBERG_ALLOW_PRIVATE_NETWORK", "1");
            }
        });
    }

    fn native_test_config() -> CrawlConfig {
        allow_private_network();
        CrawlConfig {
            browser: BrowserConfig {
                backend: BrowserBackend::Native,
                mode: BrowserMode::Always,
                timeout: Duration::from_secs(10),
                ..BrowserConfig::default()
            },
            ..CrawlConfig::default()
        }
    }

    /// Proves both halves of the task-1 diagnosis with one deterministic, non-Chrome scenario — the
    /// native backend never launches a real browser subprocess; scripts run through an in-process
    /// `deno_core` V8 isolate, so no `/Applications/Google Chrome.app` dependency is needed here:
    ///
    /// 1. The caller of `native::run` does not hang forever on a non-terminating `ExecuteJs` — the
    ///    short external timeout below fires promptly, because it is polled on the *calling* task's
    ///    OS thread, which is distinct from the dedicated worker thread that actually runs the script.
    /// 2. The worker's dedicated OS thread is genuinely NOT reclaimed. A trivial follow-up job
    ///    submitted to the same single-worker executor also never completes, because that worker
    ///    stays permanently blocked inside the synchronous `JsRuntime::execute_script` call the first
    ///    script triggers. `execute_action`'s own `tokio::time::timeout(ACTION_TIMEOUT, ...)` wrap in
    ///    `crawlberg-browser/src/adapter.rs` never gets a chance to act: `NativePageAction::ExecuteJs`
    ///    resolves via `page.evaluate_result(script)`, a plain synchronous call with no `.await` point,
    ///    so polling that branch never returns control to the worker's executor to check the timer. ~keep
    #[tokio::test]
    async fn hung_execute_js_permanently_pins_the_native_worker_thread() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("<html><body>hang test</body></html>")
                    .append_header("content-type", "text/html"),
            )
            .mount(&mock)
            .await;

        let executor = NativeBrowserExecutor::new(NativeBrowserExecutorConfig::with_workers(1))
            .expect("single-worker executor should start");
        let config = native_test_config();
        let url = mock.uri();

        let hang_actions = vec![PageAction::ExecuteJs {
            script: "while (true) {}".to_owned(),
        }];
        let hang_outcome =
            tokio::time::timeout(Duration::from_secs(5), run(&url, &hang_actions, &config, &executor)).await;
        assert!(
            hang_outcome.is_err(),
            "caller must not hang forever awaiting a non-terminating ExecuteJs action, got {hang_outcome:?}"
        );

        let followup_actions = vec![PageAction::Scrape];
        let followup_outcome =
            tokio::time::timeout(Duration::from_secs(5), run(&url, &followup_actions, &config, &executor)).await;
        assert!(
            followup_outcome.is_err(),
            "a trivial follow-up job on the same single-worker executor must also fail to complete: the sole \
             worker OS thread should still be pinned inside the hung ExecuteJs script if ACTION_TIMEOUT failed \
             to reclaim it, got {followup_outcome:?}"
        );

        // ~keep The worker OS thread never returns from the hung V8 call, so `NativeBrowserExecutor`'s
        // ~keep `Drop` (which joins every worker thread) would block this test process forever. Leak the
        // ~keep executor deliberately instead of letting it drop — this mirrors the real leak the test
        // ~keep demonstrates and keeps the test binary from hanging on exit.
        std::mem::forget(executor);
    }
}
