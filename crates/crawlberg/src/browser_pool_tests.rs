//! Unit tests for [`super`]'s browser pool, session reuse and teardown.
//!
//! ~keep In its own file because `browser_pool.rs` crossed poly's 1000-line limit when the
//! ~keep #146 close-outcome work landed, and `alef.toml` exempts `**/*_tests.rs` from the
//! ~keep quality metrics -- the same split `engine/wasm_crawl_tests.rs` already uses. Nothing
//! ~keep but test code belongs here: a production helper moved in would become lint-exempt
//! ~keep by accident.

use chromiumoxide::detection::{DetectionOptions, default_executable};
use sysinfo::UpdateKind;

use super::*;

#[test]
fn test_config_defaults() {
    let config = BrowserPoolConfig::default();
    assert_eq!(config.max_pages, 8);
    assert_eq!(config.launch_timeout, Duration::from_secs(30));
    assert!(config.browser_endpoint.is_none());
    assert!(config.chrome_path.is_none());
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

    let user_data_dir = std::env::temp_dir().join(format!("crawlberg-shutdown-timeout-test-{}", std::process::id()));
    let browser_config = match build_pool_launch_builder(&user_data_dir, &BrowserPoolConfig::default())
        .expect("the default pool config names no binary to check")
        .build()
    {
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
    let user_data_dir = std::env::temp_dir().join(format!("crawlberg-release-stopped-test-{}", std::process::id()));
    let launched = match build_pool_launch_builder(&user_data_dir, &BrowserPoolConfig::default())
        .expect("the default pool config names no binary to check")
        .build()
    {
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
    let _ = std::fs::remove_dir_all(&user_data_dir);

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
    let user_data_dir = std::env::temp_dir().join(format!("crawlberg-release-connected-test-{}", std::process::id()));
    let launched = match build_pool_launch_builder(&user_data_dir, &BrowserPoolConfig::default())
        .expect("the default pool config names no binary to check")
        .build()
    {
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
    let _ = std::fs::remove_dir_all(&user_data_dir);

    assert!(
        disconnected,
        "the handler task for a connected browser must stop, closing its websocket"
    );
    assert!(
        matches!(version, Ok(Ok(_))),
        "the connected Chrome must still answer CDP after release: {version:?}"
    );
}

/// Launch a Chrome on `user_data_dir` for a test, or announce the skip and return `None`.
async fn launch_for(test_name: &str, user_data_dir: &std::path::Path) -> Option<(Browser, JoinHandle<()>)> {
    let builder = build_pool_launch_builder(user_data_dir, &BrowserPoolConfig::default())
        .expect("the default pool config names no binary to check");
    launch_config(test_name, builder).await
}

/// Launch the Chrome `builder` describes for a test, or announce the skip and return `None`.
#[allow(
    clippy::print_stderr,
    reason = "test-only skip announcement, matching tests/common/mod.rs's convention"
)]
async fn launch_config(test_name: &str, builder: BrowserConfigBuilder) -> Option<(Browser, JoinHandle<()>)> {
    let launched = match builder.build() {
        Ok(config) => Browser::launch(config).await.map_err(|error| error.to_string()),
        Err(error) => Err(error),
    };
    match launched {
        Ok((browser, mut handler)) => {
            let task = tokio::spawn(async move { while handler.next().await.is_some() {} });
            Some((browser, task))
        }
        Err(error) => {
            eprintln!("skipping {test_name} because no usable Chrome was found: {error}");
            None
        }
    }
}

/// How many live processes have `text` on their command line.
fn processes_naming(text: &str) -> usize {
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .without_tasks()
            .with_cmd(UpdateKind::Always),
    );
    system
        .processes()
        .values()
        .filter(|process| {
            process.status() != ProcessStatus::Zombie
                && process
                    .cmd()
                    .iter()
                    .any(|argument| argument.to_string_lossy().contains(text))
        })
        .count()
}

