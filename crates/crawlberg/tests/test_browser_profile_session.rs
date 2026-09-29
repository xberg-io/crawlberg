//! Chrome-backed tests that sessions on one `browser_profile` never overlap while a Chrome writes
//! the profile: a session on a saved profile holds it until its Chrome has been reaped, and a
//! session on an unsaved profile copies it only while no Chrome writes it.

#![cfg(feature = "browser")]

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crawlberg::{BrowserBackend, BrowserConfig, BrowserMode, BrowserProfile, CrawlConfig, create_engine, scrape};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

mod common;
use common::announce_chrome_skip;

static NAME_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_profile_name(tag: &str) -> String {
    format!(
        "crawlberg-test-session-{tag}-{}-{}",
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
        ..CrawlConfig::builder().allow_private_networks(true).build()
    }
}

/// Deletes the backing profile directory on drop, regardless of test outcome.
struct ProfileGuard(BrowserProfile);

impl Drop for ProfileGuard {
    fn drop(&mut self) {
        let _ = self.0.delete();
    }
}

/// Serves a page that sets a cookie on every request, so each saved session writes the profile.
async fn start_cookie_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("test server should bind");
    let addr = listener.local_addr().expect("test server should have local addr");
    tokio::spawn(async move {
        let mut served = 0_u64;
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            served += 1;
            tokio::spawn(async move {
                let mut buffer = [0_u8; 4096];
                let _ = stream.read(&mut buffer).await.unwrap_or(0);
                let body = "<html><body>profile-session-marker</body></html>";
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\nset-cookie: n{served}=1; Path=/; Max-Age=3600\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    format!("http://{addr}/")
}

/// The Chrome these tests run, or `None`, announced, when none is installed.
///
/// ~keep A launch failure is no reason to skip here: a Chrome that finds another Chrome on its
/// ~keep profile exits before its websocket is up, the same launch failure a missing Chrome gives.
fn chrome_or_skip(test_name: &str) -> Option<std::path::PathBuf> {
    match chromiumoxide::detection::default_executable(Default::default()) {
        Ok(path) => Some(path),
        Err(message) => {
            announce_chrome_skip(test_name, &message);
            None
        }
    }
}

/// Scrape `url` with `config`; a panic naming `what` on any failure.
async fn scrape_ok(test_name: &str, what: &str, config: CrawlConfig, url: &str) -> String {
    let engine = create_engine(Some(config)).expect("engine must build");
    match scrape(&engine, url).await {
        Ok(result) => result.html,
        Err(error) => panic!("{test_name}: {what} must succeed: {error:?}"),
    }
}

/// A scrape that does not save the profile, started as soon as a saved scrape on the same profile
/// returns, copies the profile the saved scrape's Chrome wrote.
#[tokio::test]
async fn an_unsaved_scrape_right_after_a_saved_one_copies_the_profile() {
    let test_name = "an_unsaved_scrape_right_after_a_saved_one_copies_the_profile";
    if chrome_or_skip(test_name).is_none() {
        return;
    }
    let profile = BrowserProfile::new(&unique_profile_name("back-to-back")).expect("profile name must be valid");
    let _guard = ProfileGuard(profile.clone());
    let url = start_cookie_server().await;
    for round in 1..=3 {
        for (save, what) in [(true, "the saved scrape"), (false, "the unsaved scrape right after it")] {
            let what = format!("{what} (round {round})");
            scrape_ok(test_name, &what, config_with_profile(&profile.name, save), &url).await;
        }
    }
}

/// Two saved scrapes and an unsaved one, started together on one profile, all succeed: no copy runs
/// while a Chrome writes the profile, and no two Chromes write it at once.
#[tokio::test]
async fn scrapes_started_together_on_one_profile_all_succeed() {
    let test_name = "scrapes_started_together_on_one_profile_all_succeed";
    if chrome_or_skip(test_name).is_none() {
        return;
    }
    let profile = BrowserProfile::new(&unique_profile_name("together")).expect("profile name must be valid");
    let _guard = ProfileGuard(profile.clone());
    let url = start_cookie_server().await;
    for round in 1..=2 {
        let first_name = format!("the first saved scrape (round {round})");
        let second_name = format!("the second saved scrape (round {round})");
        let unsaved_name = format!("the unsaved scrape (round {round})");
        tokio::join!(
            scrape_ok(test_name, &first_name, config_with_profile(&profile.name, true), &url),
            scrape_ok(test_name, &second_name, config_with_profile(&profile.name, true), &url),
            scrape_ok(
                test_name,
                &unsaved_name,
                config_with_profile(&profile.name, false),
                &url
            ),
        );
    }
}

/// Write an executable `/bin/sh` script at `path`.
#[cfg(unix)]
fn write_script(path: &std::path::Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;

    std::fs::write(path, format!("#!/bin/sh\n{body}")).expect("script must be writable");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("script must be chmod-able");
}

/// A saved session whose Chrome keeps writing the profile after it is closed holds the profile
/// until teardown has killed and reaped that process: the next session on the profile, an
/// unsaved one, copies the profile and launches only once the process is gone.
///
/// ~keep The saved session's wrapper runs the real Chrome, then replaces itself with a loop that
/// ~keep writes into the profile, so the process crawlberg launched outlives `Browser.close` under
/// ~keep the same pid. The unsaved session's wrapper records, before it starts Chrome and so after
/// ~keep the copy, whether that pid still exists; a killed but unreaped process still does.
#[cfg(unix)]
#[tokio::test]
async fn the_next_session_on_a_profile_waits_for_a_stuck_chrome_to_be_reaped() {
    let test_name = "the_next_session_on_a_profile_waits_for_a_stuck_chrome_to_be_reaped";
    let Some(real_chrome) = chrome_or_skip(test_name) else {
        return;
    };
    let dir = std::env::temp_dir().join(format!("crawlberg-stuck-chrome-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir must be creatable");
    let pid_file = dir.join("stuck-pid");
    let probe_file = dir.join("stuck-at-next-launch");

    let profile = BrowserProfile::new(&unique_profile_name("stuck")).expect("profile name must be valid");
    let _guard = ProfileGuard(profile.clone());
    let url = start_cookie_server().await;

    let stuck_chrome = dir.join("stuck-chrome.sh");
    write_script(
        &stuck_chrome,
        &format!(
            "echo $$ > '{pid}'\n'{chrome}' \"$@\"\nexec sh -c 'while :; do date > \"$0/stuck-chrome-write\"; sleep 0.1; done' '{profile}'\n",
            pid = pid_file.display(),
            chrome = real_chrome.display(),
            profile = profile.user_data_dir.display(),
        ),
    );
    let next_chrome = dir.join("next-chrome.sh");
    write_script(
        &next_chrome,
        &format!(
            "if kill -0 \"$(cat '{pid}')\" 2>/dev/null; then echo alive; else echo gone; fi > '{probe}'\nexec '{chrome}' \"$@\"\n",
            pid = pid_file.display(),
            probe = probe_file.display(),
            chrome = real_chrome.display(),
        ),
    );

    let mut stuck = config_with_profile(&profile.name, true);
    stuck.browser.chrome_path = Some(stuck_chrome);
    // ~keep Long enough that a session which does not wait launches while the process still runs.
    stuck.browser.shutdown_timeout = Duration::from_secs(10);
    scrape_ok(test_name, "the saved scrape with the stuck Chrome", stuck, &url).await;
    let mut next = config_with_profile(&profile.name, false);
    next.browser.chrome_path = Some(next_chrome);
    scrape_ok(test_name, "the unsaved scrape after the stuck one", next, &url).await;

    let pid = std::fs::read_to_string(&pid_file).unwrap_or_default().trim().to_owned();
    let at_next_launch = std::fs::read_to_string(&probe_file)
        .unwrap_or_default()
        .trim()
        .to_owned();
    if at_next_launch == "alive" {
        let _ = std::process::Command::new("kill").args(["-9", &pid]).status();
    }
    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        !pid.is_empty(),
        "{test_name}: the stuck Chrome must have recorded its pid"
    );
    assert_eq!(
        at_next_launch, "gone",
        "{test_name}: the stuck Chrome (pid {pid}) must be killed and reaped before the next session on its profile copies it"
    );
}
