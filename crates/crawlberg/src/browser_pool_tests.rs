//! Unit tests for [`super`]'s browser pool, session reuse and teardown.
//!
//! ~keep In its own file because `browser_pool.rs` crossed poly's 1000-line limit when the
//! ~keep #146 close-outcome work landed, and `alef.toml` exempts `**/*_tests.rs` from the
//! ~keep quality metrics -- the same split `engine/wasm_crawl_tests.rs` already uses. Nothing
//! ~keep but test code belongs here: a production helper moved in would become lint-exempt
//! ~keep by accident.

use super::*;

#[test]
fn test_config_defaults() {
    let config = BrowserPoolConfig::default();
    assert_eq!(config.max_pages, 8);
    assert_eq!(config.launch_timeout, Duration::from_secs(30));
    assert!(config.browser_endpoint.is_none());
    assert!(config.chrome_args.is_empty());
}

#[test]
fn test_pool_creation() {
    let pool = BrowserPool::new(BrowserPoolConfig::default());
    assert!(!pool.shutdown.load(Ordering::Relaxed));
}

#[tokio::test]
async fn test_shutdown_idempotent() {
    let pool = BrowserPool::new(BrowserPoolConfig::default());
    pool.shutdown().await;
    pool.shutdown().await;
}

#[tokio::test]
async fn test_acquire_after_shutdown_fails() {
    let pool = BrowserPool::new(BrowserPoolConfig::default());
    pool.shutdown().await;
    let result = pool.acquire_page().await;
    assert!(result.is_err());
}

thread_local! {
    /// How many [`ScratchProfileDir`]s dropped on this thread, so a test can tell a site dropped one.
    pub(crate) static PROFILE_HAND_OFFS_HERE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// How many profile teardowns ran on this thread. An executor thread must see none.
    pub(crate) static PROFILE_TEARDOWNS_HERE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// This thread's counts of dropped profile directories and of profile teardowns run on it.
pub(crate) fn profile_drops_here() -> (usize, usize) {
    (
        PROFILE_HAND_OFFS_HERE.with(std::cell::Cell::get),
        PROFILE_TEARDOWNS_HERE.with(std::cell::Cell::get),
    )
}

/// Assert that this thread dropped a profile directory since `before` and ran none of its teardown.
pub(crate) fn assert_profile_teardown_left_this_thread(before: (usize, usize)) {
    let (hand_offs, teardowns) = profile_drops_here();
    assert!(hand_offs > before.0, "the site must drop a profile directory");
    assert_eq!(
        teardowns, before.1,
        "the profile teardown must run on another thread, not the executor thread that dropped it"
    );
}

/// Wait up to ten seconds for the teardown another thread runs to remove `path`.
pub(crate) fn wait_for_removal(path: &std::path::Path) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while path.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    !path.exists()
}

/// Assert that the profile directory at `path` is removed, that no process uses it, and that it is
/// still gone a second later, when a helper that outlived its browser would have written again.
pub(crate) fn assert_profile_directory_is_gone_for_good(path: &std::path::Path) {
    assert!(
        wait_for_removal(path),
        "the profile directory must be removed: {}",
        path.display()
    );
    let mut system = sysinfo::System::new();
    let users: Vec<_> = processes_naming(&mut system, &user_data_dir_flag(path))
        .iter()
        .map(|process| process.pid())
        .collect();
    assert!(users.is_empty(), "processes {users:?} still use {}", path.display());
    std::thread::sleep(Duration::from_secs(1));
    assert!(
        !path.exists(),
        "the profile directory must stay removed: {}",
        path.display()
    );
}