/// A killed browser's processes are found through the process tree, not by matching text, so a
/// profile path with a space in it is no different: every process is gone and the directory is
/// removed when the kill returns.
#[tokio::test]
async fn a_killed_browser_with_a_space_in_its_profile_path_leaves_no_process_and_no_directory() {
    let test_name = "a_killed_browser_with_a_space_in_its_profile_path_leaves_no_process_and_no_directory";
    let root = std::env::temp_dir().join(format!("crawlberg spaced {}", std::process::id()));
    let profile = root.join("crawlberg-interact-0-0");
    let Some((browser, task)) = launch_for(test_name, &profile).await else {
        return;
    };
    let text = profile.display().to_string();
    let before = processes_naming(&text);
    kill_browser(browser, task, profile.clone(), Duration::from_secs(5)).await;
    let after = processes_naming(&text);
    let left = profile.exists();
    let _ = std::fs::remove_dir_all(&root);

    assert!(
        before > 1,
        "{test_name}: the launched Chrome must have helper processes that name its profile, got {before}"
    );
    assert_eq!(
        after, 0,
        "{test_name}: no process may still run with the profile once the kill returns"
    );
    assert!(!left, "{test_name}: the profile must be removed");
}

/// The kill ends the browser it launched and nothing else: a process of another program that
/// names crawlberg's profile on its command line, and a Chrome on a profile whose path starts
/// with crawlberg's, both survive it.
#[tokio::test]
async fn kill_browser_ends_only_the_browser_it_launched() {
    let test_name = "kill_browser_ends_only_the_browser_it_launched";
    if !cfg!(unix) {
        return;
    }
    let root = std::env::temp_dir().join(format!("crawlberg-only-ours-{}", std::process::id()));
    let profile = root.join("crawlberg-interact-0-0");
    let Some((browser, task)) = launch_for(test_name, &profile).await else {
        return;
    };
    let Some((mut foreign, foreign_task)) = launch_for(test_name, &root.join("crawlberg-interact-0-0 copy")).await
    else {
        return;
    };
    let mut wrapper = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("sleep 60; : --user-data-dir={}", profile.display()))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("sh must start");

    kill_browser(browser, task, profile.clone(), Duration::from_secs(5)).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let wrapper_alive = wrapper.try_wait().expect("the wrapper must be waitable").is_none();
    let foreign_alive = foreign
        .get_mut_child()
        .is_some_and(|child| matches!(child.try_wait(), Ok(None)));
    let foreign_answers = tokio::time::timeout(Duration::from_secs(5), foreign.version()).await;

    let _ = wrapper.kill();
    let _ = wrapper.wait();
    let _ = foreign.kill().await;
    foreign_task.abort();
    let _ = std::fs::remove_dir_all(&root);

    assert!(
        wrapper_alive,
        "{test_name}: a process of another program that names crawlberg's profile must survive the kill"
    );
    assert!(
        foreign_alive,
        "{test_name}: a Chrome on a profile whose path starts with crawlberg's must survive the kill"
    );
    assert!(
        matches!(foreign_answers, Ok(Ok(_))),
        "{test_name}: the other Chrome must still answer after the kill: {foreign_answers:?}"
    );
}

/// A family that keeps forking is stopped as it is found, and every member has taken its stop when
/// the collection returns, so a child forked while the family is collected cannot slip past the
/// kill.
#[test]
fn a_forking_family_is_stopped_as_it_is_found_and_leaves_no_child_behind() {
    let test_name = "a_forking_family_is_stopped_as_it_is_found_and_leaves_no_child_behind";
    if !cfg!(unix) {
        return;
    }
    let marker = format!("crawlberg-family-test-{}", std::process::id());
    let mut parent = std::process::Command::new("sh")
        .arg("-c")
        .arg("while :; do sh -c 'sleep 2; :' \"$0\" & sleep 0.01; done")
        .arg(&marker)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("sh must start");
    std::thread::sleep(Duration::from_millis(200));

    let family = ChromeFamily::freeze(parent.id());
    let running = {
        let mut system = System::new();
        ChromeFamily::refresh(&mut system, ProcessesToUpdate::Some(&family.members));
        family
            .members
            .iter()
            .filter(|pid| {
                system
                    .process(**pid)
                    .is_some_and(|process| !matches!(process.status(), ProcessStatus::Stop | ProcessStatus::Zombie))
            })
            .count()
    };
    family.kill();
    let gone = family.wait(Duration::from_secs(5));
    let _ = parent.wait();
    std::thread::sleep(Duration::from_millis(100));
    let survivors = processes_naming(&marker);

    assert!(
        family.members.len() > 2,
        "{test_name}: the family must hold the shell and its children, got {}",
        family.members.len()
    );
    assert_eq!(
        running, 0,
        "{test_name}: every member must have taken its stop when the collection returns, {running} still ran"
    );
    assert!(gone, "{test_name}: every member must be gone within the limit");
    assert_eq!(
        survivors, 0,
        "{test_name}: no child forked while the family was collected may survive the kill"
    );
}

