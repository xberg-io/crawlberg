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

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crawlberg::{
    BrowserBackend, BrowserConfig, BrowserMode, BrowserProfile, CrawlConfig, CrawlError, create_engine, scrape,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

mod common;
use common::{announce_chrome_skip, is_missing_chrome_message};

static NAME_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Builds a `CrawlConfig` whose SSRF policy permits private networks, so the loopback
/// test server is reachable.
///
// ~keep Uses the `allow_private_networks` config seam rather than the
// `CRAWLBERG_ALLOW_PRIVATE_NETWORK` env var: writing that variable is a process-global mutation
// that races every concurrent `std::env::var` read (`SsrfPolicy::from_env`, reached from
// `CrawlConfig::default()`) in this binary's other tests, aborting the process on glibc
// with no failing test name.
fn allow_private_config() -> CrawlConfig {
    CrawlConfig::builder().allow_private_networks(true).build()
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
    CrawlConfig {
        browser: BrowserConfig {
            backend: BrowserBackend::Chromiumoxide,
            mode: BrowserMode::Always,
            timeout: Duration::from_secs(20),
            ..BrowserConfig::default()
        },
        browser_profile: Some(name.to_owned()),
        save_browser_profile,
        ..allow_private_config()
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
    if let Err(CrawlError::BrowserError { message, .. }) = &result
        && is_missing_chrome_message(message)
    {
        announce_chrome_skip("missing_profile_is_created_and_populated_when_saved", message);
        return;
    }
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
    if let Err(CrawlError::BrowserError { message, .. }) = &result
        && is_missing_chrome_message(message)
    {
        announce_chrome_skip("unsaved_profile_changes_are_not_written_back", message);
        return;
    }
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

/// A raw HTTP server that records the `Cookie` header each `GET /` carried. By default its n-th
/// `GET /` sets the cookie `seen=<n>` on the marker page; given `pages`, the n-th answers with the
/// n-th `(header line, body)`, the last one for every request after.
struct CookieServer {
    base_url: String,
    cookies_seen: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl CookieServer {
    async fn start() -> Self {
        Self::serving(Vec::new()).await
    }

    async fn serving(pages: Vec<(String, String)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("test server should bind");
        let addr = listener.local_addr().expect("test server should have local addr");
        let cookies_seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = std::sync::Arc::clone(&cookies_seen);
        let pages = std::sync::Arc::new(pages);
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let seen = std::sync::Arc::clone(&seen);
                let pages = std::sync::Arc::clone(&pages);
                tokio::spawn(async move {
                    let mut buffer = [0_u8; 4096];
                    let n = stream.read(&mut buffer).await.unwrap_or(0);
                    let head = String::from_utf8_lossy(&buffer[..n]).to_string();
                    let mut header = String::new();
                    let mut body = "<html><body>profile-wiring-marker</body></html>".to_owned();
                    if head.starts_with("GET / ") {
                        let cookie = head
                            .lines()
                            .find(|line| line.to_ascii_lowercase().starts_with("cookie:"))
                            .map(|line| line[7..].trim().to_owned())
                            .unwrap_or_default();
                        let mut seen = seen.lock().expect("lock");
                        seen.push(cookie);
                        match pages.get((seen.len() - 1).min(pages.len().saturating_sub(1))) {
                            Some((page_header, page_body)) => {
                                header = page_header.clone();
                                body = page_body.clone();
                            }
                            None => header = format!("set-cookie: seen={}; Path=/; Max-Age=3600\r\n", seen.len()),
                        }
                    }
                    let response = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\n{header}content-length: {}\r\nconnection: close\r\n\r\n{}",
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
            cookies_seen,
        }
    }

    fn cookies_seen(&self) -> Vec<String> {
        self.cookies_seen.lock().expect("lock").clone()
    }
}

/// The newest modification time of a file under `dir`, if there is one.
fn newest_mtime(dir: &std::path::Path) -> Option<std::time::SystemTime> {
    let mut newest = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let candidate = if metadata.is_dir() {
            newest_mtime(&entry.path())
        } else {
            metadata.modified().ok()
        };
        if let Some(candidate) = candidate
            && newest.is_none_or(|newest| candidate > newest)
        {
            newest = Some(candidate);
        }
    }
    newest
}

/// Scrape `url` with the profile and return the page, then wait until its Chrome has left the
/// profile: its `SingletonLock` is gone and nothing under the profile changed for half a second.
/// `None` without Chrome.
///
/// ~keep The one-shot session returns before its Chrome has exited. A second session on the same
/// ~keep profile before that races the exit: with `save_browser_profile: false` the copy of the
/// ~keep profile fails on a file Chrome renames meanwhile ("failed to copy profile file
/// ~keep .../Default/.com.google.Chrome.TransportSecurity.hp1rB0: No such file or directory",
/// ~keep 3 of 3 runs; Chrome removes the lock before its last profile writes). That is the
/// ~keep session's defect, reported separately; these tests wait so they measure cookies.
async fn scrape_with_profile(
    test_name: &str,
    profile: &BrowserProfile,
    save_browser_profile: bool,
    url: &str,
) -> Option<String> {
    let engine =
        create_engine(Some(config_with_profile(&profile.name, save_browser_profile))).expect("engine must build");
    match scrape(&engine, url).await {
        Ok(result) => {
            let lock = profile.user_data_dir.join("SingletonLock");
            let quiet = Duration::from_millis(500);
            let mut left = false;
            for _ in 0..100 {
                left = std::fs::symlink_metadata(&lock).is_err()
                    && newest_mtime(&profile.user_data_dir)
                        .is_none_or(|newest| newest.elapsed().is_ok_and(|since| since > quiet));
                if left {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            assert!(
                left,
                "{test_name}: the session's Chrome must leave the profile within ten seconds"
            );
            Some(result.html)
        }
        Err(CrawlError::BrowserError { message, .. }) if is_missing_chrome_message(&message) => {
            announce_chrome_skip(test_name, &message);
            None
        }
        Err(error) => panic!("{test_name}: scrape must succeed: {error:?}"),
    }
}

/// A cookie a page sets under a saved profile reaches the next scrape with that profile: the
/// page runs in the browser's own context, whose storage is the profile's.
#[tokio::test]
async fn a_saved_profile_keeps_the_cookies_a_page_set() {
    let test_name = "a_saved_profile_keeps_the_cookies_a_page_set";
    let name = unique_profile_name("cookies-saved");
    let profile = BrowserProfile::new(&name).expect("profile name must be valid");
    let _guard = ProfileGuard(profile.clone());
    let server = CookieServer::start().await;
    let url = format!("{}/", server.base_url);
    for _ in 0..2 {
        if scrape_with_profile(test_name, &profile, true, &url).await.is_none() {
            return;
        }
    }
    let seen = server.cookies_seen();
    assert_eq!(
        seen.len(),
        2,
        "{test_name}: both scrapes must reach the server: {seen:?}"
    );
    assert_eq!(
        seen[0], "",
        "{test_name}: the first scrape of a new profile must carry no cookie"
    );
    assert!(
        seen[1].contains("seen=1"),
        "{test_name}: the cookie the first scrape set must reach the second, got {:?}",
        seen[1]
    );
}

/// A scrape that does not save its profile still starts with the profile's cookies, and the
/// cookies it sets are gone from the profile afterwards.
#[tokio::test]
async fn an_unsaved_profile_offers_its_cookies_and_keeps_none() {
    let test_name = "an_unsaved_profile_offers_its_cookies_and_keeps_none";
    let name = unique_profile_name("cookies-unsaved");
    let profile = BrowserProfile::new(&name).expect("profile name must be valid");
    let _guard = ProfileGuard(profile.clone());
    let server = CookieServer::start().await;
    let url = format!("{}/", server.base_url);
    for save in [true, false, true] {
        if scrape_with_profile(test_name, &profile, save, &url).await.is_none() {
            return;
        }
    }
    let seen = server.cookies_seen();
    assert_eq!(
        seen.len(),
        3,
        "{test_name}: all three scrapes must reach the server: {seen:?}"
    );
    assert!(
        seen[1].contains("seen=1"),
        "{test_name}: the unsaved scrape must start with the profile's cookie, got {:?}",
        seen[1]
    );
    assert!(
        seen[2].contains("seen=1") && !seen[2].contains("seen=2"),
        "{test_name}: the profile must keep the saved scrape's cookie and none of the unsaved one's, got {:?}",
        seen[2]
    );
}

/// The localStorage a page writes under a saved profile reaches the next scrape with that
/// profile: the page runs in the profile's own storage, not in a context that starts empty.
#[tokio::test]
async fn a_saved_profile_keeps_the_local_storage_a_page_wrote() {
    let test_name = "a_saved_profile_keeps_the_local_storage_a_page_wrote";
    let name = unique_profile_name("storage-saved");
    let profile = BrowserProfile::new(&name).expect("profile name must be valid");
    let _guard = ProfileGuard(profile.clone());
    let page = "<html><body><p id=o>x</p><script>const v = localStorage.getItem('k') || 'none'; \
                document.getElementById('o').textContent = 'prior=' + v; localStorage.setItem('k', 'v1');</script></body></html>";
    let server = CookieServer::serving(vec![(String::new(), page.to_owned())]).await;
    let url = format!("{}/", server.base_url);
    let Some(first) = scrape_with_profile(test_name, &profile, true, &url).await else {
        return;
    };
    let Some(second) = scrape_with_profile(test_name, &profile, true, &url).await else {
        return;
    };
    assert!(
        first.contains("prior=none"),
        "{test_name}: the first scrape of a new profile must find no localStorage: {first}"
    );
    assert!(
        second.contains("prior=v1"),
        "{test_name}: the second scrape must find the localStorage the first wrote: {second}"
    );
}

/// A cookie a page deletes under a saved profile is gone from the profile: the third scrape
/// sends nothing after the second one expired the cookie the first one set.
#[tokio::test]
async fn a_saved_profile_forgets_the_cookie_a_page_deleted() {
    let test_name = "a_saved_profile_forgets_the_cookie_a_page_deleted";
    let name = unique_profile_name("cookie-deleted");
    let profile = BrowserProfile::new(&name).expect("profile name must be valid");
    let _guard = ProfileGuard(profile.clone());
    let body = "<html><body>profile-wiring-marker</body></html>".to_owned();
    let server = CookieServer::serving(vec![
        ("set-cookie: a=1; Path=/; Max-Age=3600\r\n".to_owned(), body.clone()),
        ("set-cookie: a=gone; Path=/; Max-Age=0\r\n".to_owned(), body.clone()),
        (String::new(), body),
    ])
    .await;
    let url = format!("{}/", server.base_url);
    for _ in 0..3 {
        if scrape_with_profile(test_name, &profile, true, &url).await.is_none() {
            return;
        }
    }
    let seen = server.cookies_seen();
    assert_eq!(
        seen.len(),
        3,
        "{test_name}: all three scrapes must reach the server: {seen:?}"
    );
    assert!(
        seen[1].contains("a=1"),
        "{test_name}: the second scrape must carry the cookie the first set: {seen:?}"
    );
    assert!(
        !seen[2].contains("a="),
        "{test_name}: the cookie the second scrape deleted must be gone from the saved profile: {seen:?}"
    );
}
