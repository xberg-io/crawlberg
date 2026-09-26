//! Unit tests for [`super`]'s crawl configuration and its validation.
//!
//! ~keep In its own file because `config.rs` crossed poly's 1000-line limit, and `alef.toml`
//! ~keep exempts `**/*_tests.rs` from the quality metrics, the same split `browser_pool_tests.rs`
//! ~keep uses. Nothing but test code belongs here: a production helper moved in would become
//! ~keep lint-exempt by accident.

use super::*;

/// ~keep A field-level `#[serde(default)]` OVERRIDES the container-level one, substituting
/// `FieldType::default()` for the value the struct's `Default` impl declares. On a struct that
/// already carries `#[serde(default)]` the field attribute is therefore not redundant — it
/// silently cancels the documented default. Deserialising is how every binding builds a config,
/// so a disagreement here ships a different default to every non-Rust caller.
fn assert_default_matches_empty_object<T>(name: &str)
where
    T: Default + Serialize + serde::de::DeserializeOwned,
{
    let from_impl = serde_json::to_value(T::default()).expect("serialize Default");
    let parsed: T = serde_json::from_str("{}").expect("deserialize empty object");
    let from_json = serde_json::to_value(parsed).expect("serialize deserialized");
    assert_eq!(
        from_impl, from_json,
        "{name}::default() and from_str(\"{{}}\") disagree; a field-level #[serde(default)] is \
         overriding the struct's Default impl"
    );
}

#[test]
fn should_deserialize_empty_object_to_the_declared_default() {
    assert_default_matches_empty_object::<CrawlConfig>("CrawlConfig");
    assert_default_matches_empty_object::<ContentConfig>("ContentConfig");
    assert_default_matches_empty_object::<BrowserConfig>("BrowserConfig");
}