/// The kill ends every process the browser started, not only the main process: Chrome is launched
/// through a script that leaves a marked child behind before it becomes Chrome, and that child must
/// be gone when the kill returns.
///
/// ~keep Chrome's own helpers exit on their own soon after the main process is killed, so a kill
/// ~keep that collected nothing passed the profile tests on an idle host and left the directory
/// ~keep behind only under load. The script's child never exits on its own, so the kill has to
/// ~keep end it. The child's parent is the script's shell, which becomes Chrome's main process
/// ~keep when it execs, so the child is family.
#[tokio::test]
#[allow(
    clippy::print_stderr,
    reason = "test-only skip announcement, matching tests/common/mod.rs's convention"
)]
async fn kill_browser_ends_a_process_the_browser_started() {
    let test_name = "kill_browser_ends_a_process_the_browser_started";
    if !cfg!(unix) {
        return;
    }
    let Ok(chrome) = default_executable(DetectionOptions::default()) else {
        eprintln!("skipping {test_name} because no usable Chrome was found");
        return;
    };
    let root = std::env::temp_dir().join(format!("crawlberg-wrapped-{}", std::process::id()));
    let profile = root.join("crawlberg-interact-0-0");
    let marker = format!("crawlberg-left-behind-{}", std::process::id());
    let script = root.join("chrome-that-leaves-a-child.sh");
    std::fs::create_dir_all(&root).expect("the test directory must be created");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nsh -c 'sleep 60; : {marker}' >/dev/null 2>&1 &\nexec \"{}\" \"$@\"\n",
            chrome.display()
        ),
    )
    .expect("the script must be written");
    let executable = std::process::Command::new("chmod")
        .arg("755")
        .arg(&script)
        .status()
        .is_ok_and(|status| status.success());
    assert!(executable, "{test_name}: the script must be made executable");
    let builder = build_pool_launch_builder(&profile, &BrowserPoolConfig::default())
        .expect("the default pool config names no binary to check")
        .chrome_executable(&script);
    let Some((browser, task)) = launch_config(test_name, builder).await else {
        return;
    };
    let before = processes_naming(&marker);
    kill_browser(browser, task, profile.clone(), Duration::from_secs(5)).await;
    let after = processes_naming(&marker);
    let _ = std::process::Command::new("pkill").args(["-f", &marker]).status();
    let _ = std::fs::remove_dir_all(&root);

    assert_eq!(
        before, 1,
        "{test_name}: the script must leave one child behind before it becomes Chrome, got {before}"
    );
    assert_eq!(
        after, 0,
        "{test_name}: a process the browser started must be gone when the kill returns, {after} left"
    );
}

