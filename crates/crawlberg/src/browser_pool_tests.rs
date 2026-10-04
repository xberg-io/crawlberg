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
#[allow(clippy::print_stderr, reason = "test-only skip announcement")]
async fn a_chrome_args_flag_reaches_the_chrome_the_pool_starts() {
    const TEST_NAME: &str = "a_chrome_args_flag_reaches_the_chrome_the_pool_starts";
    const MARKER: &str = "crawlberg-pool-chrome-args-marker";
    let pool = BrowserPool::new(BrowserPoolConfig {
        chrome_args: vec![format!("--user-agent={MARKER}")],
        ..BrowserPoolConfig::default()
    });
    match pool.warm().await {
        Ok(()) => {}
        Err(error) if error.to_string().contains("auto detect a chrome executable") => {
            eprintln!("skipping {TEST_NAME}: no Chrome executable: {error}");
            return;
        }
        Err(error) => panic!("{TEST_NAME}: the detected Chrome must launch: {error}"),
    }
    let path = pool_profile_dir(&pool).await;
    let marker_flag = format!("--user-agent={MARKER}");
    let reached_chrome = processes_naming(&mut sysinfo::System::new(), &user_data_dir_flag(&path))
        .iter()
        .flat_map(|process| process.cmd())
        .any(|argument| argument.to_string_lossy() == marker_flag);
    pool.shutdown().await;

    assert!(
        reached_chrome,
        "the pool's Chrome command line must carry the caller flag"
    );
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

/// How long a process test waits for what another thread or process brings about: a profile
/// directory removed, a process started or ended.
///
/// ~keep A bound for a poll, not a timing claim: each wait ends as soon as its condition holds.
pub(crate) const PROCESS_TEST_WAIT: Duration = Duration::from_secs(10);

/// Wait up to [`PROCESS_TEST_WAIT`] for the teardown another thread runs to remove `path`.
pub(crate) fn wait_for_removal(path: &std::path::Path) -> bool {
    let deadline = std::time::Instant::now() + PROCESS_TEST_WAIT;
    while path.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    !path.exists()
}

/// The entries still in the profile directory at `path` and the processes still naming it, for a
/// failure message.
pub(crate) fn what_is_left(path: &std::path::Path) -> String {
    let entries: Vec<_> = std::fs::read_dir(path)
        .map(|entries| entries.filter_map(Result::ok).map(|entry| entry.file_name()).collect())
        .unwrap_or_default();
    let mut system = sysinfo::System::new();
    let users: Vec<_> = processes_naming(&mut system, &user_data_dir_flag(path))
        .iter()
        .map(|process| (process.pid(), process.exe().map(std::path::Path::to_path_buf)))
        .collect();
    format!("entries left {entries:?}, processes naming it {users:?}")
}

/// Assert that the profile directory at `path` is removed, that no process uses it, and that it is
/// still gone a second later, when a helper that outlived its browser would have written again.
pub(crate) fn assert_profile_directory_is_gone_for_good(path: &std::path::Path) {
    assert!(
        wait_for_removal(path),
        "the profile directory must be removed: {}; {}",
        path.display(),
        what_is_left(path)
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

/// Set on the child copy of this test binary that [`assert_refused_launch_leaves_no_scratch_dir`]
/// starts: the child makes the refused launch itself, under a temp directory of its own.
const REFUSED_LAUNCH_CHILD: &str = "CRAWLBERG_TEST_REFUSED_LAUNCH_CHILD";

/// The marker before the refusal's error message that the child prints for its parent. libtest can
/// print the test's name on the same line first.
const REFUSED_LAUNCH_ERROR_LINE: &str = "crawlberg-refused-launch-error: ";

/// The names of the entries directly inside `dir`.
fn entries_in(dir: &std::path::Path) -> Vec<std::ffi::OsString> {
    std::fs::read_dir(dir)
        .expect("the temp directory must be readable")
        .filter_map(|entry| Some(entry.ok()?.file_name()))
        .collect()
}

/// Run `attempt`, a launch expected to fail before any Chrome starts, and assert it leaves nothing
/// in the temp directory: the scratch directory the launch's guard created for the launch it never
/// made is removed on the same refusal that failed it. Returns the error message for the caller's
/// own assertion.
///
/// ~keep The calling test starts this test binary again as a child that runs only that test, with
/// ~keep `TMPDIR` (and Windows' `TMP` and `TEMP`) set to a fresh directory that no other test or
/// ~keep process uses. The child makes the launch, waits up to [`PROCESS_TEST_WAIT`] for the
/// ~keep teardown task to empty that directory, and prints the error. The parent then asserts the
/// ~keep directory is empty, so a directory another test or process creates cannot turn it red.
/// ~keep The test thread's name is the test's full name, which libtest sets.
#[allow(clippy::print_stdout, reason = "the child reports the error to its parent on stdout")]
pub(crate) async fn assert_refused_launch_leaves_no_scratch_dir<F, Fut, T>(attempt: F) -> String
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<T, CrawlError>>,
{
    if std::env::var_os(REFUSED_LAUNCH_CHILD).is_some() {
        let error = match attempt().await {
            Ok(_) => panic!("a refused launch must return an error, not launch"),
            Err(e) => e.to_string(),
        };
        let temp = std::env::temp_dir();
        let deadline = tokio::time::Instant::now() + PROCESS_TEST_WAIT;
        while !entries_in(&temp).is_empty() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        println!("{REFUSED_LAUNCH_ERROR_LINE}{error}");
        return error;
    }
    let test = std::thread::current()
        .name()
        .expect("libtest names each test's thread after the test")
        .to_owned();
    let temp = tempfile::Builder::new()
        .prefix("crawlberg-refused-launch-test-")
        .tempdir()
        .expect("the directory must be creatable");
    let output = std::process::Command::new(std::env::current_exe().expect("the test binary must be readable"))
        .args([test.as_str(), "--exact", "--nocapture", "--test-threads=1"])
        .env(REFUSED_LAUNCH_CHILD, "1")
        .env("TMPDIR", temp.path())
        .env("TMP", temp.path())
        .env("TEMP", temp.path())
        .output()
        .expect("the test binary must start");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains(" 1 passed;"),
        "the child run of {test} must run it and pass: {}\nstdout:\n{stdout}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let left = entries_in(temp.path());
    assert!(
        left.is_empty(),
        "a refused launch left a scratch directory behind under {}: {left:?}",
        temp.path().display()
    );
    stdout
        .lines()
        .find_map(|line| Some(line.split_once(REFUSED_LAUNCH_ERROR_LINE)?.1))
        .expect("the child must print the refusal's error")
        .to_owned()
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
    let dir =
        ScratchProfileDir::create("crawlberg-profile-users-test-", None).expect("the directory must be creatable");
    let path = dir.path().to_path_buf();
    let mut helper = std::process::Command::new("sh")
        .arg("-c")
        .arg(r#"d="${0#--user-data-dir=}"; while :; do : > "$d/state"; done"#)
        .arg(user_data_dir_flag(&path))
        .spawn()
        .expect("sh must start");
    let deadline = std::time::Instant::now() + PROCESS_TEST_WAIT;
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

/// The teardown's wait on a process it killed lasts while the process runs and ends once it has
/// ended, before anything reaps it.
///
/// ~keep `sleep` stands in for a killed Chrome whose threads are still exiting: its pidfd is not
/// ~keep readable while any thread of it runs, so the wait runs to its deadline.
#[cfg(target_os = "linux")]
#[test]
fn the_wait_on_a_killed_process_lasts_until_it_has_ended() {
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("sleep must start");
    let pinned = i32::try_from(child.id())
        .ok()
        .and_then(rustix::process::Pid::from_raw)
        .expect("the pid must be valid");
    let pidfd = rustix::process::pidfd_open(pinned, rustix::process::PidfdFlags::empty()).expect("the pidfd must open");
    let killed = [pidfd];

    let ended_while_running = wait_until_ended(&killed, std::time::Instant::now() + Duration::from_millis(200));
    let _ = child.kill();
    let ended_once_killed = wait_until_ended(&killed, std::time::Instant::now() + PROCESS_TEST_WAIT);
    let _ = child.wait();

    assert!(
        !ended_while_running,
        "the wait must not end while the killed process still runs"
    );
    assert!(
        ended_once_killed,
        "the wait must end once the killed process has ended, before anything reaps it"
    );
}

/// Set on the child that [`the_teardown_removes_the_profile_only_after_every_thread_of_a_killed_process_has_exited`]
/// starts from this test binary: the profile directory the child reports ready in.
#[cfg(target_os = "linux")]
const HELD_THREAD_CHILD: &str = "CRAWLBERG_TEST_HELD_THREAD_CHILD";

/// A thread of a child process that this test thread traces, so that the thread stops at its exit
/// instead of exiting. Dropping it releases the thread.
#[cfg(target_os = "linux")]
struct HeldThread(libc::pid_t);

#[cfg(target_os = "linux")]
impl HeldThread {
    /// Trace the thread `tid` with `PTRACE_O_TRACEEXIT`, or return why the kernel refused.
    #[allow(unsafe_code)]
    fn seize(tid: libc::pid_t) -> std::io::Result<Self> {
        let options = std::ptr::without_provenance_mut::<libc::c_void>(libc::PTRACE_O_TRACEEXIT as usize);
        // ~keep SAFETY: PTRACE_SEIZE reads no memory through its pointer arguments; `data` carries the options.
        let rc = unsafe { libc::ptrace(libc::PTRACE_SEIZE, tid, std::ptr::null_mut::<libc::c_void>(), options) };
        if rc == -1 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(Self(tid))
        }
    }

    /// Wait up to [`PROCESS_TEST_WAIT`] for the thread to stop at its exit. Return whether it did.
    #[allow(unsafe_code)]
    fn stopped_at_exit(&self) -> bool {
        let deadline = std::time::Instant::now() + PROCESS_TEST_WAIT;
        loop {
            let mut status = 0;
            // ~keep SAFETY: `status` is a live, writable c_int for the call's whole duration.
            let rc = unsafe { libc::waitpid(self.0, &mut status, libc::__WALL | libc::WNOHANG) };
            if rc == self.0 {
                return libc::WIFSTOPPED(status) && status >> 8 == libc::SIGTRAP | (libc::PTRACE_EVENT_EXIT << 8);
            }
            if rc != 0 || std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for HeldThread {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // ~keep SAFETY: PTRACE_DETACH reads no memory through its pointer arguments.
        unsafe {
            libc::ptrace(
                libc::PTRACE_DETACH,
                self.0,
                std::ptr::null_mut::<libc::c_void>(),
                std::ptr::null_mut::<libc::c_void>(),
            )
        };
    }
}

/// The child side of the test below: start a second thread, report ready in `dir`, and park until
/// killed.
#[cfg(target_os = "linux")]
fn hold_two_threads_in(dir: &std::path::Path) -> ! {
    std::thread::spawn(|| {
        loop {
            std::thread::park();
        }
    });
    std::fs::write(dir.join("ready"), b"").expect("the ready file must be writable");
    loop {
        std::thread::park();
    }
}

/// The teardown removes the profile only after every thread of a process it killed has exited.
///
/// ~keep The scan stops seeing a killed process once its main thread is a zombie, while its other
/// ~keep threads can still be exiting. The test holds one of them at its exit: it starts this test
/// ~keep binary again as a child that runs only this test, names the directory on its command line
/// ~keep and runs a second thread. It traces that thread with `PTRACE_O_TRACEEXIT` and runs the
/// ~keep teardown on another thread. The kill leaves the main thread a zombie and the traced thread
/// ~keep stopped at its exit, so the teardown must neither return nor remove the directory until the
/// ~keep test releases the thread. A parent may trace its own child under Yama's default
/// ~keep `ptrace_scope` of 1. Where the kernel refuses the trace (`ptrace_scope` 3, a seccomp filter),
/// ~keep `PTRACE_SEIZE` fails with EPERM, EACCES or ENOSYS, and the test returns early and says so.
#[cfg(target_os = "linux")]
#[test]
#[allow(clippy::print_stderr, reason = "test-only skip announcement")]
fn the_teardown_removes_the_profile_only_after_every_thread_of_a_killed_process_has_exited() {
    if let Some(dir) = std::env::var_os(HELD_THREAD_CHILD) {
        hold_two_threads_in(std::path::Path::new(&dir));
    }
    let path = tempfile::Builder::new()
        .prefix("crawlberg-held-thread-test-")
        .tempdir()
        .expect("the directory must be creatable")
        .keep();
    let mut child = std::process::Command::new(std::env::current_exe().expect("the test binary must be readable"))
        .args([
            "the_teardown_removes_the_profile_only_after_every_thread_of_a_killed_process_has_exited",
            "--",
            &user_data_dir_flag(&path),
        ])
        .env(HELD_THREAD_CHILD, &path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("the test binary must start");
    let ready = path.join("ready");
    let deadline = std::time::Instant::now() + PROCESS_TEST_WAIT;
    while !ready.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(ready.exists(), "the child must start its second thread");
    let chrome = chrome_started_by(child.id(), &path).expect("the child's executable must be readable");
    let pid = libc::pid_t::try_from(child.id()).expect("the pid must fit a pid_t");
    let second = std::fs::read_dir(format!("/proc/{pid}/task"))
        .expect("the child's threads must be readable")
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<libc::pid_t>().ok())
        .find(|&tid| tid != pid)
        .expect("the child must run a second thread");
    let held = match HeldThread::seize(second) {
        Ok(held) => held,
        Err(error) if matches!(error.raw_os_error(), Some(libc::EPERM | libc::EACCES | libc::ENOSYS)) => {
            eprintln!("skipping: the kernel refuses to trace a thread of this test's child: {error}");
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_dir_all(&path);
            return;
        }
        Err(error) => panic!("tracing the child's second thread must work: {error}"),
    };

    let started = std::time::Instant::now();
    let teardown = {
        let dir = path.clone();
        std::thread::spawn(move || {
            drop(ProfileTeardown {
                dir,
                chrome: Some(chrome),
            })
        })
    };
    let held_at_exit = held.stopped_at_exit();
    std::thread::sleep(Duration::from_secs(1));
    let returned_while_held = teardown.is_finished();
    let removed_while_held = !path.exists();
    drop(held);
    let deadline = std::time::Instant::now() + PROCESS_TEST_WAIT;
    while !teardown.is_finished() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let elapsed = started.elapsed();
    let returned = teardown.is_finished();
    let _ = child.kill();
    let _ = child.wait();

    assert!(
        held_at_exit,
        "the teardown must kill the child, which stops the traced thread at its exit"
    );
    assert!(
        !returned_while_held && !removed_while_held,
        "the teardown must wait while a thread of the killed process has not exited: returned {returned_while_held}, removed {removed_while_held}"
    );
    assert!(
        returned && elapsed < PROFILE_USERS_EXIT_TIMEOUT,
        "the teardown must return once the released thread has exited, before its deadline: took {elapsed:?}"
    );
    assert!(
        !path.exists(),
        "the teardown must remove the directory: {}",
        path.display()
    );
}

/// A profile directory dropped outside a Tokio runtime is torn down on another thread, so a host's
/// finalizer thread that drops the last owner is not held for up to the five-second wait.
#[test]
fn a_profile_directory_dropped_outside_a_runtime_is_torn_down_on_another_thread() {
    let dir = ScratchProfileDir::create("crawlberg-no-runtime-test-", None).expect("the directory must be creatable");
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
    let deadline = std::time::Instant::now() + PROCESS_TEST_WAIT;
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
        ScratchProfileDir::create("crawlberg-profile-bystander-test-", None).expect("the directory must be creatable");
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

    let exited = other.try_wait().expect("the status must be readable");
    let _ = other.kill();
    let _ = other.wait();
    assert!(
        exited.is_none(),
        "a process that does not name the profile must not be killed: pid {} ended with {exited:?}",
        other.id()
    );
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
/// ~keep It shares the `process_table` serial key with the test below only: each rewinds
/// ~keep `ns_last_pid`, and two rewinds at once make each other miss the freed pid.
#[cfg(target_os = "linux")]
#[test]
#[serial_test::serial(process_table)]
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
/// ~keep Repeats on a miss like the test above, and shares its `process_table` serial key for the
/// ~keep same reason.
#[cfg(target_os = "linux")]
#[test]
#[serial_test::serial(process_table)]
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
    let dir =
        ScratchProfileDir::create("crawlberg-running-chrome-test-", None).expect("the directory must be creatable");
    let path = dir.path().to_path_buf();
    let config = match build_pool_launch_builder(&path, &BrowserPoolConfig::default())
        .expect("the default pool config names no binary to check")
        .build()
    {
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
    handler: Handler,
    profile: P,
    path: std::path::PathBuf,
) {
    let handler_handle = spawn_handler(handler);

    drop(profile);
    let deadline = tokio::time::Instant::now() + PROCESS_TEST_WAIT;
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
        let dir = ScratchProfileDir::create("crawlberg-launcher-test-", None).expect("the directory must be creatable");
        let path = dir.path().to_path_buf();
        let config = build_pool_launch_builder(&path, &BrowserPoolConfig::default())
            .expect("the default pool config names no binary to check")
            .chrome_executable(&launcher)
            .build()
            .expect("a config naming its executable must build");
        let (mut browser, handler, dir) = match dir.launch(config).await {
            Ok(launched) => launched,
            Err(error) => panic!(
                "{} must launch as {} did: {error}",
                launcher.display(),
                chrome.display()
            ),
        };
        let handler_handle = spawn_handler(handler);
        let mut bystander = spawn_bystander(&user_data_dir_flag(&path));

        drop(dir);
        let deadline = tokio::time::Instant::now() + PROCESS_TEST_WAIT;
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
        let flag = user_data_dir_flag(&path);
        let mut system = sysinfo::System::new();
        let left: Vec<_> = processes_naming(&mut system, &flag)
            .iter()
            .filter_map(|process| Some((process.pid(), process.exe()?.to_path_buf())))
            .collect();
        for (pid, executable) in left {
            kill_if_chrome_using(pid, &flag, &executable);
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
            let panic = error.into_panic();
            let message = panic
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| panic.downcast_ref::<&str>().copied())
                .unwrap_or("the removal check panicked");
            panic!("{launcher}: {message}");
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

/// The profile directory of the Chrome `pool` runs, which must exist and be the one that Chrome
/// uses.
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
    assert!(
        !processes_naming(&mut sysinfo::System::new(), &user_data_dir_flag(&path)).is_empty(),
        "the pool's Chrome must run on the profile directory the pool owns: {}",
        path.display()
    );
    path
}

/// Kill the main process of the Chrome `pool` runs, and return its profile directory.
async fn kill_pool_chrome(pool: &BrowserPool) -> std::path::PathBuf {
    let path = pool_profile_dir(pool).await;
    let mut system = sysinfo::System::new();
    let killed = processes_naming(&mut system, &user_data_dir_flag(&path))
        .into_iter()
        .filter(|process| {
            !process
                .cmd()
                .iter()
                .any(|argument| argument.to_string_lossy().contains("--type="))
        })
        .map(|process| process.kill_with(sysinfo::Signal::Kill).unwrap_or(false))
        .filter(|killed| *killed)
        .count();
    assert_eq!(killed, 1, "the pool's Chrome main process must be killed");
    path
}

/// A pool whose Chrome was killed launches a new one for the next page, and still shuts down.
///
/// ~keep chromiumoxide 0.9.1's handler stays pending after its websocket breaks, so a handler loop
/// ~keep that went on past the error never finished. The pool, which relaunches only a browser
/// ~keep whose handler has ended, kept the dead Chrome: every page request failed and
/// ~keep `shutdown` waited forever (xberg-io/crawlberg#581). Every wait here is bounded, so that
/// ~keep defect fails the test instead of hanging it.
#[tokio::test(flavor = "multi_thread")]
#[allow(clippy::print_stderr, reason = "test-only skip announcement")]
async fn a_pool_whose_chrome_was_killed_relaunches_it_and_shuts_down() {
    let pool = BrowserPool::new(BrowserPoolConfig::default());
    if let Err(error) = pool.warm().await {
        eprintln!("skipping a_pool_whose_chrome_was_killed_relaunches_it_and_shuts_down: no usable Chrome: {error}");
        return;
    }
    kill_pool_chrome(&pool).await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut handler_finished = false;
    while tokio::time::Instant::now() < deadline {
        if pool
            .state
            .lock()
            .await
            .as_ref()
            .is_some_and(|state| state.handler_handle.is_finished())
        {
            handler_finished = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let page = match tokio::time::timeout(Duration::from_secs(60), pool.acquire_page()).await {
        Ok(Ok(page)) => Ok(page),
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => Err("no answer within 60 s".to_string()),
    };
    let page_outcome = match &page {
        Ok(_) => "ok".to_string(),
        Err(error) => error.clone(),
    };
    drop(page);
    let shut_down = tokio::time::timeout(Duration::from_secs(30), pool.shutdown())
        .await
        .is_ok();

    assert!(
        handler_finished && page_outcome == "ok" && shut_down,
        "a pool whose Chrome was killed must end its handler, open a page on a new Chrome and shut \
         down: handler_finished_within_15s={handler_finished} acquire_page={page_outcome} \
         shutdown_within_30s={shut_down}"
    )
}

/// A warm pool whose Chrome was killed and whose handler has ended, with the handler's task held
/// open, and the dead Chrome's profile directory. `None` after a skip line when no Chrome is usable.
///
/// ~keep A handler fails its commands as it drops, and its task is finished only after that. A
/// ~keep page request that meets a dying Chrome runs in that gap, which lasts microseconds. The
/// ~keep pool's test hold keeps the gap open, so a test reaches it by construction and not by
/// ~keep timing (xberg-io/crawlberg#581).
#[allow(clippy::print_stderr, reason = "test-only skip announcement")]
async fn pool_whose_handler_ended_with_its_task_held(
    test_name: &str,
) -> Option<(Arc<BrowserPool>, std::path::PathBuf)> {
    let pool = BrowserPool::new(BrowserPoolConfig::default());
    pool.hold_handler_end.send_replace(true);
    if let Err(error) = pool.warm().await {
        eprintln!("skipping {test_name}: no usable Chrome: {error}");
        return None;
    }
    let old = kill_pool_chrome(&pool).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let (ended, finished) = pool
            .state
            .lock()
            .await
            .as_ref()
            .map(|state| (state.handler_end.has_ended(), state.handler_handle.is_finished()))
            .expect("a warm pool must hold a browser");
        assert!(!finished, "{test_name}: the hold must keep the handler's task open");
        if ended {
            let cause = pool
                .state
                .lock()
                .await
                .as_ref()
                .and_then(|state| state.handler_end.cause().map(str::to_owned));
            assert!(
                cause.as_deref().is_some_and(|cause| !cause.is_empty()),
                "{test_name}: the handler of a killed Chrome must record the websocket error it stopped on"
            );
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{test_name}: the handler of a killed Chrome must end within 15 s"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Some((pool, old))
}

/// The text for a websocket error names the kind of the I/O error under it, and is the error's
/// `Display` alone when there is none.
#[test]
fn the_text_for_a_websocket_error_names_the_io_kind_under_it() {
    #[derive(Debug)]
    struct Outer(std::io::Error);
    impl std::fmt::Display for Outer {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "IO error: {}", self.0)
        }
    }
    impl std::error::Error for Outer {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.0)
        }
    }

    let reset = Outer(std::io::Error::new(
        std::io::ErrorKind::ConnectionReset,
        "reset by peer",
    ));
    assert_eq!(
        websocket_error_text(&reset),
        "IO error: reset by peer (ConnectionReset)"
    );
    assert_eq!(websocket_error_text(&std::fmt::Error), std::fmt::Error.to_string());
}

/// A command Chrome never answers fails with a timeout, on a connection with no other traffic.
///
/// ~keep chromiumoxide 0.9.1 fails such a command only when something polls its handler after
/// ~keep the request timeout, and on a quiet connection nothing did: the command waited forever
/// ~keep (xberg-io/crawlberg#586). The request timeout is 2 s here and the wait is bounded at
/// ~keep 20 s, so that defect fails the test.
#[tokio::test(flavor = "multi_thread")]
#[allow(clippy::print_stderr, reason = "test-only skip announcement")]
async fn a_command_chrome_never_answers_times_out_on_a_quiet_connection() {
    use chromiumoxide::cdp::js_protocol::runtime::EvaluateParams;
    use chromiumoxide::error::CdpError;

    let user_data_dir = ScratchProfileDir::create("crawlberg-chrome-", None).expect("a profile directory");
    let config = build_pool_launch_builder(user_data_dir.path(), &BrowserPoolConfig::default())
        .expect("the launch builder must build")
        .request_timeout(Duration::from_secs(2))
        .build()
        .expect("the launch config must build");
    let (mut browser, handler) = match Browser::launch(config).await {
        Ok(launched) => launched,
        Err(error) => {
            eprintln!(
                "skipping a_command_chrome_never_answers_times_out_on_a_quiet_connection: no usable Chrome: {error}"
            );
            return;
        }
    };
    let handler_task = spawn_handler(handler);
    let page = browser.new_page("about:blank").await.expect("a page must open");
    let never_settles = EvaluateParams::builder()
        .expression("new Promise(() => {})")
        .await_promise(true)
        .build()
        .expect("the evaluate parameters must build");
    let outcome = tokio::time::timeout(Duration::from_secs(20), page.evaluate(never_settles)).await;
    let _ = browser.kill().await;
    handler_task.abort();
    drop(user_data_dir);

    match outcome {
        Ok(Err(CdpError::Timeout)) => {}
        Ok(other) => panic!("a command with no answer must fail with a timeout, got {other:?}"),
        Err(_) => panic!("a command with no answer must fail after the request timeout, not wait 20 s and more"),
    }
}

/// The profile directory of the Chrome `pool` holds now, if it holds one.
async fn pool_profile_path(pool: &BrowserPool) -> Option<std::path::PathBuf> {
    pool.state
        .lock()
        .await
        .as_ref()
        .and_then(|state| state.user_data_dir.as_ref())
        .map(|dir| dir.path().to_path_buf())
}

/// A page opened after the handler ended and before its task finished is opened on a new Chrome.
#[tokio::test(flavor = "multi_thread")]
async fn opening_a_page_replaces_a_dead_chrome_before_its_handler_task_finishes() {
    let test_name = "opening_a_page_replaces_a_dead_chrome_before_its_handler_task_finishes";
    let Some((pool, old)) = pool_whose_handler_ended_with_its_task_held(test_name).await else {
        return;
    };
    let opened = match tokio::time::timeout(Duration::from_secs(60), pool.try_new_page(None, None)).await {
        Ok(Ok((page, _))) => {
            let _ = page.close().await;
            Ok(pool_profile_path(&pool).await)
        }
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => Err("no answer within 60 s".to_string()),
    };
    pool.hold_handler_end.send_replace(false);
    let shut_down = tokio::time::timeout(Duration::from_secs(30), pool.shutdown())
        .await
        .is_ok();

    assert!(
        opened
            .as_ref()
            .is_ok_and(|new| new.as_ref().is_some_and(|new| *new != old))
            && shut_down,
        "the page must open on a new Chrome: opened={opened:?} old={} shutdown_within_30s={shut_down}",
        old.display()
    );
}

/// The relaunch a failed page request asks for replaces a Chrome whose handler ended, before the
/// handler's task finishes.
#[tokio::test(flavor = "multi_thread")]
async fn a_relaunch_replaces_a_dead_chrome_before_its_handler_task_finishes() {
    let test_name = "a_relaunch_replaces_a_dead_chrome_before_its_handler_task_finishes";
    let Some((pool, old)) = pool_whose_handler_ended_with_its_task_held(test_name).await else {
        return;
    };
    let relaunched = match tokio::time::timeout(Duration::from_secs(60), pool.relaunch_browser()).await {
        Ok(Ok(())) => Ok(pool_profile_path(&pool).await),
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => Err("no answer within 60 s".to_string()),
    };
    pool.hold_handler_end.send_replace(false);
    let shut_down = tokio::time::timeout(Duration::from_secs(30), pool.shutdown())
        .await
        .is_ok();

    assert!(
        relaunched
            .as_ref()
            .is_ok_and(|new| new.as_ref().is_some_and(|new| *new != old))
            && shut_down,
        "the relaunch must start a new Chrome: relaunched={relaunched:?} old={} shutdown_within_30s={shut_down}",
        old.display()
    );
}

/// Relaunching a pool connection whose handler ended reconnects to the caller's Chrome without
/// stopping that Chrome.
#[tokio::test(flavor = "multi_thread")]
#[allow(clippy::print_stderr, reason = "test-only skip announcement")]
async fn relaunching_an_external_pool_connection_leaves_the_callers_chrome_running() {
    let executable = match default_executable(DetectionOptions::default()) {
        Ok(executable) => executable,
        Err(error) => {
            eprintln!(
                "skipping relaunching_an_external_pool_connection_leaves_the_callers_chrome_running: \
                 no Chrome executable: {error}"
            );
            return;
        }
    };
    let owner_dir = ScratchProfileDir::create("crawlberg-external-owner-test-", Some(&executable))
        .expect("the external Chrome's profile directory must be created");
    let owner_config = build_pool_launch_builder(owner_dir.path(), &BrowserPoolConfig::default())
        .expect("the default pool configuration must build")
        .chrome_executable(executable)
        .build()
        .expect("the external Chrome configuration must build");
    let (mut owner, owner_handler, owner_dir) = owner_dir
        .launch(owner_config)
        .await
        .expect("the detected Chrome executable must launch");
    let mut owner_task = spawn_handler(owner_handler);
    let pool = BrowserPool::new(BrowserPoolConfig {
        browser_endpoint: Some(owner.websocket_address().clone()),
        ..BrowserPoolConfig::default()
    });
    let operation_timeout = Duration::from_secs(30);
    let mut pool_handlers = Vec::new();
    let exercise: Result<(), String> = async {
        tokio::time::timeout(operation_timeout, pool.warm())
            .await
            .map_err(|_| "the pool warm timed out".to_owned())?
            .map_err(|error| format!("the pool must connect to the external Chrome: {error}"))?;

        {
            let state = pool.state.lock().await;
            let Some(state) = state.as_ref() else {
                return Err("a warm pool must hold a browser connection".to_owned());
            };
            if state.user_data_dir.is_some() {
                return Err("the pool must treat the connected Chrome as external".to_owned());
            }
            let old_handler = state.handler_handle.abort_handle();
            old_handler.abort();
            pool_handlers.push(old_handler);
        }
        tokio::time::timeout(PROCESS_TEST_WAIT, async {
            while !pool
                .state
                .lock()
                .await
                .as_ref()
                .is_some_and(|state| state.handler_end.has_ended())
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| "the aborted pool connection's handler must end".to_owned())?;

        tokio::time::timeout(operation_timeout, pool.relaunch_browser())
            .await
            .map_err(|_| "the pool relaunch timed out".to_owned())?
            .map_err(|error| format!("the pool must reconnect after its handler ends: {error}"))?;
        {
            let state = pool.state.lock().await;
            let Some(state) = state.as_ref() else {
                return Err("a successful relaunch must install new browser state".to_owned());
            };
            pool_handlers.push(state.handler_handle.abort_handle());
            if state.handler_end.has_ended() {
                return Err("the relaunched handler must still be running before page acquisition".to_owned());
            }
        }

        let page = tokio::time::timeout(operation_timeout, pool.acquire_page())
            .await
            .map_err(|_| "page acquisition after relaunch timed out".to_owned())?
            .map_err(|error| format!("the relaunched connection must open a page: {error}"))?;
        tokio::time::timeout(operation_timeout, page.close())
            .await
            .map_err(|_| "closing the relaunched page timed out".to_owned())?;
        tokio::time::timeout(operation_timeout, pool.shutdown())
            .await
            .map_err(|_| "pool shutdown after relaunch timed out".to_owned())?;
        tokio::time::timeout(Duration::from_secs(5), owner.version())
            .await
            .map_err(|_| "the caller's Chrome version request timed out".to_owned())?
            .map_err(|error| format!("the caller's Chrome must answer after pool shutdown: {error}"))?;
        Ok(())
    }
    .await;

    let cleanup_shutdown = tokio::time::timeout(operation_timeout, pool.shutdown()).await;
    for handler in &pool_handlers {
        handler.abort();
    }
    let pool_handlers_stopped = tokio::time::timeout(Duration::from_secs(5), async {
        while pool_handlers.iter().any(|handler| !handler.is_finished()) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    owner_task.abort();
    let owner_handler_stopped = tokio::time::timeout(Duration::from_secs(5), &mut owner_task).await;
    let owner_killed = tokio::time::timeout(Duration::from_secs(5), owner.kill()).await;
    drop(owner_dir);

    assert!(exercise.is_ok(), "{}", exercise.expect_err("checked above"));
    assert!(cleanup_shutdown.is_ok(), "cleanup pool shutdown must not time out");
    assert!(
        pool_handlers_stopped.is_ok(),
        "every pool handler must stop during cleanup"
    );
    assert!(
        matches!(&owner_killed, Ok(Some(Ok(())))),
        "stopping the test-owned Chrome must succeed: {owner_killed:?}"
    );
    assert!(
        matches!(&owner_handler_stopped, Ok(Err(error)) if error.is_cancelled()),
        "the aborted test-owned Chrome handler must report cancellation: {owner_handler_stopped:?}"
    );
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
    let deadline = tokio::time::Instant::now() + PROCESS_TEST_WAIT;
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
    let dir =
        ScratchProfileDir::create("crawlberg-failed-launch-test-", None).expect("the directory must be creatable");
    let path = dir.path().to_path_buf();
    let config = build_pool_launch_builder(&path, &BrowserPoolConfig::default())
        .expect("the default pool config names no binary to check")
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

    let user_data_dir =
        ScratchProfileDir::create("crawlberg-pool-test-", None).expect("a profile directory must be created");
    let browser_config = match build_pool_launch_builder(user_data_dir.path(), &BrowserPoolConfig::default())
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
    let (mut browser, handler) = match Browser::launch(browser_config).await {
        Ok(pair) => pair,
        Err(error) => {
            eprintln!(
                "skipping close_browser_within_returns_promptly_when_the_process_is_stopped \
                 because no usable Chrome was found: {error}"
            );
            return;
        }
    };
    let handler_task = spawn_handler(handler);

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
    let still_running = browser
        .try_wait()
        .expect("the browser's status must be readable")
        .is_none();

    // ~keep Always sent, even if the assertions below fail: a stopped process left behind
    // ~keep by a broken implementation would otherwise leak past this test. Sent through the
    // ~keep browser's own handle, which signals nothing once the process is reaped: a kill by pid
    // ~keep after the reap reaches whatever process has the pid now.
    let _ = browser.kill().await;
    handler_task.abort();

    assert!(
        elapsed < Duration::from_secs(5),
        "close_browser_within must return near its configured budget ({shutdown_timeout:?}) \
         even when close()/wait() cannot make progress on a stopped process; took {elapsed:?}"
    );

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
    let user_data_dir =
        ScratchProfileDir::create("crawlberg-pool-test-", None).expect("a profile directory must be created");
    let launched = match build_pool_launch_builder(user_data_dir.path(), &BrowserPoolConfig::default())
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
    // ~keep Kept as the plain loop, which a killed Chrome never ends: the release must not wait on
    // ~keep it (#146). A loop that ends at the websocket error would hide a release that waits again.
    let handler_task = tokio::spawn(async move { while handler.next().await.is_some() {} });
    // ~keep Bounded here: the plain loop never fails this command if the Chrome dies first.
    let page = tokio::time::timeout(PROCESS_TEST_WAIT, browser.new_page("about:blank"))
        .await
        .expect("a launched Chrome must answer the request for a tab within the wait")
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
    // ~keep No kill by pid follows: the release owns the process and chromiumoxide's handle kills
    // ~keep it on drop, and once it is reaped the pid can name another process.
    let died_after = died_after.join().expect("the watcher thread must not panic");

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
    let user_data_dir =
        ScratchProfileDir::create("crawlberg-pool-test-", None).expect("a profile directory must be created");
    let launched = match build_pool_launch_builder(user_data_dir.path(), &BrowserPoolConfig::default())
        .expect("the default pool config names no binary to check")
        .build()
    {
        Ok(config) => Browser::launch(config).await.map_err(|error| error.to_string()),
        Err(error) => Err(error),
    };
    let (mut owner, owner_handler) = match launched {
        Ok(pair) => pair,
        Err(error) => {
            eprintln!(
                "skipping release_browser_disconnects_from_a_connected_browser_without_closing_it \
                 because no usable Chrome was found: {error}"
            );
            return;
        }
    };
    let owner_task = spawn_handler(owner_handler);

    let (connected, mut handler) = Browser::connect(owner.websocket_address().clone())
        .await
        .expect("connecting to the launched Chrome must succeed");
    // ~keep Kept as the plain loop: the assertion reads this task's end as the release's abort, and
    // ~keep a loop that ends at a websocket error would also end if the release closed the Chrome.
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
        Ok((browser, handler)) => Some((browser, spawn_handler(handler))),
        Err(error) => {
            eprintln!("skipping {test_name} because no usable Chrome was found: {error}");
            None
        }
    }
}

/// How many live processes have `text` on their command line.
fn processes_containing(text: &str) -> usize {
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
    let before = processes_containing(&text);
    kill_browser(browser, task, profile.clone(), Duration::from_secs(5)).await;
    let after = processes_containing(&text);
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

/// How many members of `family` are neither stopped nor zombies.
fn members_running(family: &ChromeFamily) -> usize {
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
}

/// Start `script` in a shell, with `marker` as its `$0`.
///
/// ~keep Every script of these tests starts its commands in the background and waits with the
/// ~keep `wait` builtin. A shell such as dash starts a foreground command with `vfork`, and a
/// ~keep shell inside `vfork` cannot take a stop while its child is stopped before the child
/// ~keep starts its program: it reads as running, and 1 collection in 1054 under load ended
/// ~keep with such a shell. A background command is started with `fork`.
fn start_family(script: &str, marker: &str) -> std::process::Child {
    std::process::Command::new("sh")
        .arg("-c")
        .arg(script)
        .arg(marker)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("sh must start")
}

/// A shell with two children. None of them forks, so a test decides which member runs.
fn start_idle_family() -> std::process::Child {
    start_family("sleep 60 & sleep 60 & wait", "crawlberg-idle-family")
}

/// Continue the stopped process `pid`, and wait for at most five seconds until it reads as
/// running. Returns whether it does.
fn resume(pid: Pid) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let mut system = System::new();
        ChromeFamily::refresh(&mut system, ProcessesToUpdate::Some(&[pid]));
        let Some(process) = system.process(pid) else {
            return false;
        };
        if process.status() != ProcessStatus::Stop {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        process.kill_with(Signal::Continue);
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// How many live children the process `parent` has.
fn children_of(parent: Pid) -> usize {
    let mut system = System::new();
    ChromeFamily::refresh(&mut system, ProcessesToUpdate::All);
    system
        .processes()
        .values()
        .filter(|process| process.parent() == Some(parent) && process.status() != ProcessStatus::Zombie)
        .count()
}

/// Whether a captured log holds a line that contains `text`.
fn logged(fields: &[(String, String)], text: &str) -> bool {
    fields.iter().any(|(_, value)| value.contains(text))
}

/// The line that is logged for a member that did not take its stop.
const UNSTOPPED: &str = "did not take its stop in two waits";
/// The line that is logged for a family whose walks ran out.
const UNSETTLED: &str = "did not come to rest";

/// A family that keeps forking is stopped as it is found, and every member has taken its stop when
/// the collection returns, so a child forked while the family is collected cannot slip past the
/// kill.
#[test]
#[serial_test::serial(chrome_family_log)]
fn a_forking_family_is_stopped_as_it_is_found_and_leaves_no_child_behind() {
    let test_name = "a_forking_family_is_stopped_as_it_is_found_and_leaves_no_child_behind";
    if !cfg!(unix) {
        return;
    }
    let marker = format!("crawlberg-family-test-{}", std::process::id());
    let mut parent = start_family(
        "while :; do sh -c 'sleep 2 & wait' \"$0\" & sleep 0.01 & wait $!; done",
        &marker,
    );
    std::thread::sleep(Duration::from_millis(200));

    let family = ChromeFamily::freeze(parent.id());
    let running = members_running(&family);
    family.kill();
    let gone = family.wait(Duration::from_secs(5));
    let _ = parent.wait();
    std::thread::sleep(Duration::from_millis(100));
    let survivors = processes_containing(&marker);

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

/// A family at rest is collected without a warning: every member reads as stopped in the first
/// wait.
///
/// ~keep This holds the read of the status into a new `System`. With a kept `System`, sysinfo on
/// ~keep macOS never shows a stop, every wait runs out, and the collection ends with the warning
/// ~keep for a member that did not take its stop. Linux reads the status on every refresh, so
/// ~keep only the macOS run of this test can fail for that.
#[test]
#[serial_test::serial(chrome_family_log)]
fn a_family_at_rest_is_collected_without_a_warning() {
    let test_name = "a_family_at_rest_is_collected_without_a_warning";
    if !cfg!(unix) {
        return;
    }
    let mut parent = start_idle_family();
    std::thread::sleep(Duration::from_millis(200));

    let (family, fields) = crate::tracing_capture::capture_events(|| ChromeFamily::freeze(parent.id()));
    let running = members_running(&family);
    family.kill();
    let gone = family.wait(Duration::from_secs(5));
    let _ = parent.wait();

    assert!(
        family.members.len() > 2,
        "{test_name}: the family must hold the shell and its children, got {}",
        family.members.len()
    );
    assert!(
        fields.is_empty(),
        "{test_name}: a family at rest must be collected without a log line, got {fields:?}"
    );
    assert_eq!(
        running, 0,
        "{test_name}: every member must have its stop, {running} still ran"
    );
    assert!(gone, "{test_name}: every member must be gone within the limit");
}

/// A member that runs again after the family was seen at rest is stopped again and stays in the
/// list the collection waits for, and a walk taken while a member ran is not the last one: every
/// member has its stop when the collection returns.
///
/// ~keep On macOS a stop sent to a process that is starting a program does not always hold
/// ~keep (xberg-io/crawlberg#585). The test makes that happen on every Unix: it continues one
/// ~keep member right after each of the first two waits that have a member to wait for.
#[test]
#[serial_test::serial(chrome_family_log)]
fn a_member_that_runs_again_after_its_stop_is_stopped_again() {
    let test_name = "a_member_that_runs_again_after_its_stop_is_stopped_again";
    if !cfg!(unix) {
        return;
    }
    let mut parent = start_idle_family();
    std::thread::sleep(Duration::from_millis(200));

    let mut resumed: Vec<Pid> = Vec::new();
    let mut waited_for_resumed = true;
    let mut waits = 0;
    let family = ChromeFamily::freeze_with(parent.id(), |stopping| {
        waits += 1;
        waited_for_resumed &= resumed.iter().all(|member| stopping.contains(member));
        let stopped = ChromeFamily::settle(stopping);
        if let (true, Some(member)) = (resumed.len() < 2, stopping.last())
            && resume(*member)
        {
            resumed.push(*member);
        }
        stopped
    });
    let running = members_running(&family);
    family.kill();
    let gone = family.wait(Duration::from_secs(5));
    let _ = parent.wait();

    assert!(
        family.members.len() > 2,
        "{test_name}: the family must hold the shell and its children, got {}",
        family.members.len()
    );
    assert!(
        waited_for_resumed,
        "{test_name}: every wait after a member ran again must be for that member too"
    );
    assert!(
        waits > 3,
        "{test_name}: the collection must wait and walk once more each time a member ran, got {waits} waits"
    );
    assert_eq!(
        resumed.len(),
        2,
        "{test_name}: the test must have made a stopped member run again twice"
    );
    assert_eq!(
        running, 0,
        "{test_name}: a member that ran again must have its stop when the collection returns, {running} still ran"
    );
    assert!(gone, "{test_name}: every member must be gone within the limit");
}

/// A walk taken after one wait that ran out is not the last one, because a member that still ran
/// during it can have forked a child it did not see. The collection waits and walks once more.
#[test]
#[serial_test::serial(chrome_family_log)]
fn a_walk_after_a_wait_that_ran_out_is_not_the_last_one() {
    let test_name = "a_walk_after_a_wait_that_ran_out_is_not_the_last_one";
    if !cfg!(unix) {
        return;
    }
    let mut parent = start_idle_family();
    std::thread::sleep(Duration::from_millis(200));

    // ~keep The first wait has no member to wait for. The second one waits for the whole family
    // ~keep and is then reported as run out.
    let mut waits = 0;
    let (family, fields) = crate::tracing_capture::capture_events(|| {
        ChromeFamily::freeze_with(parent.id(), |stopping| {
            waits += 1;
            ChromeFamily::settle(stopping) && waits != 2
        })
    });
    let running = members_running(&family);
    family.kill();
    let gone = family.wait(Duration::from_secs(5));
    let _ = parent.wait();

    assert!(
        family.members.len() > 2,
        "{test_name}: the family must hold the shell and its children, got {}",
        family.members.len()
    );
    assert!(
        waits > 2,
        "{test_name}: the collection must wait and walk once more after the wait that ran out, got {waits} waits"
    );
    assert!(
        fields.is_empty(),
        "{test_name}: one wait that ran out must not end the collection with a warning, got {fields:?}"
    );
    assert_eq!(
        running, 0,
        "{test_name}: every member must have its stop, {running} still ran"
    );
    assert!(gone, "{test_name}: every member must be gone within the limit");
}

/// A member that does not take its stop does not hold the collection for every walk: after two
/// waits in a row that ran out, with no new member in the walk after each, the collection ends
/// and logs the member.
///
/// ~keep A process inside `vfork` on Linux is such a member. The test reports every wait as
/// ~keep run out, which is what the collection sees of it.
#[test]
#[serial_test::serial(chrome_family_log)]
fn a_member_that_does_not_take_its_stop_ends_the_collection_after_two_waits() {
    let test_name = "a_member_that_does_not_take_its_stop_ends_the_collection_after_two_waits";
    if !cfg!(unix) {
        return;
    }
    let mut parent = start_idle_family();
    std::thread::sleep(Duration::from_millis(200));

    let mut waits = 0;
    let (family, fields) = crate::tracing_capture::capture_events(|| {
        ChromeFamily::freeze_with(parent.id(), |_| {
            waits += 1;
            false
        })
    });
    family.kill();
    let gone = family.wait(Duration::from_secs(5));
    let _ = parent.wait();

    assert!(
        family.members.len() > 2,
        "{test_name}: the family must hold the shell and its children, got {}",
        family.members.len()
    );
    assert_eq!(
        waits, 3,
        "{test_name}: the collection must end after the first wait, which has no member, and two waits that ran out"
    );
    assert!(
        logged(&fields, UNSTOPPED) && !logged(&fields, UNSETTLED),
        "{test_name}: the collection must log the member that did not take its stop, got {fields:?}"
    );
    assert!(gone, "{test_name}: every member must be gone within the limit");
}

/// A family in which a member runs and forks before every walk is never taken as at rest, and not
/// as a member that cannot take its stop either: the collection uses every walk and logs that the
/// family did not come to rest.
///
/// ~keep The wait of the test continues every member, waits until the shell has one more child,
/// ~keep and reports that the wait ran out, so every walk finds a new member.
#[test]
#[serial_test::serial(chrome_family_log)]
fn a_family_that_forks_before_every_walk_uses_every_walk_and_is_logged() {
    let test_name = "a_family_that_forks_before_every_walk_uses_every_walk_and_is_logged";
    if !cfg!(unix) {
        return;
    }
    let mut parent = start_family(
        "while :; do sleep 5 & sleep 0.05 & wait $!; done",
        "crawlberg-forking-family",
    );
    let shell = Pid::from_u32(parent.id());
    std::thread::sleep(Duration::from_millis(200));

    let mut waits = 0;
    let mut forked = true;
    let (family, fields) = crate::tracing_capture::capture_events(|| {
        ChromeFamily::freeze_with(parent.id(), |stopping| {
            waits += 1;
            if !stopping.is_empty() {
                let before = children_of(shell);
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                let mut grew = false;
                while !grew && std::time::Instant::now() < deadline {
                    for member in stopping {
                        resume(*member);
                    }
                    std::thread::sleep(Duration::from_millis(5));
                    grew = children_of(shell) > before;
                }
                forked &= grew;
            }
            false
        })
    });
    family.kill();
    let gone = family.wait(Duration::from_secs(5));
    let _ = parent.wait();

    assert!(forked, "{test_name}: the shell must have forked before every walk");
    assert_eq!(
        waits,
        ChromeFamily::WALKS,
        "{test_name}: a family that forks before every walk must use every walk"
    );
    assert!(
        logged(&fields, UNSETTLED) && !logged(&fields, UNSTOPPED),
        "{test_name}: the collection must log that the family did not come to rest, got {fields:?}"
    );
    assert!(gone, "{test_name}: every member must be gone within the limit");
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
    let before = processes_containing(&marker);
    kill_browser(browser, task, profile.clone(), Duration::from_secs(5)).await;
    let after = processes_containing(&marker);
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
        let settled = ChromeFamily::settle(&members);
        let _ = report.send((started.elapsed(), settled));
    });
    let waited = waited.recv_timeout(ChromeFamily::SETTLE * 5);
    let started = std::time::Instant::now();
    let settled = ChromeFamily::settle(&members[..1]);
    let settled_in = started.elapsed();
    let _ = stopped.kill();
    let _ = stopped.wait();
    let _ = running.kill();
    let _ = running.wait();

    assert!(sent, "{test_name}: the stop must be sent");
    assert!(
        matches!(waited, Ok((waited, false)) if waited >= ChromeFamily::SETTLE),
        "{test_name}: the wait must last until its bound while a member has not taken its stop, and say that it ran out, got {waited:?}"
    );
    assert!(
        settled && settled_in < ChromeFamily::SETTLE,
        "{test_name}: the wait must end once every member has taken its stop, and say so: it took {settled_in:?} and returned {settled}"
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
    let (mut owner, owner_handler) = match launched {
        Ok(pair) => pair,
        Err(error) => {
            eprintln!(
                "skipping kill_browser_releases_a_browser_it_cannot_kill_and_removes_the_profile \
                 because no usable Chrome was found: {error}"
            );
            return;
        }
    };
    let owner_task = spawn_handler(owner_handler);
    let (connected, mut handler) = Browser::connect(owner.websocket_address().clone())
        .await
        .expect("connecting to the launched Chrome must succeed");
    // ~keep Kept as the plain loop: the assertion reads this task's end as the release's abort, and
    // ~keep a loop that ends at a websocket error would also end if the release closed the Chrome.
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
    let (mut browser, handler) = match launched {
        Ok(pair) => pair,
        Err(error) => {
            eprintln!("skipping a_launched_browser_opens_no_startup_tab: no usable Chrome: {error}");
            return;
        }
    };
    let handler_task = spawn_handler(handler);
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

/// The pool is the one launch path a caller reaches without going through `CrawlConfig::validate`,
/// so a refused `chrome_path` must remove the scratch directory the guard created for the launch it
/// never made, not only fail with the right message.
///
/// ~keep Pins the call site in `launch_browser`: `ScratchProfileDir::create` then
/// ~keep `build_pool_launch_builder(..)?`, whose `?` drops the guard on a refusal. A future call
/// ~keep site that reorders this (or forgets the `?`) leaks the directory and every shipped test
/// ~keep before this one still passes, because none of them checked for the directory.
#[tokio::test]
async fn a_pool_refused_by_a_missing_chrome_path_leaves_no_profile_directory() {
    let pool = BrowserPool::new(BrowserPoolConfig {
        chrome_path: Some(std::path::PathBuf::from("/nonexistent/crawlberg-pool-chrome")),
        ..BrowserPoolConfig::default()
    });
    let error = assert_refused_launch_leaves_no_scratch_dir(|| pool.acquire_page()).await;
    assert!(
        error.contains("BrowserPoolConfig.chrome_path '/nonexistent/crawlberg-pool-chrome' cannot be used"),
        "the error must name the pool key and the path, got: {error}"
    );
}

/// The same call site refused by a `chrome_args` entry instead of `chrome_path`: `--user-data-dir`
/// is a flag the launch itself sets, so `CrawlConfig::validate`'s own rule refuses it here too.
#[tokio::test]
async fn a_pool_refused_by_a_user_data_dir_flag_leaves_no_profile_directory() {
    let pool = BrowserPool::new(BrowserPoolConfig {
        chrome_args: vec!["--user-data-dir=/tmp/crawlberg-pool-elsewhere".to_owned()],
        ..BrowserPoolConfig::default()
    });
    let error = assert_refused_launch_leaves_no_scratch_dir(|| pool.acquire_page()).await;
    assert!(
        error.contains("must not set --user-data-dir"),
        "the error must name the refused flag, got: {error}"
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

#[test]
fn the_pool_config_debug_hides_a_credential_in_a_chrome_flag() {
    let config = BrowserPoolConfig {
        chrome_args: vec![
            "--proxy-server=http://operator:IMPL385-CA-PW@proxy.test:8080".to_owned(),
            "--proxy-server=operator:IMPL385-CA-BARE@proxy.test:8080".to_owned(),
            "--disable-gpu".to_owned(),
        ],
        ..BrowserPoolConfig::default()
    };
    let shown = format!("{config:?}");
    assert!(
        !shown.contains("IMPL385-CA"),
        "a chrome flag shows its credential: {shown}"
    );
    assert!(
        shown.contains("--proxy-server") && shown.contains("--disable-gpu"),
        "positive twin: the flag names are shown: {shown}"
    );
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

fn preferences(dir: &std::path::Path) -> serde_json::Value {
    let bytes = std::fs::read(dir.join("Default").join("Preferences")).expect("the preference file must exist");
    serde_json::from_slice(&bytes).expect("the preference file must be JSON")
}

#[test]
fn the_webrtc_preference_is_written_into_a_new_profile() {
    let dir = tempfile::tempdir().expect("a temp profile directory");
    disable_non_proxied_udp(dir.path()).expect("the preference must be written");
    assert_eq!(
        preferences(dir.path()),
        serde_json::json!({ "webrtc": { "ip_handling_policy": "disable_non_proxied_udp" } })
    );
}

#[test]
fn the_webrtc_preference_keeps_what_a_profile_already_holds() {
    let dir = tempfile::tempdir().expect("a temp profile directory");
    std::fs::create_dir_all(dir.path().join("Default")).expect("the profile directory must be made");
    std::fs::write(
        dir.path().join("Default").join("Preferences"),
        r#"{"profile":{"name":"kept"},"webrtc":{"multiple_routes_enabled":false,"ip_handling_policy":"default"}}"#,
    )
    .expect("the preference file must be written");
    disable_non_proxied_udp(dir.path()).expect("the preference must be merged");
    assert_eq!(
        preferences(dir.path()),
        serde_json::json!({
            "profile": { "name": "kept" },
            "webrtc": { "multiple_routes_enabled": false, "ip_handling_policy": "disable_non_proxied_udp" }
        })
    );
}

#[test]
fn a_preference_file_that_is_not_a_json_object_is_refused_and_left_alone() {
    for content in ["{\"webrtc\": ", "[1]", "{\"webrtc\": 1}"] {
        let dir = tempfile::tempdir().expect("a temp profile directory");
        let path = dir.path().join("Default").join("Preferences");
        std::fs::create_dir_all(dir.path().join("Default")).expect("the profile directory must be made");
        std::fs::write(&path, content).expect("the preference file must be written");
        let error = disable_non_proxied_udp(dir.path()).expect_err("a broken preference file must be refused");
        assert!(
            error.to_string().contains("WebRTC preference"),
            "the error must name the WebRTC preference for {content:?}, got: {error}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("the file must still be there"),
            content,
            "a refused preference file must be left as it was"
        );
    }
}

/// The DevTools websocket address chromiumoxide reads from a launched Chrome's stderr.
const WEBSOCKET: &str = "ws://127.0.0.1:41235/devtools/browser/5c1e0f6a-6a43-4f4a-9d0b-2b7b0f0e3a11";

#[test]
fn a_profile_chrome_wrote_its_devtools_port_into_is_confirmed() {
    let dir = tempfile::tempdir().expect("a temp profile directory");
    std::fs::write(
        dir.path().join("DevToolsActivePort"),
        "41235\n/devtools/browser/5c1e0f6a-6a43-4f4a-9d0b-2b7b0f0e3a11",
    )
    .expect("the port file must be written");
    assert!(wrote_devtools_port(dir.path(), WEBSOCKET));
}

#[test]
fn a_profile_chrome_did_not_write_is_refused() {
    let dir = tempfile::tempdir().expect("a temp profile directory");
    disable_non_proxied_udp(dir.path()).expect("the preference must be written");
    assert!(
        !wrote_devtools_port(dir.path(), WEBSOCKET),
        "a profile with no DevToolsActivePort was not opened by the launched Chrome"
    );
    for stale in [
        "41236\n/devtools/browser/5c1e0f6a-6a43-4f4a-9d0b-2b7b0f0e3a11",
        "41235\n/devtools/browser/00000000-6a43-4f4a-9d0b-2b7b0f0e3a11",
        "41235",
        "",
    ] {
        std::fs::write(dir.path().join("DevToolsActivePort"), stale).expect("the port file must be written");
        assert!(
            !wrote_devtools_port(dir.path(), WEBSOCKET),
            "a port file another Chrome wrote must be refused: {stale:?}"
        );
    }
}

#[test]
fn a_snap_chrome_gets_its_scratch_profile_in_the_snaps_common_directory() {
    let home = std::path::Path::new("/home/runner");
    let common = home.join("snap").join("chromium").join("common");
    for executable in [
        "/snap/bin/chromium",
        "/snap/bin/chromium.chromedriver",
        "/snap/chromium/current/usr/lib/chromium-browser/chrome",
    ] {
        assert_eq!(
            snap_common_dir(std::path::Path::new(executable), home),
            Some(common.clone()),
            "{executable} runs the chromium snap"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_link_to_a_snap_chrome_gets_the_snaps_common_directory() {
    let home = std::path::Path::new("/home/runner");
    let dir = tempfile::tempdir().expect("a temp directory");
    // ~keep The second target is the link setup-chrome made on CI, whose binary does not exist.
    for (index, target) in ["/snap/bin/chromium", "/snap/chromium/current/usr/bin/chromium"]
        .into_iter()
        .enumerate()
    {
        let link = dir.path().join(format!("chromium-{index}"));
        std::os::unix::fs::symlink(target, &link).expect("the link must be made");
        assert_eq!(
            snap_common_dir(&link, home),
            Some(home.join("snap").join("chromium").join("common")),
            "a link to {target} runs the chromium snap"
        );
    }
}

#[test]
fn a_chrome_that_is_not_a_snap_gets_its_scratch_profile_in_the_temp_directory() {
    let chrome = crate::types::executable_temp_file("not-a-snap");
    assert_eq!(snap_common_dir(&chrome, std::path::Path::new("/home/runner")), None);
    let dir = ScratchProfileDir::create("crawlberg-not-a-snap-test-", Some(&chrome))
        .expect("the directory must be creatable");
    let _ = std::fs::remove_file(&chrome);
    assert!(
        dir.path().starts_with(std::env::temp_dir()),
        "a scratch profile for a Chrome that is not a snap must be in the temp directory: {}",
        dir.path().display()
    );
}