#[test]
fn should_keep_nested_defaults_when_one_unrelated_field_is_set() {
    let config: CrawlConfig = serde_json::from_str(r#"{"content":{"remove_forms":true}}"#).expect("parse");
    assert_eq!(
        config.content.exclude_selectors,
        vec!["noscript".to_owned()],
        "setting one content field must not drop the other content defaults"
    );

    let config: CrawlConfig = serde_json::from_str(r#"{"browser":{"capture_network_events":true}}"#).expect("parse");
    assert!(
        config.browser.session_affinity,
        "setting one browser field must not turn off session_affinity, documented as default true"
    );
}

/// Characterization: `validate` reports the FIRST violation, and the order it checks
/// rules in is observable behaviour — a config that breaks several rules gets exactly one
/// message, and which one depends on the check order. Pinned here so the order survives
/// any restructuring of `validate`. ~keep
#[test]
fn validate_reports_violations_in_a_fixed_order() {
    let mut config = maximally_invalid_config();

    for (position, (fragment, repair)) in ORDERED_VIOLATIONS.iter().enumerate() {
        let error = config
            .validate()
            .expect_err(&format!("violation {position} ({fragment}) must still be reported"))
            .to_string();
        assert!(
            error.contains(fragment),
            "violation {position}: expected an error containing {fragment:?}, got: {error}"
        );
        repair(&mut config);
    }

    config.validate().expect("every violation has been repaired");
}

/// Repairs the violation its table entry names, so the next check becomes reachable.
type ConfigRepair = fn(&mut CrawlConfig);

/// A config that breaks every rule `validate` enforces, at once.
fn maximally_invalid_config() -> CrawlConfig {
    let mut config = CrawlConfig {
        max_concurrent: Some(0),
        content_filter: Some(ContentFilterKind::Bm25),
        bm25_query: None,
        max_depth: Some(101),
        max_pages: Some(0),
        max_redirects: 101,
        max_body_size: Some(0),
        proxy: Some(ProxyConfig {
            url: "ftp://proxy.internal:2121".into(),
            ..Default::default()
        }),
        auth: Some(AuthConfig::Bearer { token: String::new() }),
        include_paths: vec!["(unclosed".into()],
        exclude_paths: vec!["(unclosed".into()],
        retry_codes: vec![999],
        retry_count: MAX_RETRY_COUNT + 1,
        request_timeout: Duration::ZERO,
        browser: BrowserConfig {
            wait: BrowserWait::Selector,
            wait_selector: None,
            backend: BrowserBackend::Native,
            endpoint: Some("http://not-websocket:3000".into()),
            chrome_args: vec!["--".into()],
            chrome_path: Some(PathBuf::from("/nonexistent/crawlberg-chrome")),
            ..Default::default()
        },
        ..Default::default()
    };
    config.ssrf.scheme_allowlist = vec!["ftp".to_owned()];
    config
}

/// Every violation `maximally_invalid_config` carries, in the order `validate` reports
/// them, each paired with the repair that unblocks the next one.
const ORDERED_VIOLATIONS: &[(&str, ConfigRepair)] = &[
    ("max_concurrent must be > 0", |c| c.max_concurrent = Some(1)),
    ("bm25_query is required when content_filter is bm25", |c| {
        c.bm25_query = Some("query".to_owned())
    }),
    ("browser.wait_selector required when browser.wait is Selector", |c| {
        c.browser.wait_selector = Some("#main".to_owned())
    }),
    ("max_depth must be <= 100 (got 101)", |c| c.max_depth = Some(100)),
    ("max_pages must be > 0", |c| c.max_pages = Some(1)),
    ("max_redirects must be <= 100", |c| c.max_redirects = 100),
    ("ssrf.scheme_allowlist contains unsupported scheme 'ftp'", |c| {
        c.ssrf.scheme_allowlist = vec!["https".to_owned()]
    }),
    ("max_body_size must be > 0", |c| c.max_body_size = Some(1)),
    ("invalid proxy URL scheme 'ftp'", |c| {
        c.proxy = Some(ProxyConfig {
            url: "http://proxy.internal:8080".into(),
            ..Default::default()
        })
    }),
    ("auth.bearer.token must not be empty", |c| {
        c.auth = Some(AuthConfig::Bearer {
            token: "token".to_owned(),
        })
    }),
    ("invalid include_path regex '(unclosed'", |c| {
        c.include_paths = vec!["^/docs".to_owned()]
    }),
    ("invalid exclude_path regex '(unclosed'", |c| {
        c.exclude_paths = vec!["^/private".to_owned()]
    }),
    ("invalid retry code: 999", |c| c.retry_codes = vec![503]),
    ("retry_count must be <= 20 (got 21)", |c| {
        c.retry_count = MAX_RETRY_COUNT
    }),
    ("request_timeout must be > 0", |c| {
        c.request_timeout = Duration::from_secs(30)
    }),
    ("browser.endpoint must start with ws:// or wss://", |c| {
        c.browser.endpoint = Some("ws://localhost:9222".to_owned())
    }),
    // ~keep The repair also clears the endpoint: with an endpoint set no Chrome is
    // ~keep launched, so the launch-option checks below would be skipped.
    ("browser.endpoint is only supported by the chromiumoxide backend", |c| {
        c.browser.backend = BrowserBackend::Chromiumoxide;
        c.browser.endpoint = None;
    }),
    ("browser.chrome_args entry \"--\" must start with --", |c| {
        c.browser.chrome_args = vec!["--disable-gpu".to_owned()]
    }),
    (
        "browser.chrome_path '/nonexistent/crawlberg-chrome' cannot be used",
        |c| c.browser.chrome_path = None,
    ),
];

#[test]
fn validate_rejects_an_absurd_retry_count() {
    let config = CrawlConfig {
        retry_count: 1_000_000,
        ..Default::default()
    };
    let err = config.validate().unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("retry_count must be <= 20"),
        "expected a retry_count bound error, got: {msg}"
    );
}

#[test]
fn validate_accepts_the_maximum_allowed_retry_count() {
    let config = CrawlConfig {
        retry_count: 20,
        ..Default::default()
    };
    assert!(config.validate().is_ok(), "retry_count at the bound must be accepted");
}

#[test]
fn validate_rejects_http_browser_endpoint() {
    let config = CrawlConfig {
        browser: BrowserConfig {
            endpoint: Some("http://not-websocket:3000".into()),
            ..Default::default()
        },
        ..Default::default()
    };
    let err = config.validate().unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("endpoint"), "error should mention 'endpoint', got: {msg}");
}