/// The wait for a member's stop ends once the member has taken it, and at its bound while a member
/// has not.
#[test]
fn the_wait_for_a_stop_ends_when_it_is_taken_or_at_its_bound() {
    let test_name = "the_wait_for_a_stop_ends_when_it_is_taken_or_at_its_bound";
    if !cfg!(unix) {
        return;
    }
    let mut stopped = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("sleep must start");
    let mut running = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("sleep must start");
    let sent = std::process::Command::new("kill")
        .args(["-STOP", &stopped.id().to_string()])
        .status()
        .is_ok_and(|status| status.success());
    let members = [Pid::from_u32(stopped.id()), Pid::from_u32(running.id())];
    let (report, waited) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let started = std::time::Instant::now();
        ChromeFamily::settle(&mut System::new(), &members);
        let _ = report.send(started.elapsed());
    });
    let waited = waited.recv_timeout(ChromeFamily::SETTLE * 5);
    let started = std::time::Instant::now();
    ChromeFamily::settle(&mut System::new(), &members[..1]);
    let settled_in = started.elapsed();
    let _ = stopped.kill();
    let _ = stopped.wait();
    let _ = running.kill();
    let _ = running.wait();

    assert!(sent, "{test_name}: the stop must be sent");
    assert!(
        matches!(waited, Ok(waited) if waited >= ChromeFamily::SETTLE),
        "{test_name}: the wait must last until its bound while a member has not taken its stop, got {waited:?}"
    );
    assert!(
        settled_in < ChromeFamily::SETTLE,
        "{test_name}: the wait must end once every member has taken its stop, it took {settled_in:?}"
    );
}

/// The wait for a family ends when its members are gone, and at its limit while they are not.
#[test]
fn the_wait_for_a_family_ends_with_its_members_or_at_its_limit() {
    let test_name = "the_wait_for_a_family_ends_with_its_members_or_at_its_limit";
    if !cfg!(unix) {
        return;
    }
    let mut child = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("sleep must start");
    let family = ChromeFamily {
        members: vec![Pid::from_u32(child.id())],
    };
    let started = std::time::Instant::now();
    let gone_early = family.wait(Duration::from_millis(300));
    let waited = started.elapsed();
    let _ = child.kill();
    let _ = child.wait();
    let gone = family.wait(Duration::from_secs(5));

    assert!(
        !gone_early,
        "{test_name}: the wait must report a member still running at its limit"
    );
    assert!(
        waited >= Duration::from_millis(300),
        "{test_name}: the wait must last until its limit, it ended after {waited:?}"
    );
    assert!(gone, "{test_name}: the wait must end once every member is gone");
}

/// A member whose main thread has exited reads as a zombie while another thread of it still
/// runs, and the wait holds until that thread is gone too.
///
/// ~keep The child's main thread leaves through pthread_exit while a second thread sleeps on:
/// ~keep the shape a killed Chrome process takes while a thread of it finishes a write.
#[test]
#[allow(
    clippy::print_stderr,
    reason = "test-only skip announcement, matching tests/common/mod.rs's convention"
)]
fn the_wait_for_a_family_holds_while_a_thread_of_a_zombie_member_runs() {
    let test_name = "the_wait_for_a_family_holds_while_a_thread_of_a_zombie_member_runs";
    if !cfg!(target_os = "linux") {
        return;
    }
    let Ok(mut child) = std::process::Command::new("python3")
        .arg("-c")
        .arg(
            "import ctypes, threading, time\nthreading.Thread(target=time.sleep, args=(30,)).start()\n\
             ctypes.CDLL(None).pthread_exit(None)",
        )
        .spawn()
    else {
        eprintln!("skipping {test_name} because python3 is not available");
        return;
    };
    let pid = Pid::from_u32(child.id());
    let mut system = System::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut reads_as_zombie = false;
    while std::time::Instant::now() < deadline {
        system.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), true, ProcessRefreshKind::nothing());
        if system
            .process(pid)
            .is_some_and(|process| process.status() == ProcessStatus::Zombie)
        {
            reads_as_zombie = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let family = ChromeFamily { members: vec![pid] };
    let started = std::time::Instant::now();
    let gone_while_running = family.wait(Duration::from_millis(500));
    let held = started.elapsed();
    let _ = child.kill();
    let _ = child.wait();
    let gone = family.wait(Duration::from_secs(5));

    assert!(
        reads_as_zombie,
        "{test_name}: the child's main thread must have exited so that it reads as a zombie"
    );
    assert!(
        !gone_while_running && held >= Duration::from_millis(500),
        "{test_name}: a member with a running thread must not be taken as gone, gone={gone_while_running} after {held:?}"
    );
    assert!(gone, "{test_name}: the member must be gone once its threads are");
}