/// Stopping a profile's users kills a process of the named Chrome still writing into it.
///
/// ~keep The stand-in carries `--user-data-dir=<dir>` on its command line, as each of Chrome's
/// ~keep helper processes does, and rewrites a file in the directory in a loop, as the helpers do
/// ~keep for a moment after the browser process dies. Its own executable stands in for Chrome's.
/// ~keep It is a shell builtin loop, so no child of it without the flag can write into the
/// ~keep directory after the kill. It is a child of this process that nothing reaps before the stop
/// ~keep returns, as the browser process can be, so the stop must finish well inside its deadline
/// ~keep instead of waiting on the zombie.
#[cfg(unix)]
#[test]
fn stopping_a_profiles_users_kills_the_process_writing_into_it_and_skips_its_zombie() {
    let dir = ScratchProfileDir::create("crawlberg-profile-users-test-").expect("the directory must be creatable");
    let path = dir.path().to_path_buf();
    let mut helper = std::process::Command::new("sh")
        .arg("-c")
        .arg(r#"d="${0#--user-data-dir=}"; while :; do : > "$d/state"; done"#)
        .arg(user_data_dir_flag(&path))
        .spawn()
        .expect("sh must start");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !path.join("state").exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        path.join("state").exists(),
        "the stand-in must write into the directory"
    );
    let chrome = chrome_of(helper.id()).expect("the stand-in's executable must be readable");

    let started = std::time::Instant::now();
    stop_chrome_processes_using(&path, &chrome);
    let elapsed = started.elapsed();

    let exited = helper.try_wait().expect("the stand-in's status must be readable");
    if exited.is_none() {
        let _ = helper.kill();
        let _ = helper.wait();
    }
    assert!(exited.is_some(), "stopping the profile's users must kill the stand-in");
    assert!(
        elapsed < PROFILE_USERS_EXIT_TIMEOUT / 2,
        "the stop must not wait on a killed child nobody has reaped yet: took {elapsed:?}"
    );
    drop(dir);
    assert_profile_directory_is_gone_for_good(&path);
}

/// A profile directory dropped outside a Tokio runtime is torn down on another thread, so a host's
/// finalizer thread that drops the last owner is not held for up to the five-second wait.
#[test]
fn a_profile_directory_dropped_outside_a_runtime_is_torn_down_on_another_thread() {
    let dir = ScratchProfileDir::create("crawlberg-no-runtime-test-").expect("the directory must be creatable");
    let path = dir.path().to_path_buf();
    let before = profile_drops_here();

    drop(dir);

    assert_profile_teardown_left_this_thread(before);
    assert!(wait_for_removal(&path), "the directory must be removed");
}

/// The wait for killed processes to exit gives up once its timeout passes.
///
/// ~keep A user this process cannot kill, such as another account's process naming the flag, would
/// ~keep otherwise hold the teardown forever.
#[test]
fn stopping_gives_up_on_a_user_that_never_exits_once_its_timeout_passes() {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(kill_until_gone(Duration::from_millis(100), || true));
    });
    let stopped = receiver
        .recv_timeout(Duration::from_secs(10))
        .expect("the wait must end once its timeout passes");
    assert!(!stopped, "a user that never exits must be reported as still running");
}

/// The flag counts only as a whole token of the command line, bounded by a space or an end.
#[test]
fn a_command_line_names_the_flag_only_as_a_whole_token() {
    let flag = "--user-data-dir=/tmp/crawlberg-chrome-a1";
    let names = |arguments: &[&str]| {
        let cmd: Vec<std::ffi::OsString> = arguments.iter().map(std::ffi::OsString::from).collect();
        command_line_names(&cmd, flag)
    };
    assert!(names(&[flag]), "the flag alone");
    assert!(names(&["/opt/chrome", flag, "--headless"]), "the flag as one argument");
    assert!(
        names(&["/opt/chrome --type=renderer --user-data-dir=/tmp/crawlberg-chrome-a1 --lang=en"]),
        "the flag inside Chrome's space-joined process title"
    );
    for other in [
        format!("{flag}-other"),
        format!("{flag}/Default"),
        format!("x{flag}"),
        format!("--no{flag}"),
        "/tmp/crawlberg-chrome-a1".to_owned(),
    ] {
        assert!(!names(&["/opt/chrome", &other]), "{other:?} must not name the flag");
    }
}

/// Start `sh` blocked on its stdin with `argument` on its command line, and wait until a scan
/// of the process table sees it there.
#[cfg(unix)]
pub(crate) fn spawn_bystander(argument: &str) -> std::process::Child {
    let child = std::process::Command::new("sh")
        .arg("-c")
        .arg("read _")
        .arg(argument)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("sh must start");
    let mut system = sysinfo::System::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while processes_naming(&mut system, argument).is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !processes_naming(&mut system, argument).is_empty(),
        "the bystander must show {argument:?} on its command line"
    );
    child
}