#[test]
fn validate_rejects_unsupported_ssrf_scheme_allowlist_entries() {
    for scheme in ["ftp", "http://"] {
        let mut config = CrawlConfig::default();
        config.ssrf.scheme_allowlist = vec![scheme.to_owned()];

        let error = config.validate().expect_err("only HTTP transports are supported");
        assert!(
            error.to_string().contains(scheme),
            "validation error must identify the unsupported scheme, got: {error}"
        );
    }

    let mut config = CrawlConfig::default();
    config.ssrf.scheme_allowlist = vec!["http".to_owned(), "HTTP".to_owned()];
    let error = config
        .validate()
        .expect_err("scheme matching is case-insensitive, so case variants are duplicates");
    assert!(
        error.to_string().contains("duplicate scheme 'HTTP'"),
        "validation error must identify the duplicate scheme, got: {error}"
    );
}

#[test]
fn validate_accepts_ws_endpoint() {
    let config = CrawlConfig {
        browser: BrowserConfig {
            endpoint: Some("ws://localhost:9222".into()),
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(config.validate().is_ok());
}

#[test]
fn validate_accepts_wss_endpoint() {
    let config = CrawlConfig {
        browser: BrowserConfig {
            endpoint: Some("wss://remote-browser.example.com/devtools".into()),
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(config.validate().is_ok());
}

#[test]
fn validate_accepts_no_endpoint() {
    let config = CrawlConfig {
        browser: BrowserConfig {
            endpoint: None,
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(config.validate().is_ok());
}

#[test]
fn browser_backend_defaults_to_chromiumoxide() {
    assert_eq!(BrowserConfig::default().backend, BrowserBackend::Chromiumoxide);
}

#[test]
fn validate_rejects_native_endpoint() {
    let config = CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Native,
            endpoint: Some("ws://localhost:9222".into()),
            ..Default::default()
        },
        ..Default::default()
    };
    let err = config.validate().unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("chromiumoxide"), "unexpected error: {msg}");
}

#[test]
fn proxy_config_debug_redacts_password_and_url_userinfo() {
    let proxy = ProxyConfig {
        url: "http://svc-account:hunter2@proxy.internal:8080".into(),
        username: Some("svc-account".into()),
        password: Some("hunter2".into()),
    };
    let rendered = format!("{proxy:?}");
    assert!(
        !rendered.contains("hunter2"),
        "Debug output must not contain the raw password, got '{rendered}'"
    );
    assert!(
        rendered.contains("svc-account"),
        "Debug output should still show the non-secret username, got '{rendered}'"
    );
}

#[test]
fn proxy_config_debug_shows_none_when_password_unset() {
    let proxy = ProxyConfig {
        url: "http://proxy.internal:8080".into(),
        username: None,
        password: None,
    };
    let rendered = format!("{proxy:?}");
    assert!(
        rendered.contains("password: None"),
        "unset password must render as None, got '{rendered}'"
    );
}

#[test]
fn auth_config_debug_redacts_basic_password() {
    let auth = AuthConfig::Basic {
        username: "alice".into(),
        password: "hunter2".into(),
    };
    let rendered = format!("{auth:?}");
    assert!(
        !rendered.contains("hunter2"),
        "Debug output must not contain the raw password, got '{rendered}'"
    );
    assert!(
        rendered.contains("alice"),
        "Debug output should still show the non-secret username, got '{rendered}'"
    );
}

#[test]
fn auth_config_debug_redacts_bearer_token() {
    let auth = AuthConfig::Bearer {
        token: "sk-super-secret-token".into(),
    };
    let rendered = format!("{auth:?}");
    assert!(
        !rendered.contains("sk-super-secret-token"),
        "Debug output must not contain the raw bearer token, got '{rendered}'"
    );
}

#[test]
fn auth_config_debug_redacts_header_value() {
    let auth = AuthConfig::Header {
        name: "X-Api-Key".into(),
        value: "sk-super-secret-key".into(),
    };
    let rendered = format!("{auth:?}");
    assert!(
        !rendered.contains("sk-super-secret-key"),
        "Debug output must not contain the raw header value, got '{rendered}'"
    );
    assert!(
        rendered.contains("X-Api-Key"),
        "Debug output should still show the non-secret header name, got '{rendered}'"
    );
}

fn config_with_chrome(chrome_path: Option<PathBuf>, chrome_args: Vec<String>) -> CrawlConfig {
    CrawlConfig {
        browser: BrowserConfig {
            chrome_path,
            chrome_args,
            ..Default::default()
        },
        ..Default::default()
    }
}

#[test]
fn validate_rejects_an_empty_or_bare_double_dash_chrome_arg() {
    for bad in ["", "--"] {
        let err = config_with_chrome(None, vec!["--disable-gpu".into(), bad.into()])
            .validate()
            .expect_err("an empty chrome_args entry must be rejected")
            .to_string();
        assert!(
            err.contains("browser.chrome_args"),
            "entry {bad:?}: unexpected error: {err}"
        );
    }
}

#[test]
fn validate_rejects_a_chrome_arg_without_a_leading_double_dash() {
    // ~keep `["--user-agent", "x"]` must not turn `x` into a stray `--x` flag, and a
    // ~keep single-dash spelling must not slip past the launch-owned check.
    for bad in [
        "x",
        "user-agent=x",
        "-user-data-dir=/tmp/elsewhere",
        "-headless",
        "---headless",
        "----headless",
    ] {
        let err = config_with_chrome(None, vec!["--user-agent".into(), bad.into()])
            .validate()
            .expect_err("an entry without a leading -- must be rejected")
            .to_string();
        assert!(
            err.contains("must start with --") && err.contains("--flag=value"),
            "{bad}: unexpected error: {err}"
        );
    }
}

/// With `browser.endpoint` set or the native backend selected, no Chrome is launched, so
/// `chrome_path` and `chrome_args` are ignored at fetch time with a warning and must not
/// refuse the configuration here.
#[test]
fn validate_skips_the_chrome_launch_options_when_they_are_ignored() {
    let bad = |backend: BrowserBackend, endpoint: Option<&str>| CrawlConfig {
        browser: BrowserConfig {
            backend,
            endpoint: endpoint.map(str::to_owned),
            chrome_path: Some(PathBuf::from("/nonexistent/crawlberg-chrome")),
            chrome_args: vec!["x".into(), "--headless".into()],
            ..Default::default()
        },
        ..Default::default()
    };
    bad(
        BrowserBackend::Chromiumoxide,
        Some("ws://127.0.0.1:9222/devtools/browser/x"),
    )
    .validate()
    .expect("an endpoint config must not be refused for launch options it ignores");
    bad(BrowserBackend::Native, None)
        .validate()
        .expect("a native-backend config must not be refused for launch options it ignores");
    bad(BrowserBackend::Chromiumoxide, None)
        .validate()
        .expect_err("a launching config must still check its launch options");
}

#[cfg(feature = "browser")]
#[test]
fn validate_still_checks_the_chrome_launch_options_with_a_shared_browser_pool() {
    let config = CrawlConfig {
        browser: BrowserConfig {
            chrome_path: Some(PathBuf::from("/nonexistent/crawlberg-chrome")),
            ..Default::default()
        },
        browser_pool: Some(crate::browser_pool::BrowserPool::new(
            crate::browser_pool::BrowserPoolConfig::default(),
        )),
        ..Default::default()
    };
    // ~keep `interact()` ignores the pool and launches Chrome from these fields.
    config
        .validate()
        .expect_err("interact() launches from these options even with a pool, so they are checked");
}

#[test]
fn validate_rejects_a_chrome_arg_the_launch_sets_itself() {
    for owned in [
        "--user-data-dir=/tmp/elsewhere",
        "--headless",
        "--remote-debugging-port=9222",
    ] {
        let err = config_with_chrome(None, vec![owned.into()])
            .validate()
            .expect_err("a launch-owned flag must be rejected")
            .to_string();
        assert!(
            err.contains("crawlberg sets it to run Chrome"),
            "{owned}: unexpected error: {err}"
        );
    }
}

#[test]
fn validate_rejects_a_chrome_arg_name_with_an_uppercase_letter() {
    for mixed_case in [
        "--Headless",
        "--USER-DATA-DIR=/tmp/elsewhere",
        "--User-Agent=b",
        "--LANG=fr",
    ] {
        let err = config_with_chrome(None, vec![mixed_case.into()])
            .validate()
            .expect_err("a flag name with an uppercase letter must be rejected")
            .to_string();
        assert!(err.contains("in lowercase"), "{mixed_case}: unexpected error: {err}");
    }
    config_with_chrome(None, vec!["--user-agent=Mozilla/5.0 Crawlberg".into()])
        .validate()
        .expect("uppercase letters in a flag's value must be accepted");
}

#[test]
fn validate_rejects_a_chrome_arg_named_twice() {
    let err = config_with_chrome(None, vec!["--user-agent=a".into(), "--user-agent=b".into()])
        .validate()
        .expect_err("a flag named twice must be rejected")
        .to_string();
    assert!(
        err.contains("sets --user-agent more than once"),
        "unexpected error: {err}"
    );
}

#[test]
fn validate_rejects_a_missing_chrome_path_and_names_it() {
    let err = config_with_chrome(Some(PathBuf::from("/nonexistent/crawlberg-chrome")), Vec::new())
        .validate()
        .expect_err("a missing chrome_path must be rejected")
        .to_string();
    assert!(
        err.contains("/nonexistent/crawlberg-chrome"),
        "the error must name the path, got: {err}"
    );
}

#[test]
fn validate_rejects_a_chrome_path_that_is_a_directory() {
    let dir = std::env::temp_dir();
    let err = config_with_chrome(Some(dir.clone()), Vec::new())
        .validate()
        .expect_err("a directory is not a Chrome binary")
        .to_string();
    assert!(err.contains("is not a file"), "unexpected error: {err}");
    assert!(
        err.contains(&dir.display().to_string()),
        "the error must name the path, got: {err}"
    );
}

#[cfg(unix)]
#[test]
fn validate_rejects_a_chrome_path_without_the_execute_bit() {
    use std::os::unix::fs::PermissionsExt;
    let path = executable_temp_file("no-exec");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod must succeed");
    let result = config_with_chrome(Some(path.clone()), Vec::new()).validate();
    let _ = std::fs::remove_file(&path);
    let err = result
        .expect_err("a file without the execute bit must be rejected")
        .to_string();
    assert!(err.contains("is not executable"), "unexpected error: {err}");
}

#[test]
fn validate_accepts_an_executable_chrome_path_and_non_empty_chrome_args() {
    let path = executable_temp_file("accept");
    let result = config_with_chrome(Some(path.clone()), vec!["--disable-gpu".into(), "--lang=fr".into()]).validate();
    let _ = std::fs::remove_file(&path);
    result.expect("an executable chrome_path and non-empty chrome_args must be accepted");
}