/// A kill that cannot run falls back to releasing the browser, and the profile is still removed.
///
/// ~keep A browser reached through `Browser::connect` has no child process, so its kill returns
/// ~keep nothing: that is the failed kill this drives.
#[tokio::test]
#[allow(
    clippy::print_stderr,
    reason = "test-only skip announcement, matching tests/common/mod.rs's convention"
)]
async fn kill_browser_releases_a_browser_it_cannot_kill_and_removes_the_profile() {
    let user_data_dir = std::env::temp_dir().join(format!("crawlberg-kill-fallback-test-{}", std::process::id()));
    let launched = match build_pool_launch_builder(&user_data_dir, &BrowserPoolConfig::default())
        .expect("the default pool config names no binary to check")
        .build()
    {
        Ok(config) => Browser::launch(config).await.map_err(|error| error.to_string()),
        Err(error) => Err(error),
    };
    let (mut owner, mut owner_handler) = match launched {
        Ok(pair) => pair,
        Err(error) => {
            eprintln!(
                "skipping kill_browser_releases_a_browser_it_cannot_kill_and_removes_the_profile \
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
    let profile = std::env::temp_dir().join(format!("crawlberg-kill-fallback-profile-{}", std::process::id()));
    std::fs::create_dir_all(profile.join("Default")).expect("the profile must be created");

    kill_browser(connected, handler_task, profile.clone(), Duration::from_secs(5)).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let released = handler_abort.is_finished();
    let removed = !profile.exists();

    let _ = owner.kill().await;
    owner_task.abort();
    let _ = std::fs::remove_dir_all(&user_data_dir);
    let _ = std::fs::remove_dir_all(&profile);

    assert!(
        released,
        "a browser the kill cannot end must be released: its handler task must stop"
    );
    assert!(removed, "the profile must be removed after the fallback release");
}

/// A launched browser opens no tab of its own, so nothing loads before crawlberg asks.
#[tokio::test]
#[allow(
    clippy::print_stderr,
    reason = "test-only skip announcement, matching tests/common/mod.rs's convention"
)]
async fn a_launched_browser_opens_no_startup_tab() {
    let user_data_dir = std::env::temp_dir().join(format!("crawlberg-startup-tab-test-{}", std::process::id()));
    let launched = match build_pool_launch_builder(&user_data_dir, &BrowserPoolConfig::default())
        .expect("the default pool config names no binary to check")
        .build()
    {
        Ok(config) => Browser::launch(config).await,
        Err(error) => {
            eprintln!("skipping a_launched_browser_opens_no_startup_tab: no usable Chrome: {error}");
            return;
        }
    };
    let (mut browser, mut handler) = match launched {
        Ok(pair) => pair,
        Err(error) => {
            eprintln!("skipping a_launched_browser_opens_no_startup_tab: no usable Chrome: {error}");
            return;
        }
    };
    let handler_task = tokio::spawn(async move { while handler.next().await.is_some() {} });
    tokio::time::sleep(Duration::from_millis(1000)).await;
    let pages: Vec<String> = browser
        .fetch_targets()
        .await
        .expect("the targets must be listed")
        .into_iter()
        .filter(|target| target.r#type == "page")
        .map(|target| target.url)
        .collect();
    let _ = close_browser_within(&mut browser, HANDLER_SHUTDOWN_TIMEOUT).await;
    handler_task.abort();
    let _ = std::fs::remove_dir_all(&user_data_dir);
    assert!(
        pages.is_empty(),
        "the browser must open no tab of its own, got {pages:?}"
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
    let builder = apply_default_args(BrowserConfig::builder(), &[]);
    assert_launch_flags_are_normalized(&builder);
}

#[test]
fn the_pool_launch_builder_carries_no_double_dashed_flag_and_the_macos_keychain_flag() {
    // ~keep Behavioral, not textual: this calls the exact function `launch_browser`
    // ~keep uses to build its `BrowserConfig`, so a path that stops calling
    // ~keep `apply_default_args` (even by looping over a raw flag instead) fails here
    // ~keep because the returned flags actually change.
    let builder = build_pool_launch_builder(
        std::path::Path::new("/tmp/pool-test-profile"),
        &BrowserPoolConfig::default(),
    )
    .expect("the default pool config names no binary to check");
    assert_launch_flags_are_normalized(&builder);
}

#[test]
fn the_pool_launch_builder_uses_the_configured_chrome_path_and_args() {
    assert_launch_overrides_reach_the_builder(|chrome_path, chrome_args| {
        build_pool_launch_builder(
            std::path::Path::new("/tmp/pool-test-profile"),
            &BrowserPoolConfig {
                chrome_path,
                chrome_args,
                ..BrowserPoolConfig::default()
            },
        )
    });
}

#[test]
fn the_pool_launch_builder_refuses_the_chrome_args_validate_refuses_and_names_the_pool_key() {
    for (chrome_args, expected) in [
        (
            vec!["disable-gpu"],
            "browser: BrowserPoolConfig.chrome_args entry \"disable-gpu\" must start with --",
        ),
        (
            vec!["--headless=new"],
            "browser: BrowserPoolConfig.chrome_args must not set --headless; crawlberg sets it to run Chrome",
        ),
        (
            vec!["--LANG=fr"],
            "browser: BrowserPoolConfig.chrome_args entry \"--LANG=fr\" must name the flag in lowercase",
        ),
        (
            vec!["--enable-features=A", "--enable-features=B"],
            "browser: BrowserPoolConfig.chrome_args sets --enable-features more than once",
        ),
    ] {
        let err = build_pool_launch_builder(
            std::path::Path::new("/tmp/pool-test-profile"),
            &BrowserPoolConfig {
                chrome_args: chrome_args.iter().map(|arg| (*arg).to_owned()).collect(),
                ..BrowserPoolConfig::default()
            },
        )
        .expect_err("the pool must refuse what CrawlConfig::validate refuses")
        .to_string();
        assert!(err.contains(expected), "{chrome_args:?}: unexpected error: {err}");
    }
}

#[tokio::test]
async fn a_pool_with_a_missing_chrome_path_fails_to_launch_and_names_the_path() {
    let pool = BrowserPool::new(BrowserPoolConfig {
        chrome_path: Some(std::path::PathBuf::from("/nonexistent/crawlberg-pool-chrome")),
        ..BrowserPoolConfig::default()
    });
    let error = match pool.acquire_page().await {
        Ok(_) => panic!("a pool must not launch a different Chrome when chrome_path is missing"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains("BrowserPoolConfig.chrome_path '/nonexistent/crawlberg-pool-chrome' cannot be used"),
        "the error must name the pool key and the path, got: {error}"
    );
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
        let code_only: String = src
            .split("#[cfg(test)]")
            .next()
            .unwrap_or(src)
            .lines()
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

/// Every spelling of a WebSocket endpoint that the endpoint checks accept, each with its own
/// path so a recorded request names the spelling that sent it: upper case, mixed case, padded
/// with spaces, no `//`, and the plain lower-case form as the control.
fn accepted_endpoint_spellings(port: u16) -> Vec<(String, &'static str)> {
    vec![
        (format!("ws://127.0.0.1:{port}/lower"), "/lower"),
        (format!("WS://127.0.0.1:{port}/upper"), "/upper"),
        (format!("Ws://127.0.0.1:{port}/mixed"), "/mixed"),
        (format!(" ws://127.0.0.1:{port}/padded "), "/padded"),
        (format!("ws:127.0.0.1:{port}/no-slashes"), "/no-slashes"),
    ]
}

/// Drive `connect` with every accepted endpoint spelling against a local listener, and assert
/// that each one reaches it: an endpoint the checks accept must never fail before it connects.
///
/// ~keep The listener answers every request with HTTP 418 and records the request line before
/// ~keep it answers, so the connect fails fast after the request arrived and the record is
/// ~keep complete when `connect` returns. A closed port cannot tell the two failures apart:
/// ~keep the TCP connect happens before the scheme check, so every spelling is refused alike.
pub(crate) async fn assert_every_accepted_endpoint_reaches_the_browser<F, Fut, T>(mut connect: F)
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = Result<T, CrawlError>>,
{
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a local port must be free");
    let port = listener.local_addr().expect("the listener has an address").port();
    let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let recorder = Arc::clone(&seen);
    let server = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]).into_owned();
            let line = request.lines().next().unwrap_or_default().to_owned();
            recorder.lock().expect("the recorder lock is never poisoned").push(line);
            let _ = stream
                .write_all(b"HTTP/1.1 418 I'm a teapot\r\nContent-Length: 0\r\n\r\n")
                .await;
        }
    });

    let mut missed = Vec::new();
    for (endpoint, path) in accepted_endpoint_spellings(port) {
        assert!(
            crate::net::is_websocket_scheme(&endpoint),
            "the endpoint checks must accept {endpoint:?}"
        );
        let outcome = tokio::time::timeout(Duration::from_secs(10), connect(endpoint.clone())).await;
        let error = match outcome {
            Ok(Ok(_)) => panic!("a listener that answers 418 must not complete a connect for {endpoint:?}"),
            Ok(Err(e)) => e.to_string(),
            Err(_) => panic!("the connect for {endpoint:?} did not finish within 10 seconds"),
        };
        let expected = format!("GET {path} HTTP/1.1");
        if !seen
            .lock()
            .expect("the recorder lock is never poisoned")
            .contains(&expected)
        {
            missed.push(format!("{endpoint:?} (connect error: {error})"));
        }
    }
    server.abort();
    assert!(
        missed.is_empty(),
        "accepted endpoints that never reached the browser: {missed:?}"
    );
}