/// Dropping a profile directory leaves running a process that is not Chrome, even one that
/// carries the exact flag as an argument of its own, as a shell, `strace` or `grep` can.
///
/// ~keep A `sleep` stands in for the Chrome launched on the directory. It lies in the same
/// ~keep directory as the bystander's `sh`, as a launcher such as `/usr/bin/snap` lies beside
/// ~keep shells, so only the executable itself tells the two apart.
#[cfg(unix)]
#[test]
fn dropping_a_profile_directory_leaves_a_process_that_is_not_chrome_running() {
    let mut dir =
        ScratchProfileDir::create("crawlberg-profile-bystander-test-").expect("the directory must be creatable");
    let mut chrome = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("sleep must start");
    dir.record_chrome(chrome.id());
    let path = dir.path().to_path_buf();
    let mut bystander = spawn_bystander(&user_data_dir_flag(&path));

    drop(dir);
    let removed = wait_for_removal(&path);

    let running = bystander.try_wait().expect("the status must be readable").is_none();
    let _ = bystander.kill();
    let _ = bystander.wait();
    let _ = chrome.kill();
    let _ = chrome.wait();
    assert!(removed, "the directory must be removed");
    assert!(running, "a process that is not Chrome must not be killed");
}

/// Removing the profile directory of a Chrome that is still running stops that Chrome first.
///
/// ~keep A real Chrome, because Chrome rewrites the command line of each of its processes into
/// ~keep one space-joined string, which a stand-in started with separate arguments does not do, and
/// ~keep because its helpers must run the executable the launch reads from the browser process. The
/// ~keep browser's exit is read from its own handle, not from the process scan under test.
#[tokio::test(flavor = "multi_thread")]
#[allow(clippy::print_stderr, reason = "test-only skip announcement")]
async fn removing_a_running_chromes_profile_directory_stops_that_chrome() {
    let dir = ScratchProfileDir::create("crawlberg-running-chrome-test-").expect("the directory must be creatable");
    let path = dir.path().to_path_buf();
    let config = match build_pool_launch_builder(&path, &[]).build() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("skipping: no usable Chrome: {error}");
            return;
        }
    };
    let (mut browser, mut handler, dir) = match dir.launch(config).await {
        Ok(launched) => launched,
        Err(error) => {
            eprintln!("skipping: no usable Chrome: {error}");
            return;
        }
    };
    let handler_handle = tokio::spawn(async move { while handler.next().await.is_some() {} });

    drop(dir);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut exited = browser.try_wait().expect("the browser's status must be readable");
    while exited.is_none() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
        exited = browser.try_wait().expect("the browser's status must be readable");
    }
    if exited.is_none() {
        let _ = browser.kill().await;
    }
    handler_handle.abort();
    assert!(
        exited.is_some(),
        "removing the profile directory must stop the Chrome still using it"
    );
    tokio::task::spawn_blocking(move || assert_profile_directory_is_gone_for_good(&path))
        .await
        .expect("no Chrome may use or recreate the profile directory after it is removed");
}

/// A pool dropped without `shutdown` removes the profile directory of the Chrome it launched,
/// off the thread that dropped it.
///
/// ~keep Tests and embedders drop pools without shutting them down, and each such drop left one
/// ~keep `crawlberg-chrome-*` directory in the temp directory (xberg-io/crawlberg#415).
#[tokio::test]
#[allow(clippy::print_stderr, reason = "test-only skip announcement")]
async fn a_pool_dropped_without_shutdown_leaves_no_profile_directory() {
    let pool = BrowserPool::new(BrowserPoolConfig::default());
    if let Err(error) = pool.warm().await {
        eprintln!("skipping a_pool_dropped_without_shutdown_leaves_no_profile_directory: no usable Chrome: {error}");
        return;
    }
    let path = pool_profile_dir(&pool).await;

    let before = profile_drops_here();
    drop(pool);
    assert_profile_teardown_left_this_thread(before);

    tokio::task::spawn_blocking(move || assert_profile_directory_is_gone_for_good(&path))
        .await
        .expect("a pool dropped without shutdown must stop its Chrome and remove its profile directory");
}

/// The profile directory of the Chrome `pool` runs, which must exist.
async fn pool_profile_dir(pool: &BrowserPool) -> std::path::PathBuf {
    let path = pool
        .state
        .lock()
        .await
        .as_ref()
        .and_then(|state| state.user_data_dir.as_ref())
        .map(|dir| dir.path().to_path_buf())
        .expect("a launched pool must own a profile directory");
    assert!(path.is_dir(), "the profile directory must exist while Chrome runs");
    path
}

