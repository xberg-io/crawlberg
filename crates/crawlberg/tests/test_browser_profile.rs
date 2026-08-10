//! Chrome-backed integration tests proving `CrawlConfig.browser_profile` and
//! `save_browser_profile` are actually wired into the one-shot (non-pooled)
//! chromiumoxide launch path in `crates/crawlberg/src/browser.rs`.
//!
//! Before this fix, both fields had zero reads outside `browser_profile.rs`
//! itself: configuring a named profile did nothing, and Chrome always
//! launched against a throwaway temp directory. These tests require a real
//! Chrome binary (found at `/Applications/Google Chrome.app` in this
//! environment; chromiumoxide auto-detects it) and are gated behind the
//! `browser` feature, matching the pattern used by
//! `test_browser_pool_lifecycle.rs` and `test_browser_native.rs`.
//!
//! Only the non-pooled launch path is covered here: a shared `BrowserPool` is
//! launched once, ahead of any per-crawl `CrawlConfig`, so a profile named on
//! a later crawl cannot retroactively change that already-running process's
//! `--user-data-dir` (see the `tracing::warn!` in `browser.rs`'s pooled
//! branch). Profile persistence with a shared pool remains unproven by
//! design, not by omission.

#![cfg(feature = "browser")]

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crawlberg::{BrowserBackend, BrowserConfig, BrowserMode, BrowserProfile, CrawlConfig, create_engine, scrape};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

static ALLOW_PRIVATE: OnceLock<()> = OnceLock::new();
static NAME_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Opts into the SSRF policy's private-network allowance so the loopback test
/// server below is reachable.
fn allow_private_network() {
    ALLOW_PRIVATE.get_or_init(|| {
        // ~keep SAFETY: OnceLock writes this env var once before any network call is made.
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("CRAWLBERG_ALLOW_PRIVATE_NETWORK", "1");
        }
    });
}

/// A collision-free profile name for this test process/run, so parallel test
/// runs never share (or race on deleting) the same on-disk profile directory.
fn unique_profile_name(tag: &str) -> String {
    format!(
        "crawlberg-test-{tag}-{}-{}",
        std::process::id(),
        NAME_COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

fn config_with_profile(name: &str, save_browser_profile: bool) -> CrawlConfig {
    allow_private_network();
    CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Chromiumoxide,
            mode: BrowserMode::Always,
            timeout: Duration::from_secs(20),
            ..BrowserConfig::default()
        },
        browser_profile: Some(name.to_owned()),
        save_browser_profile,
        ..CrawlConfig::default()
    }
}

/// Minimal raw HTTP server returning one fixed page, mirroring the
/// `TestServer` pattern in `test_browser_pool_lifecycle.rs`.
struct TestServer {
    base_url: String,
}

impl TestServer {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("test server should bind");
        let addr = listener.local_addr().expect("test server should have local addr");

        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(async move {
                    let mut buffer = [0_u8; 1024];
                    let _ = stream.read(&mut buffer).await.unwrap_or(0);
                    let body = "<html><body>profile-wiring-marker</body></html>";
                    let response = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                    let _ = stream.shutdown().await;
                });
            }
        });

        Self {
            base_url: format!("http://{addr}"),
        }
    }
}

/// Deletes the backing profile directory on drop, regardless of test outcome.
struct ProfileGuard(BrowserProfile);

impl Drop for ProfileGuard {
    fn drop(&mut self) {
        let _ = self.0.delete();
    }
}

/// A missing named profile must be created by the crawl, and — with
/// `save_browser_profile: true` — Chrome must be launched directly against
/// that profile's own directory (not a scratch copy), so it ends up
/// containing Chrome-written state (a `Default` profile subdirectory at
/// minimum) rather than staying empty.
#[tokio::test]
async fn missing_profile_is_created_and_populated_when_saved() {
    let name = unique_profile_name("create-save");
    let profile = BrowserProfile::new(&name).expect("profile name must be valid");
    assert!(!profile.exists(), "precondition: profile must not already exist");
    let _guard = ProfileGuard(profile.clone());

    let server = TestServer::start().await;
    let engine = create_engine(Some(config_with_profile(&name, true))).expect("engine must build");
    let result = scrape(&engine, &format!("{}/", server.base_url)).await;
    assert!(result.is_ok(), "scrape must succeed: {:?}", result.err());
    assert!(result.unwrap().html.contains("profile-wiring-marker"));

    assert!(
        profile.exists(),
        "browser_profile must be created on disk by the crawl when missing"
    );
    let entries: Vec<_> = std::fs::read_dir(&profile.user_data_dir)
        .expect("profile dir must be readable")
        .filter_map(Result::ok)
        .collect();
    assert!(
        !entries.is_empty(),
        "save_browser_profile: true must launch Chrome directly against the profile dir, \
         so Chrome-written state (e.g. a `Default` subdirectory) must be present afterwards"
    );
}

/// With `save_browser_profile: false`, the crawl must still be able to use an
/// existing named profile (starting from its current state) but must not
/// write any of that session's changes back into it — the profile directory
/// must come out of the crawl byte-for-byte identical to what it held going
/// in.
#[tokio::test]
async fn unsaved_profile_changes_are_not_written_back() {
    let name = unique_profile_name("no-save");
    let profile = BrowserProfile::new(&name).expect("profile name must be valid");
    profile.create().expect("profile dir must be creatable");
    let _guard = ProfileGuard(profile.clone());

    let marker_path = profile.user_data_dir.join("pre-existing-marker.txt");
    std::fs::write(&marker_path, b"pre-existing-state").expect("marker file must be writable");
    let before: Vec<_> = std::fs::read_dir(&profile.user_data_dir)
        .expect("profile dir must be readable")
        .filter_map(|e| e.ok().map(|e| e.file_name()))
        .collect();

    let server = TestServer::start().await;
    let engine = create_engine(Some(config_with_profile(&name, false))).expect("engine must build");
    let result = scrape(&engine, &format!("{}/", server.base_url)).await;
    assert!(result.is_ok(), "scrape must succeed: {:?}", result.err());
    assert!(result.unwrap().html.contains("profile-wiring-marker"));

    let after: Vec<_> = std::fs::read_dir(&profile.user_data_dir)
        .expect("profile dir must still be readable")
        .filter_map(|e| e.ok().map(|e| e.file_name()))
        .collect();
    assert_eq!(
        before, after,
        "save_browser_profile: false must leave the named profile directory untouched by the session"
    );
    assert_eq!(
        std::fs::read(&marker_path).expect("marker file must still exist"),
        b"pre-existing-state",
        "pre-existing profile content must be unmodified"
    );
}