#[tokio::test]
async fn the_pool_connects_every_endpoint_spelling_the_checks_accept() {
    assert_every_accepted_endpoint_reaches_the_browser(|endpoint| async move {
        let pool = BrowserPool::new(BrowserPoolConfig {
            browser_endpoint: Some(endpoint),
            launch_timeout: Duration::from_secs(5),
            ..BrowserPoolConfig::default()
        });
        pool.warm().await
    })
    .await;
}

/// The pool's connect-error message must never carry a `browser.endpoint` password or path
/// token, though the failing origin must still be readable for debugging.
///
/// ~keep The launch path and the interact backend have the same test (`browser/launch.rs`,
/// ~keep `interact/chromiumoxide.rs`): xberg-io/crawlberg#473 was this test missing for one
/// ~keep connect site after another added it for a different one, so each site keeps its own,
/// ~keep including the pool. A closed local port refuses the connection immediately, so this
/// ~keep needs no real Chrome and stays fast; `ws://` skips chromiumoxide's `json/version` HTTP
/// ~keep probe and goes straight to the WebSocket handshake.
#[tokio::test]
async fn pool_connect_error_prints_only_the_endpoint_origin() {
    let pool = BrowserPool::new(BrowserPoolConfig {
        browser_endpoint: Some("ws://user:hunter2@127.0.0.1:1/devtools/browser/b1946ac9-guid".into()),
        launch_timeout: Duration::from_secs(5),
        ..BrowserPoolConfig::default()
    });

    let err = pool
        .warm()
        .await
        .expect_err("a refused local port must fail the connect");
    let msg = err.to_string();
    assert!(
        !msg.contains("hunter2"),
        "password must not survive into the error, got: {msg}"
    );
    assert!(
        !msg.contains("b1946ac9-guid"),
        "the CDP path token must not survive into the error, got: {msg}"
    );
    assert!(
        msg.contains("127.0.0.1"),
        "host must still appear in the error, got: {msg}"
    );
}