/// A pool shut down stops its Chrome and removes its profile directory, off the executor thread.
#[tokio::test]
#[allow(clippy::print_stderr, reason = "test-only skip announcement")]
async fn a_pool_shut_down_leaves_no_profile_directory() {
    let pool = BrowserPool::new(BrowserPoolConfig::default());
    if let Err(error) = pool.warm().await {
        eprintln!("skipping a_pool_shut_down_leaves_no_profile_directory: no usable Chrome: {error}");
        return;
    }
    let path = pool_profile_dir(&pool).await;

    let before = profile_drops_here();
    pool.shutdown().await;
    assert_profile_teardown_left_this_thread(before);

    tokio::task::spawn_blocking(move || assert_profile_directory_is_gone_for_good(&path))
        .await
        .expect("a pool shut down must stop its Chrome and remove its profile directory");
}

/// A pool that relaunches a Chrome whose handler ended removes the old Chrome's profile directory.
#[tokio::test]
#[allow(clippy::print_stderr, reason = "test-only skip announcement")]
async fn a_relaunched_pool_removes_the_old_chromes_profile_directory() {
    let pool = BrowserPool::new(BrowserPoolConfig::default());
    if let Err(error) = pool.warm().await {
        eprintln!("skipping a_relaunched_pool_removes_the_old_chromes_profile_directory: no usable Chrome: {error}");
        return;
    }
    let old = pool_profile_dir(&pool).await;
    // ~keep A relaunch replaces only a Chrome whose handler has ended.
    if let Some(state) = pool.state.lock().await.as_ref() {
        state.handler_handle.abort();
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !pool
        .state
        .lock()
        .await
        .as_ref()
        .is_some_and(|state| state.handler_handle.is_finished())
    {
        assert!(tokio::time::Instant::now() < deadline, "the aborted handler must end");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let before = profile_drops_here();
    let relaunched = pool.relaunch_browser().await;
    assert_profile_teardown_left_this_thread(before);
    let new = pool_profile_dir(&pool).await;
    pool.shutdown().await;

    assert!(relaunched.is_ok(), "the relaunch must succeed: {relaunched:?}");
    assert_ne!(old, new, "the relaunched Chrome must use a new profile directory");
    tokio::task::spawn_blocking(move || assert_profile_directory_is_gone_for_good(&old))
        .await
        .expect("a relaunch must stop the old Chrome and remove its profile directory");
}

/// A launch that fails removes its profile directory, off the executor thread.
///
/// ~keep No Chrome is needed: the executable is missing, so the launch fails before any Chrome runs.
#[tokio::test]
async fn a_failed_launch_removes_its_profile_directory() {
    let dir = ScratchProfileDir::create("crawlberg-failed-launch-test-").expect("the directory must be creatable");
    let path = dir.path().to_path_buf();
    let config = build_pool_launch_builder(&path, &[])
        .chrome_executable(path.join("no-such-chrome"))
        .build()
        .expect("a config naming its executable must build");
    let before = profile_drops_here();

    let launched = dir.launch(config).await;

    assert!(launched.is_err(), "a launch of a missing executable must fail");
    assert_profile_teardown_left_this_thread(before);
    assert!(wait_for_removal(&path), "the directory must be removed");
}

/// A pool launch that times out hands its profile teardown off the executor thread, which holds
/// the pool's state lock at that point.
///
/// ~keep No Chrome is needed: without one the launch fails before the timeout, and the profile
/// ~keep directory drops on the same path.
#[tokio::test]
async fn a_timed_out_pool_launch_tears_its_profile_down_off_the_executor_thread() {
    let pool = BrowserPool::new(BrowserPoolConfig {
        launch_timeout: Duration::from_millis(1),
        ..BrowserPoolConfig::default()
    });
    let before = profile_drops_here();
    let warmed = pool.warm().await;
    assert!(warmed.is_err(), "a Chrome launch cannot finish within a millisecond");
    assert_profile_teardown_left_this_thread(before);
}

/// `close_browser_within` must return near its configured `shutdown_timeout`, and the
/// process must actually be dead afterward, even when `Browser::close`/`wait` cannot make
/// progress -- the reported case was a Chrome process blocked behind an OS dialog
/// (see the `~keep` on `close_browser_within`'s own doc comment).
///
/// ~keep Simulates that without a real dialog: `SIGSTOP` freezes a genuinely launched
/// ~keep Chrome process so it cannot respond to the CDP `Browser.close` command or exit,
/// ~keep without killing it -- `close()`/`wait()` then hang exactly as they did against
/// ~keep the keychain-prompt report. Skipped (not failed) when this machine has no usable
/// ~keep Chrome or `kill -STOP` is unavailable (non-Unix), matching the browser
/// ~keep integration tests' skip convention. Requires a real Chrome binary; a fully mocked
/// ~keep `Browser` was not practical here (`chromiumoxide::Browser` wraps a real child
/// ~keep process and CDP connection with no test seam for either).
#[tokio::test]
#[allow(
    clippy::print_stderr,
    reason = "test-only skip announcement, matching tests/common/mod.rs's convention"
)]
async fn close_browser_within_returns_promptly_when_the_process_is_stopped() {
    if !cfg!(unix) {
        eprintln!("skipping close_browser_within_returns_promptly_when_the_process_is_stopped: not unix");
        return;
    }

    let user_data_dir = ScratchProfileDir::create("crawlberg-pool-test-").expect("a profile directory must be created");
    let browser_config = match build_pool_launch_builder(user_data_dir.path(), &[]).build() {
        Ok(config) => config,
        Err(error) => {
            eprintln!(
                "skipping close_browser_within_returns_promptly_when_the_process_is_stopped \
                 because no usable Chrome was found: {error}"
            );
            return;
        }
    };
    let (mut browser, mut handler) = match Browser::launch(browser_config).await {
        Ok(pair) => pair,
        Err(error) => {
            eprintln!(
                "skipping close_browser_within_returns_promptly_when_the_process_is_stopped \
                 because no usable Chrome was found: {error}"
            );
            return;
        }
    };
    let handler_task = tokio::spawn(async move { while handler.next().await.is_some() {} });

    let pid = browser
        .get_mut_child()
        .and_then(|child| child.as_mut_inner().id())
        .expect("a freshly launched child must have a pid");

    let stopped = std::process::Command::new("kill")
        .args(["-STOP", &pid.to_string()])
        .status()
        .expect("`kill -STOP` must run")
        .success();
    assert!(stopped, "failed to SIGSTOP the launched Chrome process (pid {pid})");

    let shutdown_timeout = Duration::from_millis(500);
    let start = std::time::Instant::now();
    let close_outcome = close_browser_within(&mut browser, shutdown_timeout).await;
    let elapsed = start.elapsed();

    // ~keep Always sent, even if the assertions below fail: a stopped process left behind
    // ~keep by a broken implementation would otherwise leak past this test.
    let _ = std::process::Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .status();
    handler_task.abort();

    assert!(
        elapsed < Duration::from_secs(5),
        "close_browser_within must return near its configured budget ({shutdown_timeout:?}) \
         even when close()/wait() cannot make progress on a stopped process; took {elapsed:?}"
    );

    let still_running = std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .expect("`kill -0` must run")
        .success();
    assert!(
        !still_running,
        "the Chrome process (pid {pid}) must be dead after close_browser_within returns, \
         via its Browser::kill() fallback"
    );
    // ~keep A guard on the reported outcome rather than a second behaviour: the timing this
    // ~keep distinction buys is asserted by
    // ~keep `release_browser_kills_a_stopped_launched_chrome_within_one_shutdown_timeout`.
    assert_eq!(
        close_outcome,
        BrowserCloseOutcome::Killed,
        "a process that could not answer close()/wait() must be reported as killed, so the \
         caller can skip waiting on a CDP handler that will never end"
    );
}

