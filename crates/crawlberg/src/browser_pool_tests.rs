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
    /// Test-only seam: [`kill_if_chrome_using`] fires this once its re-check confirms the pid still
    /// runs `chrome`, before it sends the kill. A test installs a hook here to force a pid reuse in
    /// exactly the window a pidfd exists to close.
    pub(crate) static REUSE_WINDOW_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Call and clear the reuse-window hook a test installed on [`REUSE_WINDOW_HOOK`], if any.
pub(crate) fn fire_reuse_window_hook() {
    let hook = REUSE_WINDOW_HOOK.with(|cell| cell.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
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
    let chrome = chrome_started_by(helper.id(), &path).expect("the stand-in's executable must be readable");

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

/// Dropping a profile directory stops the Chrome launched on it and leaves running a process that
/// is not Chrome, even one that carries the exact flag as an argument of its own, as a shell,
/// `strace` or `grep` can.
///
/// ~keep A `cat` blocked on its stdin stands in for the Chrome launched on the directory, with the
/// ~keep flag as an operand it never reaches. It lies in the same directory as the bystander's `sh`,
/// ~keep as a launcher such as `/usr/bin/snap` lies beside shells, so only the executable itself
/// ~keep tells the two apart.
#[cfg(unix)]
#[test]
fn dropping_a_profile_directory_leaves_a_process_that_is_not_chrome_running() {
    let mut dir =
        ScratchProfileDir::create("crawlberg-profile-bystander-test-").expect("the directory must be creatable");
    let path = dir.path().to_path_buf();
    let flag = user_data_dir_flag(&path);
    let mut chrome = std::process::Command::new("cat")
        .args(["--", "-", &flag])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("cat must start");
    let mut bystander = spawn_bystander(&flag);
    dir.record_chrome(chrome.id());

    drop(dir);
    let removed = wait_for_removal(&path);

    let stopped = chrome.try_wait().expect("the status must be readable").is_some();
    let running = bystander.try_wait().expect("the status must be readable").is_none();
    let _ = bystander.kill();
    let _ = bystander.wait();
    let _ = chrome.kill();
    let _ = chrome.wait();
    assert!(removed, "the directory must be removed");
    assert!(stopped, "the Chrome launched on the directory must be killed");
    assert!(running, "a process that is not Chrome must not be killed");
}

/// A pid from the scan that now names a process without the flag, as a pid reused since the scan
/// does, is not killed.
///
/// ~keep `/` holds every executable, so only the check of the command line spares the process.
#[cfg(unix)]
#[test]
fn a_scanned_pid_whose_process_no_longer_names_the_profile_is_not_killed() {
    let dir = tempfile::tempdir().expect("the directory must be creatable");
    let mut other = std::process::Command::new("cat")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("cat must start");

    kill_if_chrome_using(
        sysinfo::Pid::from_u32(other.id()),
        &user_data_dir_flag(dir.path()),
        std::path::Path::new("/"),
    );
    std::thread::sleep(Duration::from_millis(200));

    let running = other.try_wait().expect("the status must be readable").is_none();
    let _ = other.kill();
    let _ = other.wait();
    assert!(running, "a process that does not name the profile must not be killed");
}

/// How many times a forced-reuse test repeats its whole scenario before it gives up.
///
/// ~keep `ns_last_pid` is one setting for the whole pid namespace, so any process that forks between
/// ~keep the write and the test's own fork takes the freed pid first, and the test's process lands on
/// ~keep another one. Nothing in a test can stop an unrelated fork, so each test detects a miss,
/// ~keep cleans up and repeats the scenario with a fresh scanned process and a fresh pid. The
/// ~keep assertions run only on an attempt where the pid landed, so the retry does not weaken them.
#[cfg(target_os = "linux")]
const PID_REUSE_ATTEMPTS: usize = 50;

/// What happened when a test tried to start a new process on a freed pid.
#[cfg(target_os = "linux")]
enum PidReuse {
    /// Writing `ns_last_pid` needs root, and the test does not have it.
    NotRoot,
    /// The new process runs on the freed pid.
    Landed(std::process::Child),
    /// Another fork took the pid first; the new process runs on a different one.
    Missed(std::process::Child),
}

/// Point `ns_last_pid` just below `pid`, which must be free, and start a `cat` on it.
#[cfg(target_os = "linux")]
fn spawn_on_pid(pid: u32) -> PidReuse {
    if std::fs::write("/proc/sys/kernel/ns_last_pid", (pid - 1).to_string()).is_err() {
        return PidReuse::NotRoot;
    }
    let child = std::process::Command::new("cat")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("cat must start");
    if child.id() == pid {
        PidReuse::Landed(child)
    } else {
        PidReuse::Missed(child)
    }
}

/// Kill and reap a process the test started.
#[cfg(target_os = "linux")]
fn stop(mut child: std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Start a `cat` that names `flag`, and return it with its executable and the pids a scan for
/// `flag` finds running that executable.
#[cfg(target_os = "linux")]
fn spawn_scanned(dir: &std::path::Path, flag: &str) -> (std::process::Child, std::path::PathBuf, Vec<sysinfo::Pid>) {
    let scanned = std::process::Command::new("cat")
        .args(["--", "-", flag])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("cat must start");
    let chrome = chrome_started_by(scanned.id(), dir).expect("the scanned process's executable must be readable");
    let mut system = sysinfo::System::new();
    let users: Vec<_> = processes_naming(&mut system, flag)
        .into_iter()
        .filter(|process| runs(process, &chrome))
        .map(sysinfo::Process::pid)
        .collect();
    assert_eq!(
        users,
        [sysinfo::Pid::from_u32(scanned.id())],
        "the scan must find the process"
    );
    (scanned, chrome, users)
}

/// A pid reused between the scan and the kill is not killed.
///
/// ~keep Forces the reuse as the review did: the scanned process exits and is reaped, then
/// ~keep `ns_last_pid` hands its pid to a new process. Writing `ns_last_pid` needs root, so the test
/// ~keep returns early without it, and says so. See [`PID_REUSE_ATTEMPTS`] for why it repeats.
/// ~keep `#[serial_test::serial]` keeps it from racing the recheck-window test, which writes the
/// ~keep same setting, as `sitemap.rs` and `map.rs` do for their host-wide state.
#[cfg(target_os = "linux")]
#[test]
#[serial_test::serial(ns_last_pid)]
#[allow(clippy::print_stderr, reason = "test-only skip announcement")]
fn a_pid_reused_between_the_scan_and_the_kill_is_not_killed() {
    let dir = tempfile::tempdir().expect("the directory must be creatable");
    let flag = user_data_dir_flag(dir.path());
    for _ in 0..PID_REUSE_ATTEMPTS {
        let (scanned, chrome, users) = spawn_scanned(dir.path(), &flag);
        let pid = scanned.id();
        stop(scanned);
        let mut reused = match spawn_on_pid(pid) {
            PidReuse::NotRoot => {
                eprintln!("skipping: choosing the next pid needs root");
                return;
            }
            PidReuse::Missed(other) => {
                stop(other);
                continue;
            }
            PidReuse::Landed(reused) => reused,
        };

        for &user in &users {
            kill_if_chrome_using(user, &flag, &chrome);
        }
        std::thread::sleep(Duration::from_millis(200));

        let running = reused.try_wait().expect("the status must be readable").is_none();
        stop(reused);
        assert!(running, "the process that reused the pid must not be killed");
        return;
    }
    panic!(
        "the new process must reuse the scanned pid: other forks took it first in all {PID_REUSE_ATTEMPTS} attempts"
    );
}

/// A pid reused between the re-check and the kill, inside [`kill_if_chrome_using`], is not killed.
///
/// ~keep The forced-reuse test above reuses the pid before `kill_if_chrome_using` runs, so its own
/// ~keep re-check catches the reuse and nothing reaches the gap between the re-check and the kill: a
/// ~keep pidfd protects against reuse in that exact gap, and nothing else does. This test installs
/// ~keep the [`REUSE_WINDOW_HOOK`] the production code fires right there, and inside it reaps the
/// ~keep scanned process and forces its pid onto a new, innocent one, so the reuse lands after the
/// ~keep re-check has already passed. Root, like the test above; returns early without it, and says so.
/// ~keep Repeats on a miss like the test above, and shares its `ns_last_pid` serial key.
#[cfg(target_os = "linux")]
#[test]
#[serial_test::serial(ns_last_pid)]
#[allow(clippy::print_stderr, reason = "test-only skip announcement")]
fn a_pid_reused_between_the_recheck_and_the_kill_is_not_killed() {
    let dir = tempfile::tempdir().expect("the directory must be creatable");
    let flag = user_data_dir_flag(dir.path());
    for _ in 0..PID_REUSE_ATTEMPTS {
        let (scanned, chrome, users) = spawn_scanned(dir.path(), &flag);
        let pid = scanned.id();
        let outcome = std::rc::Rc::new(std::cell::RefCell::new(None::<PidReuse>));
        {
            let outcome = outcome.clone();
            REUSE_WINDOW_HOOK.with(|cell| {
                *cell.borrow_mut() = Some(Box::new(move || {
                    stop(scanned);
                    *outcome.borrow_mut() = Some(spawn_on_pid(pid));
                }));
            });
        }

        for &user in &users {
            kill_if_chrome_using(user, &flag, &chrome);
        }
        std::thread::sleep(Duration::from_millis(200));

        let outcome = outcome.borrow_mut().take();
        let mut innocent = match outcome.expect("the hook must run and spawn the innocent process") {
            PidReuse::NotRoot => {
                eprintln!("skipping: choosing the next pid needs root");
                return;
            }
            PidReuse::Missed(other) => {
                stop(other);
                continue;
            }
            PidReuse::Landed(innocent) => innocent,
        };
        let running = innocent.try_wait().expect("the status must be readable").is_none();
        stop(innocent);
        assert!(running, "the process that reused the pid must not be killed");
        return;
    }
    panic!(
        "the new process must reuse the scanned pid: other forks took it first in all {PID_REUSE_ATTEMPTS} attempts"
    );
}

/// Removing the profile directory of a Chrome that is still running stops that Chrome first.
///
/// ~keep A real Chrome, because Chrome rewrites the command line of each of its processes into
/// ~keep one space-joined string, which a stand-in started with separate arguments does not do, and
/// ~keep because its helpers must run the executable the launch reads from the process tree.
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
    let (browser, handler, dir) = match dir.launch(config).await {
        Ok(launched) => launched,
        Err(error) => {
            eprintln!("skipping: no usable Chrome: {error}");
            return;
        }
    };
    assert_dropping_the_profile_stops_its_chrome(browser, handler, dir, path).await;
}

/// Drop `profile`, the profile directory of the running Chrome `browser` at `path`, and assert
/// that the drop stops that Chrome and removes the directory for good.
///
/// ~keep The browser's exit is read from its own handle, not from the process scan under test.
pub(crate) async fn assert_dropping_the_profile_stops_its_chrome<P>(
    mut browser: Browser,
    mut handler: Handler,
    profile: P,
    path: std::path::PathBuf,
) {
    let handler_handle = tokio::spawn(async move { while handler.next().await.is_some() {} });

    drop(profile);
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
        "dropping the profile directory must stop the Chrome still using it"
    );
    tokio::task::spawn_blocking(move || assert_profile_directory_is_gone_for_good(&path))
        .await
        .expect("no Chrome may use or recreate the profile directory after it is removed");
}

/// Write an executable `sh` script at `path` that runs `chrome` with its own arguments, through
/// `exec` or as a child of the script.
#[cfg(unix)]
fn write_launcher(path: &std::path::Path, chrome: &std::path::Path, exec: bool) {
    use std::os::unix::fs::PermissionsExt as _;

    let run = if exec { "exec " } else { "" };
    std::fs::write(path, format!("#!/bin/sh\n{run}'{}' \"$@\"\n", chrome.display()))
        .expect("the launcher must be writable");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("the launcher must be executable");
}

/// Dropping a running Chrome's profile directory stops that Chrome and leaves a shell naming the
/// directory running, whether the launched executable is Chrome's own launcher, a script that
/// `exec`s Chrome, or a script that runs Chrome as its child.
///
/// ~keep With the third launcher the process crawlberg starts is `sh`, not Chrome, and the
/// ~keep bystander runs that same `sh`, so the executable of the launched process tells nothing.
/// ~keep The launched process is `sh` and ends on its own once its Chrome child is killed.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
#[allow(clippy::print_stderr, reason = "test-only skip announcement")]
async fn a_profile_teardown_stops_the_launched_chrome_and_no_bystander_whatever_the_launcher() {
    let detection = chromiumoxide::detection::DetectionOptions {
        msedge: false,
        unstable: false,
    };
    let chrome = match chromiumoxide::detection::default_executable(detection) {
        Ok(chrome) => chrome,
        Err(error) => {
            eprintln!("skipping: no usable Chrome: {error}");
            return;
        }
    };
    let scripts = tempfile::tempdir().expect("the launcher directory must be creatable");
    let exec_launcher = scripts.path().join("exec-chrome");
    let child_launcher = scripts.path().join("child-chrome");
    write_launcher(&exec_launcher, &chrome, true);
    write_launcher(&child_launcher, &chrome, false);

    for launcher in [chrome.clone(), exec_launcher, child_launcher] {
        let dir = ScratchProfileDir::create("crawlberg-launcher-test-").expect("the directory must be creatable");
        let path = dir.path().to_path_buf();
        let config = build_pool_launch_builder(&path, &[])
            .chrome_executable(&launcher)
            .build()
            .expect("a config naming its executable must build");
        let (mut browser, mut handler, dir) = match dir.launch(config).await {
            Ok(launched) => launched,
            Err(error) => panic!(
                "{} must launch as {} did: {error}",
                launcher.display(),
                chrome.display()
            ),
        };
        let handler_handle = tokio::spawn(async move { while handler.next().await.is_some() {} });
        let mut bystander = spawn_bystander(&user_data_dir_flag(&path));

        drop(dir);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let mut exited = browser
            .try_wait()
            .expect("the launched process's status must be readable");
        while exited.is_none() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
            exited = browser
                .try_wait()
                .expect("the launched process's status must be readable");
        }
        let running = bystander.try_wait().expect("the status must be readable").is_none();
        let _ = bystander.kill();
        let _ = bystander.wait();
        let check = path.clone();
        let gone = tokio::task::spawn_blocking(move || assert_profile_directory_is_gone_for_good(&check)).await;
        if exited.is_none() {
            let _ = browser.kill().await;
        }
        handler_handle.abort();
        let mut system = sysinfo::System::new();
        for left in processes_naming(&mut system, &user_data_dir_flag(&path)) {
            left.kill();
        }
        let _ = std::fs::remove_dir_all(&path);

        let launcher = launcher.display();
        assert!(
            running,
            "{launcher}: a shell naming the profile directory must not be killed"
        );
        assert!(
            exited.is_some(),
            "{launcher}: the teardown must stop the Chrome it launched"
        );
        if let Err(error) = gone {
            std::panic::resume_unwind(error.into_panic());
        }
    }
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