/// Releasing a launched Chrome that has stopped responding kills it within one
/// `shutdown_timeout`, and returns within one too, even when a tab is handed over for
/// closing: the tab dies with the browser, so no separate tab close may run ahead of the
/// close-and-kill.
///
/// ~keep `SIGSTOP` freezes the process so every CDP call hangs, as in
/// ~keep `close_browser_within_returns_promptly_when_the_process_is_stopped`.
/// ~keep The return-time assertion is xberg-io/crawlberg#146's regression coverage and fails
/// ~keep before its fix: the release used to wait the full `HANDLER_SHUTDOWN_TIMEOUT` for a
/// ~keep handler loop that a killed Chrome can never end, so it returned about one
/// ~keep `shutdown_timeout` plus five seconds after it started.
#[tokio::test]
#[allow(
    clippy::print_stderr,
    reason = "test-only skip announcement, matching tests/common/mod.rs's convention"
)]
async fn release_browser_kills_a_stopped_launched_chrome_within_one_shutdown_timeout() {
    const TEST_NAME: &str = "release_browser_kills_a_stopped_launched_chrome_within_one_shutdown_timeout";
    if !cfg!(unix) {
        eprintln!("skipping {TEST_NAME}: not unix");
        return;
    }
    let user_data_dir = ScratchProfileDir::create("crawlberg-pool-test-").expect("a profile directory must be created");
    let launched = match build_pool_launch_builder(user_data_dir.path(), &[]).build() {
        Ok(config) => Browser::launch(config).await.map_err(|error| error.to_string()),
        Err(error) => Err(error),
    };
    let (mut browser, mut handler) = match launched {
        Ok(pair) => pair,
        Err(error) => {
            eprintln!("skipping {TEST_NAME} because no usable Chrome was found: {error}");
            return;
        }
    };
    let handler_task = tokio::spawn(async move { while handler.next().await.is_some() {} });
    let page = browser
        .new_page("about:blank")
        .await
        .expect("a launched Chrome must open a tab");
    let tab = page.target_id().clone();
    let pid = browser
        .get_mut_child()
        .and_then(|child| child.as_mut_inner().id())
        .expect("a freshly launched child must have a pid");
    let stopped = std::process::Command::new("kill")
        .args(["-STOP", &pid.to_string()])
        .status()
        .expect("`kill -STOP` must run")
        .success();
    assert!(stopped, "failed to SIGSTOP the launched Chrome process (pid {pid})");

    // ~keep The process's death is timed on its own thread because it lands while
    // ~keep `release_browser` is still running; the release's own return is timed below.
    let alive = move || {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .is_ok_and(|status| status.success())
    };
    let start = std::time::Instant::now();
    let died_after = std::thread::spawn(move || {
        while alive() && start.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(20));
        }
        start.elapsed()
    });
    let shutdown_timeout = Duration::from_secs(1);
    let cleanup = ExternalTabCleanup {
        open_tab: Some(tab),
        ..ExternalTabCleanup::default()
    };
    release_browser(browser, handler_task, cleanup, shutdown_timeout).await;
    let released_after = start.elapsed();
    let died_after = died_after.join().expect("the watcher thread must not panic");

    let _ = std::process::Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .status();

    // ~keep The kill lands one `shutdown_timeout` after the release starts. A tab close run
    // ~keep ahead of the close-and-kill would add a second timeout before it.
    assert!(
        died_after < shutdown_timeout + Duration::from_millis(700),
        "a stopped launched Chrome must be killed within one shutdown_timeout \
         ({shutdown_timeout:?}); it died after {died_after:?}"
    );
    assert!(
        released_after < shutdown_timeout + Duration::from_secs(2),
        "the release must return once the process is reaped, not wait \
         {HANDLER_SHUTDOWN_TIMEOUT:?} for a CDP handler that a killed Chrome can never end \
         (xberg-io/crawlberg#146); it returned after {released_after:?}"
    );
}

/// Releasing a connected browser disconnects from it: the handler task that owns the CDP
/// websocket stops at once, and the Chrome at the other end keeps running.
#[tokio::test]
#[allow(
    clippy::print_stderr,
    reason = "test-only skip announcement, matching tests/common/mod.rs's convention"
)]
async fn release_browser_disconnects_from_a_connected_browser_without_closing_it() {
    let user_data_dir = ScratchProfileDir::create("crawlberg-pool-test-").expect("a profile directory must be created");
    let launched = match build_pool_launch_builder(user_data_dir.path(), &[]).build() {
        Ok(config) => Browser::launch(config).await.map_err(|error| error.to_string()),
        Err(error) => Err(error),
    };
    let (mut owner, mut owner_handler) = match launched {
        Ok(pair) => pair,
        Err(error) => {
            eprintln!(
                "skipping release_browser_disconnects_from_a_connected_browser_without_closing_it \
                 because no usable Chrome was found: {error}"
            );
            return;
        }
    };
    let owner_task = tokio::spawn(async move { while owner_handler.next().await.is_some() {} });

    let (connected, mut handler) = Browser::connect(owner.websocket_address().clone())
        .await
        .expect("connecting to the launched Chrome must succeed");
    let handler_task = tokio::spawn(async move { while handler.next().await.is_some() {} });
    let handler_abort = handler_task.abort_handle();

    release_browser(
        connected,
        handler_task,
        ExternalTabCleanup::default(),
        Duration::from_secs(5),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let disconnected = handler_abort.is_finished();
    let version = tokio::time::timeout(Duration::from_secs(5), owner.version()).await;

    let _ = owner.kill().await;
    owner_task.abort();

    assert!(
        disconnected,
        "the handler task for a connected browser must stop, closing its websocket"
    );
    assert!(
        matches!(version, Ok(Ok(_))),
        "the connected Chrome must still answer CDP after release: {version:?}"
    );
}

#[test]
fn test_safe_default_args_never_double_prefixes_for_chromiumoxide() {
    // ~keep chromiumoxide's BrowserConfig::arg renders every entry as `--{arg}`; an
    // ~keep already-`--`-prefixed entry would render as `----...` and Chrome discards
    // ~keep it as an unknown flag (see chrome_args.rs).
    for arg in safe_default_args() {
        let rendered = format!("--{arg}");
        assert!(!rendered.starts_with("----"), "double-prefixed flag: {rendered}");
    }
}

#[test]
fn test_safe_default_args_adds_use_mock_keychain_on_macos_only() {
    let args = safe_default_args();
    if cfg!(target_os = "macos") {
        assert!(
            args.contains(&"use-mock-keychain"),
            "missing --use-mock-keychain on macOS"
        );
    } else {
        assert!(
            !args.contains(&"use-mock-keychain"),
            "use-mock-keychain should be macOS-only"
        );
    }
}

#[test]
fn test_apply_default_args_produces_normalized_flags() {
    let builder = apply_default_args(BrowserConfig::builder());
    assert_launch_flags_are_normalized(&builder);
}

#[test]
fn the_pool_launch_builder_carries_no_double_dashed_flag_and_the_macos_keychain_flag() {
    // ~keep Behavioral, not textual: this calls the exact function `launch_browser`
    // ~keep uses to build its `BrowserConfig`, so a path that stops calling
    // ~keep `apply_default_args` (even by looping over a raw flag instead) fails here
    // ~keep because the returned flags actually change.
    let builder = build_pool_launch_builder(std::path::Path::new("/tmp/pool-test-profile"), &[]);
    assert_launch_flags_are_normalized(&builder);
}

#[test]
fn test_every_known_launch_path_calls_the_shared_apply_default_args_helper() {
    // ~keep Textual guard, kept alongside the behavioral tests above and in browser.rs's
    // ~keep and interact/chromiumoxide.rs's own test modules (each builds the real
    // ~keep launch config for its path and inspects the flags). Comments are stripped
    // ~keep and apply_default_args' own definition line is excluded, so a path that
    // ~keep only mentions the helper's name in a comment, or is the file that defines
    // ~keep it, does not satisfy this; only a real call site does.
    // ~keep Limitation: this can only check the three files named here. A fourth
    // ~keep launch path added in a new file is NOT caught by this test; it needs its
    // ~keep own behavioral test or a new entry in this list.
    for (path, src) in [
        ("browser/launch.rs", include_str!("browser/launch.rs")),
        ("browser_pool.rs", include_str!("browser_pool.rs")),
        ("interact/chromiumoxide.rs", include_str!("interact/chromiumoxide.rs")),
    ] {
        // ~keep Cut at the first test module's declaration, not at the first `#[cfg(test)]`:
        // ~keep production code carries `#[cfg(test)]` statements of its own (the profile
        // ~keep teardown's counters in browser_pool.rs), and cutting there hid every launch
        // ~keep path below them.
        let code_only: String = src
            .lines()
            .take_while(|line| {
                let line = line.trim_start();
                !((line.starts_with("mod ") || line.starts_with("pub(crate) mod ")) && line.contains("tests"))
            })
            .filter(|line| !line.contains("fn apply_default_args("))
            .map(|line| line.split("//").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            code_only.contains("apply_default_args("),
            "{path} does not call the shared apply_default_args helper outside a comment or its own definition"
        );
    }
}
