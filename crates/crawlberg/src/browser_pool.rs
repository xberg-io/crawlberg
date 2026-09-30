//! Browser pool for managing a persistent Chrome instance with bounded concurrency.
//!
//! ~keep This module is feature-gated behind `#[cfg(feature = "browser-chromiumoxide")]` at
//! ~keep the module level in `lib.rs` (the narrower flag -- `browser` implies it, see the
//! ~keep `~keep` there). Two methods compiled under this module, `PooledPage::into_parts` and
//! ~keep `BrowserPool::firewall`, are only called from code gated on the wider `browser`
//! ~keep feature, so each carries its own `#[cfg(feature = "browser")]` inline; those are the
//! ~keep sanctioned in-file feature gates, not a precedent for adding more.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use chromiumoxide::Handler;
use chromiumoxide::browser::{Browser, BrowserConfig, BrowserConfigBuilder};
use chromiumoxide::cdp::browser_protocol::target::{CloseTargetParams, TargetId};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinHandle;
use tokio_stream::StreamExt;

use crate::chrome_args::chrome_arg_key;
use crate::error::CrawlError;
use crate::ssrf_intercept::{BrowserFirewall, BrowserOrigin, PageContext};

/// Timeout for opening a new page (tab) in Chrome.
const PAGE_OPEN_TIMEOUT: Duration = Duration::from_secs(5);

/// Timeout for waiting on the CDP handler task during shutdown.
const HANDLER_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// Puppeteer-derived default args, filtered for snap chromium compatibility.
///
/// The Ubuntu snap chromium wrapper on linux-arm64 rejects flags that regular
/// chromium accepts:
/// - `--disable-background-networking` → "unknown command"
/// - `--enable-features=NetworkService,NetworkServiceInProcess` → "unknown command"
/// - `--disable-background-timer-throttling` → "unknown flag"
/// - `--metrics-recording-only` → "unknown command"
///
/// We detect snap chromium at runtime and return a filtered set when detected.
///
/// ~keep This is the single source of every launch path's default Chrome flags. None of
/// ~keep `browser.rs`, `browser_pool.rs`, `interact/chromiumoxide.rs` calls this function
/// ~keep directly or keeps its own copy of the list; each calls [`apply_default_args`],
/// ~keep which calls this. The returned strings are already run through `chrome_arg_key`,
/// ~keep so every caller can hand them straight to chromiumoxide's `BrowserConfig::arg`
/// ~keep without re-stripping the `--`. Normalizing here, once, means a fourth launch path
/// ~keep can't reintroduce the double-dash bug (see `chrome_args.rs`) by forgetting to
/// ~keep call `chrome_arg_key` itself.
/// ~keep Caller-supplied `chrome_args` config entries do not come through this function;
/// ~keep [`apply_launch_overrides`] normalizes them for every launch path.
pub(crate) fn safe_default_args() -> Vec<&'static str> {
    let mut all_args = vec![
        "--disable-background-networking",
        "--enable-features=NetworkService,NetworkServiceInProcess",
        "--disable-background-timer-throttling",
        "--disable-backgrounding-occluded-windows",
        "--disable-breakpad",
        "--disable-client-side-phishing-detection",
        "--disable-component-extensions-with-background-pages",
        "--disable-default-apps",
        "--disable-dev-shm-usage",
        "--disable-features=TranslateUI",
        "--disable-hang-monitor",
        "--disable-ipc-flooding-protection",
        "--disable-popup-blocking",
        "--disable-prompt-on-repost",
        "--disable-renderer-backgrounding",
        "--disable-sync",
        "--force-color-profile=srgb",
        "--metrics-recording-only",
        "--no-first-run",
        // ~keep No startup tab: Chrome's new-tab page fetches remote content that belongs to no
        // ~keep watched page, so the SSRF check refuses it. Pages are created on demand.
        "--no-startup-window",
        "--password-store=basic",
        "--lang=en_US",
    ];

    // ~keep macOS shows a blocking "wants to use your confidential information stored in
    // ~keep Chrome Safe Storage" prompt unless told to use a mock keychain instead.
    // ~keep `--use-mock-keychain` (Chromium's `kUseMockKeychain`) is defined and consumed only
    // ~keep in the macOS os_crypt backend that reads the login keychain; on Linux and Windows
    // ~keep Chrome's os_crypt backend never looks at this switch, so passing it there is a
    // ~keep no-op, not a behavior change. Gate it here anyway rather than relying on that
    // ~keep upstream no-op, so this list documents its own platform scope.
    if cfg!(target_os = "macos") {
        all_args.push("--use-mock-keychain");
    }

    let is_snap = std::path::Path::new("/snap/chromium/current/usr/bin/chromium").exists();

    let filtered: Vec<&'static str> = if is_snap {
        all_args
            .into_iter()
            .filter(|&arg| {
                !matches!(
                    arg,
                    "--disable-background-networking"
                        | "--enable-features=NetworkService,NetworkServiceInProcess"
                        | "--disable-background-timer-throttling"
                        | "--metrics-recording-only"
                )
            })
            .collect()
    } else {
        all_args
    };

    filtered.into_iter().map(chrome_arg_key).collect()
}

/// Push every entry of [`safe_default_args`] onto `builder`, except a default whose switch
/// name one of the caller's `chrome_args` also names: the caller's flag replaces it.
///
/// ~keep All three launch paths (`browser.rs`, `browser_pool.rs`,
/// ~keep `interact/chromiumoxide.rs`) call this instead of looping over
/// ~keep `safe_default_args()` themselves, so the loop that hands flags to
/// ~keep chromiumoxide exists exactly once. A fourth launch path gets the fix
/// ~keep by calling this function; it cannot reintroduce the double-dash bug by
/// ~keep writing its own loop and forgetting to normalize.
/// ~keep Replacing rather than appending is the only way to make the caller's value win:
/// ~keep chromiumoxide keeps launch flags in a HashMap, so two values for one switch reach
/// ~keep Chrome in no fixed order.
pub(crate) fn apply_default_args(mut builder: BrowserConfigBuilder, chrome_args: &[String]) -> BrowserConfigBuilder {
    for arg in safe_default_args() {
        if !caller_sets_switch(chrome_args, crate::types::chrome_switch_name(arg)) {
            builder = builder.arg(arg);
        }
    }
    builder
}

/// Whether one of the caller's `chrome_args` names the Chrome switch `name`, byte-exact.
///
/// ~keep Exact comparison is sound because `check_chrome_args` refuses a name with an
/// ~keep uppercase letter before any launch.
pub(crate) fn caller_sets_switch(chrome_args: &[String], name: &str) -> bool {
    chrome_args
        .iter()
        .any(|arg| crate::types::chrome_switch_name(arg) == name)
}

/// Point `builder` at the caller's Chrome binary, if one is named, and add the caller's
/// extra flags. [`apply_default_args`] has already left out any default they replace.
///
/// A named binary that is missing or not executable is an error naming the path, never a
/// fallback to chromiumoxide's own detection. `chrome_args` that `CrawlConfig::validate` would
/// refuse are an error here too, for the pool, whose config never passes through `validate`.
/// `section` names the config the options came from (`browser` or `BrowserPoolConfig`), and
/// the error names the key in it.
pub(crate) fn apply_launch_overrides(
    mut builder: BrowserConfigBuilder,
    section: &str,
    chrome_path: Option<&std::path::Path>,
    chrome_args: &[String],
) -> Result<BrowserConfigBuilder, CrawlError> {
    crate::types::check_chrome_args(section, chrome_args).map_err(CrawlError::browser_error)?;
    if let Some(path) = chrome_path {
        crate::types::check_chrome_executable(section, path).map_err(CrawlError::browser_error)?;
        builder = builder.chrome_executable(path);
    }
    for arg in chrome_args {
        builder = builder.arg(chrome_arg_key(arg.as_str()));
    }
    Ok(builder)
}

/// How long a [`ScratchProfileDir`]'s teardown waits for the processes it killed to exit.
const PROFILE_USERS_EXIT_TIMEOUT: Duration = Duration::from_secs(5);
const PROFILE_USERS_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// A Chrome `--user-data-dir` in the system temp directory, removed when dropped.
///
/// ~keep The removal first kills every Chrome process still using the directory. Chrome's
/// ~keep helper processes (renderers, the GPU process, the network and storage services) outlive
/// ~keep the browser process for a moment and keep writing into the directory, so a removal made
/// ~keep while they run fails part-way or is undone, which left a profile behind in 6 of 20 drops
/// ~keep on Linux (xberg-io/crawlberg#415). chromiumoxide 0.9.1 starts Chrome in the caller's
/// ~keep process group and kills only the browser process, which orphans the helpers: their
/// ~keep parent is then init or a subreaper, so no parent pid or process group leads back to the
/// ~keep browser. They are found by the `--user-data-dir` flag that Chrome passes to each of them,
/// ~keep which still names them after the browser process is gone, and by the executable of the
/// ~keep Chrome that [`Self::launch`] started, which each of them runs.
/// ~keep The drop hands that work to another thread and returns at once: the scan, the kills, the
/// ~keep wait of up to five seconds and the delete ran for up to a second on a tokio worker, and in
/// ~keep the pool while it held its state lock.
#[derive(Debug)]
pub(crate) struct ScratchProfileDir(Option<ProfileTeardown>);

impl ScratchProfileDir {
    /// Create a fresh directory. The random suffix avoids Chrome `SingletonLock` collisions.
    pub(crate) fn create(prefix: &str) -> Result<Self, CrawlError> {
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir()
            .map(|dir| {
                Self(Some(ProfileTeardown {
                    dir: dir.keep(),
                    chrome: None,
                }))
            })
            .map_err(|e| CrawlError::browser_error(format!("failed to create a Chrome profile directory: {e}")))
    }

    pub(crate) fn path(&self) -> &std::path::Path {
        &self.teardown().dir
    }

    /// Launch Chrome on this directory and record the Chrome it started, whose processes the
    /// teardown stops. A failed launch drops the directory, which removes it.
    pub(crate) async fn launch(
        mut self,
        config: BrowserConfig,
    ) -> Result<(Browser, Handler, Self), chromiumoxide::error::CdpError> {
        // ~keep Boxed: the launch future is large, and each caller's future holds it inline, which
        // ~keep pushed the generated dart binding's async dispatch past rustc's query depth limit.
        let (mut browser, handler) = Box::pin(Browser::launch(config)).await?;
        if let Some(pid) = browser.get_mut_child().and_then(|child| child.as_mut_inner().id()) {
            self.record_chrome(pid);
        }
        Ok((browser, handler, self))
    }

    /// Record the Chrome that the launched process `pid` started as the one using this directory.
    fn record_chrome(&mut self, pid: u32) {
        let chrome = chrome_started_by(pid, &self.teardown().dir);
        if chrome.is_none() {
            tracing::warn!(
                pid,
                "the launched Chrome's executable is unreadable; its profile teardown stops no process"
            );
        }
        self.0.as_mut().expect("the teardown is taken only by Drop").chrome = chrome;
    }

    fn teardown(&self) -> &ProfileTeardown {
        self.0.as_ref().expect("the teardown is taken only by Drop")
    }
}

impl Drop for ScratchProfileDir {
    /// Hand the teardown to the runtime's blocking pool, or to a thread of its own outside a runtime.
    fn drop(&mut self) {
        let Some(teardown) = self.0.take() else {
            return;
        };
        #[cfg(test)]
        tests::PROFILE_HAND_OFFS_HERE.with(|count| count.set(count.get() + 1));
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => drop(handle.spawn_blocking(move || drop(teardown))),
            Err(_) => {
                let spawned = std::thread::Builder::new()
                    .name("crawlberg-profile-teardown".to_owned())
                    .spawn(move || drop(teardown));
                if let Err(error) = spawned {
                    tracing::warn!(%error, "no thread for the Chrome profile teardown; it ran on the dropping thread");
                }
            }
        }
    }
}

/// The work a dropped [`ScratchProfileDir`] hands off: stop the Chrome processes using the
/// directory, then remove it.
///
/// ~keep Its own `Drop` does the work, so a hand-off that never runs still does it wherever the
/// ~keep closure holding it is dropped: tokio drops a blocking task queued as the runtime shuts
/// ~keep down without running it, and `std::thread::Builder::spawn` drops its closure when the OS
/// ~keep refuses a thread.
#[derive(Debug)]
struct ProfileTeardown {
    dir: std::path::PathBuf,
    /// The executables of the Chrome launched on `dir`, from [`chrome_started_by`]. `None` when no launch
    /// succeeded, so no Chrome of crawlberg's can be using the directory.
    chrome: Option<std::path::PathBuf>,
}

impl Drop for ProfileTeardown {
    fn drop(&mut self) {
        #[cfg(test)]
        tests::PROFILE_TEARDOWNS_HERE.with(|count| count.set(count.get() + 1));
        if let Some(chrome) = &self.chrome {
            stop_chrome_processes_using(&self.dir, chrome);
        }
        if let Err(error) = std::fs::remove_dir_all(&self.dir)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(dir = %self.dir.display(), %error, "failed to remove the Chrome profile directory");
        }
    }
}

/// The executables of the Chrome that the launched process `pid` started on `dir`: the executable
/// of the deepest process below `pid` whose command line names `dir` as its profile, or on macOS
/// the outermost `.app` bundle that holds it. `None` when `pid` names no such process or that
/// executable cannot be read.
///
/// ~keep The launched process is Chrome only when every launcher on the way execs. Debian's
/// ~keep `/usr/bin/chromium` and Google's `google-chrome` scripts do; a script that runs Chrome as
/// ~keep its child stays a shell, and every shell naming the flag would then pass for Chrome.
/// ~keep Each process of the launch carries the flag, the launcher's shell included, and the deepest
/// ~keep one is Chrome or a helper Chrome started, which runs Chrome's executable (on macOS a helper
/// ~keep bundle inside Chrome's own bundle). The path comes from the process table, as each helper's
/// ~keep does, so the two compare equal on every platform, where a canonicalized path carries a
/// ~keep `\\?\` prefix on Windows that the process table does not.
fn chrome_started_by(pid: u32, dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut system = sysinfo::System::new();
    let users = processes_naming(&mut system, &user_data_dir_flag(dir));
    let mut chrome = *users.iter().find(|process| process.pid().as_u32() == pid)?;
    // ~keep Bounded: a table read across a pid reuse could link two processes both ways.
    for _ in 0..users.len() {
        match users.iter().find(|process| process.parent() == Some(chrome.pid())) {
            Some(child) => chrome = child,
            None => break,
        }
    }
    let executable = chrome.exe()?;
    let bundle = executable
        .ancestors()
        .filter(|dir| dir.extension().is_some_and(|extension| extension == "app"))
        .last();
    Some(bundle.unwrap_or(executable).to_path_buf())
}

/// The flag a Chrome process using `dir` as its profile carries on its command line.
///
/// ~keep Formatted as chromiumoxide formats it for the browser process, which Chrome copies to
/// ~keep every helper unchanged.
pub(crate) fn user_data_dir_flag(dir: &std::path::Path) -> String {
    format!("--user-data-dir={}", dir.display())
}

/// Refresh `system` and return the live processes whose command line holds `token` as a whole,
/// space-delimited token.
///
/// ~keep Chrome on Linux rewrites the command line of each of its processes into one string with
/// ~keep the arguments joined by spaces (its process title), so the flag is never an argument of
/// ~keep its own there. The arguments are joined the same way and the token must be bounded by a
/// ~keep space or an end on both sides, so a process naming a different directory that starts with
/// ~keep this path, or naming a file inside it, never matches. A zombie is skipped: it can no longer
/// ~keep write, and it keeps its command line until its parent reaps it, which for the browser
/// ~keep process is this process, possibly not before the teardown returns.
pub(crate) fn processes_naming<'s>(system: &'s mut sysinfo::System, token: &str) -> Vec<&'s sysinfo::Process> {
    use sysinfo::ProcessesToUpdate;

    system.refresh_processes_specifics(ProcessesToUpdate::All, true, user_refresh());
    system
        .processes()
        .values()
        .filter(|process| names(process, token))
        .collect()
}

/// What [`processes_naming`] and [`kill_if_chrome_using`] read of a process.
fn user_refresh() -> sysinfo::ProcessRefreshKind {
    use sysinfo::{ProcessRefreshKind, UpdateKind};

    ProcessRefreshKind::nothing()
        .without_tasks()
        .with_cmd(UpdateKind::Always)
        .with_exe(UpdateKind::Always)
}

/// Whether `process` is live and its command line holds `token` as a whole token.
fn names(process: &sysinfo::Process, token: &str) -> bool {
    process.status() != sysinfo::ProcessStatus::Zombie && command_line_names(process.cmd(), token)
}

/// Whether the arguments in `cmd`, joined by spaces, hold `token` bounded by a space or an end.
fn command_line_names(cmd: &[std::ffi::OsString], token: &str) -> bool {
    let line = cmd
        .iter()
        .map(|argument| argument.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    let bytes = line.as_bytes();
    line.match_indices(token).any(|(start, _)| {
        let end = start + token.len();
        (start == 0 || bytes[start - 1] == b' ') && (end == bytes.len() || bytes[end] == b' ')
    })
}

/// Kill every process of `chrome`, as [`chrome_started_by`] names it, whose command line names `dir`
/// as its profile, and wait until none is left and each one killed has ended, or until
/// [`PROFILE_USERS_EXIT_TIMEOUT`] passes.
///
/// ~keep A process counts only when its executable is `chrome` or lies in it and its command line
/// ~keep carries the flag for `dir`, a directory a [`ScratchProfileDir`] created under a random
/// ~keep name. A shell, `strace` or `grep` that names the flag runs another executable and is left
/// ~keep alone. A saved profile the caller named is never a `ScratchProfileDir`, so its Chrome is
/// ~keep never killed here and it is never removed.
fn stop_chrome_processes_using(dir: &std::path::Path, chrome: &std::path::Path) {
    let flag = user_data_dir_flag(dir);
    let mut system = sysinfo::System::new();
    let mut killed = Vec::new();
    let deadline = std::time::Instant::now() + PROFILE_USERS_EXIT_TIMEOUT;
    let stopped = kill_until_gone(PROFILE_USERS_EXIT_TIMEOUT, || {
        let users: Vec<_> = processes_naming(&mut system, &flag)
            .into_iter()
            .filter(|process| runs(process, chrome))
            .map(sysinfo::Process::pid)
            .collect();
        for &pid in &users {
            killed.extend(kill_if_chrome_using(pid, &flag, chrome));
        }
        !users.is_empty()
    });
    if !(stopped && wait_until_ended(&killed, deadline)) {
        tracing::warn!(dir = %dir.display(), "Chrome processes still use the profile directory after a kill");
    }
}

/// Whether `process` runs `chrome`, as [`chrome_started_by`] names it.
fn runs(process: &sysinfo::Process, chrome: &std::path::Path) -> bool {
    process.exe().is_some_and(|exe| exe.starts_with(chrome))
}

/// A process [`kill_if_chrome_using`] killed, for [`wait_until_ended`] to wait on.
///
/// ~keep On Linux it is the pidfd the kill went through. The scan stops seeing a killed process once
/// ~keep its main thread is a zombie, but its other threads can still be finishing a file operation
/// ~keep then, and a killed Chrome's browser process has a dozen or more. One that lands in the
/// ~keep directory while it is being removed makes the removal fail with "directory not empty" and
/// ~keep leaves the profile behind (xberg-io/crawlberg#415). A pidfd turns readable only once every
/// ~keep thread of its process has exited. Elsewhere the kill goes by pid and leaves nothing to wait on.
#[cfg(target_os = "linux")]
type KilledProcess = rustix::fd::OwnedFd;
#[cfg(not(target_os = "linux"))]
type KilledProcess = std::convert::Infallible;

/// Kill the process `pid` if it still runs `chrome` and its command line still holds `flag`, and
/// return what to wait on until it has ended.
///
/// ~keep `pid` comes from an earlier scan, and its process can have exited and the pid gone to a new
/// ~keep process since. A pidfd does not stop the pid number from being reused once its process is
/// ~keep reaped; it pins the process itself, so on Linux the kill goes through the pidfd and reaches
/// ~keep only the pinned process, failing with no such process if it has already exited, even if the
/// ~keep re-check ran just before the reuse. Elsewhere, and on a Linux kernel older than 5.3, the kill
/// ~keep goes by pid, so the re-check running right before it narrows the gap but does not close it.
fn kill_if_chrome_using(pid: sysinfo::Pid, flag: &str, chrome: &std::path::Path) -> Option<KilledProcess> {
    #[cfg(target_os = "linux")]
    if let Some(pinned) = i32::try_from(pid.as_u32())
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    {
        match rustix::process::pidfd_open(pinned, rustix::process::PidfdFlags::empty()) {
            Ok(pidfd) => {
                if chrome_user(&mut sysinfo::System::new(), pid, flag, chrome).is_some() {
                    #[cfg(test)]
                    tests::fire_reuse_window_hook();
                    let _ = rustix::process::pidfd_send_signal(&pidfd, rustix::process::Signal::KILL);
                    return Some(pidfd);
                }
                return None;
            }
            Err(rustix::io::Errno::SRCH) => return None,
            Err(_) => {}
        }
    }
    if let Some(process) = chrome_user(&mut sysinfo::System::new(), pid, flag, chrome) {
        #[cfg(test)]
        tests::fire_reuse_window_hook();
        process.kill();
    }
    None
}

/// Wait until every process in `killed` has ended, or until `deadline` passes. Return whether all
/// of them ended.
#[cfg(target_os = "linux")]
fn wait_until_ended(killed: &[KilledProcess], deadline: std::time::Instant) -> bool {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};

    killed.iter().all(|pidfd| {
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let Ok(timeout) = Timespec::try_from(left) else {
                return false;
            };
            match poll(&mut [PollFd::new(pidfd, PollFlags::IN)], Some(&timeout)) {
                Ok(0) => return false,
                Ok(_) => return true,
                Err(rustix::io::Errno::INTR) => {}
                Err(_) => return false,
            }
        }
    })
}

/// Wait until every process in `killed` has ended: none can be in it off Linux.
#[cfg(not(target_os = "linux"))]
fn wait_until_ended(killed: &[KilledProcess], _deadline: std::time::Instant) -> bool {
    killed.is_empty()
}

/// The process `pid`, read afresh into `system`, if it runs `chrome` and its command line holds `flag`.
fn chrome_user<'s>(
    system: &'s mut sysinfo::System,
    pid: sysinfo::Pid,
    flag: &str,
    chrome: &std::path::Path,
) -> Option<&'s sysinfo::Process> {
    system.refresh_processes_specifics(sysinfo::ProcessesToUpdate::Some(&[pid]), true, user_refresh());
    system
        .process(pid)
        .filter(|process| names(process, flag) && runs(process, chrome))
}

/// Call `kill_users` until it reports that it found no user, pausing between calls, or until
/// `timeout` passes. Return whether it found none.
fn kill_until_gone(timeout: Duration, mut kill_users: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while kill_users() {
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(PROFILE_USERS_POLL_INTERVAL);
    }
    true
}

/// Build the [`BrowserConfigBuilder`] for a fresh pooled launch (not the
/// `browser_endpoint` connect branch).
///
/// ~keep Split out from `launch_browser` so a test can assert on the flags this path
/// ~keep actually passes without spawning a real Chrome process.
fn build_pool_launch_builder(
    user_data_dir: &std::path::Path,
    config: &BrowserPoolConfig,
) -> Result<BrowserConfigBuilder, CrawlError> {
    let mut builder = BrowserConfig::builder()
        .no_sandbox()
        .new_headless_mode()
        .user_data_dir(user_data_dir)
        .disable_default_args();
    // ~keep Chrome helper forks can trip macOS fork-safety checks; disable the ObjC abort so helpers exec.
    // ~keep The env vars are harmless on older macOS and Linux and keep pooled launches consistent.
    builder = builder
        .env("OBJC_DISABLE_INITIALIZE_FORK_SAFETY", "YES")
        .env("OS_ACTIVITY_MODE", "disable");
    builder = apply_default_args(builder, &config.chrome_args);
    apply_launch_overrides(
        builder,
        "BrowserPoolConfig",
        config.chrome_path.as_deref(),
        &config.chrome_args,
    )
}

/// Configuration for a [`BrowserPool`].
///
/// Rust-only: this type is excluded from alef-generated polyglot bindings.
/// Pool reuse is intended for long-lived Rust processes (e.g. the cloud
/// worker); language bindings construct pools internally per-call.
#[derive(Clone)]
pub struct BrowserPoolConfig {
    /// Maximum number of concurrent pages (tabs) the pool will open.
    pub max_pages: usize,
    /// If set, connect to an already-running Chrome via this CDP WebSocket URL
    /// instead of launching a new process.
    pub browser_endpoint: Option<String>,
    /// Chrome executable to launch. `None` uses the `CHROME` environment variable, then
    /// searches the machine. Ignored when `browser_endpoint` is set.
    pub chrome_path: Option<std::path::PathBuf>,
    /// Extra command-line arguments forwarded to the Chrome process, after the defaults.
    /// Checked by the rules of `BrowserConfig::chrome_args` when the pool launches Chrome:
    /// a refused entry makes `warm` and `acquire_page` return an error.
    pub chrome_args: Vec<String>,
    /// How long to wait for Chrome to start before giving up.
    pub launch_timeout: Duration,
}

impl std::fmt::Debug for BrowserPoolConfig {
    /// Redacted: a CDP `browser_endpoint` is itself the capability, so only its scheme, host
    /// and port print. See `crate::net::redact::redact_url_to_origin`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self {
            max_pages,
            browser_endpoint,
            chrome_path,
            chrome_args,
            launch_timeout,
        } = self;
        f.debug_struct("BrowserPoolConfig")
            .field("max_pages", max_pages)
            .field(
                "browser_endpoint",
                &browser_endpoint
                    .as_deref()
                    .map(crate::net::redact::redact_url_to_origin),
            )
            .field("chrome_path", chrome_path)
            .field("chrome_args", chrome_args)
            .field("launch_timeout", launch_timeout)
            .finish()
    }
}

impl Default for BrowserPoolConfig {
    fn default() -> Self {
        Self {
            max_pages: 8,
            browser_endpoint: None,
            chrome_path: None,
            chrome_args: Vec::new(),
            launch_timeout: Duration::from_secs(30),
        }
    }
}

/// The page-close tasks that [`PooledPage`]'s `Drop` spawned for the current browser, kept so
/// teardown can wait for them instead of cancelling them.
type PendingCloses = Arc<std::sync::Mutex<Vec<JoinHandle<()>>>>;

/// What a caller-owned (connected) Chrome needs tidied before crawlberg disconnects from it:
/// the tabs crawlberg opened there. Ignored for a Chrome crawlberg launched itself, which is
/// closed outright and takes its tabs with it.
#[derive(Default)]
pub(crate) struct ExternalTabCleanup {
    /// A tab to close directly, for a caller that opened exactly one and tracked its id.
    pub(crate) open_tab: Option<TargetId>,
    /// Page-close tasks already in flight, to be awaited before the CDP websocket goes away.
    pub(crate) pending_closes: Option<PendingCloses>,
}

struct BrowserState {
    browser: Arc<Browser>,
    /// The SSRF check every page of this browser runs under. It holds the other reference
    /// to `browser` until it is stopped.
    firewall: BrowserFirewall,
    handler_handle: JoinHandle<()>,
    user_data_dir: Option<ScratchProfileDir>,
    pending_closes: PendingCloses,
}

/// How a browser left [`close_browser_within`]: under its own steam, or killed.
///
/// ~keep This distinction is the whole point of the return value: a killed Chrome never
/// ~keep answers the CDP `Browser.close` its handler loop is waiting on, so teardown has to
/// ~keep treat the two cases differently (xberg-io/crawlberg#146). `#[must_use]` sits on the
/// ~keep type so a call site that discards the outcome is a warning, not a silent 5-second wait.
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BrowserCloseOutcome {
    /// `Browser::close` and `Browser::wait` both finished inside `shutdown_timeout`.
    Exited,
    /// `shutdown_timeout` expired, so the process was force-killed via [`Browser::kill`].
    Killed,
}

impl BrowserState {
    /// Stop the SSRF check, then close the browser, or disconnect from one crawlberg does not
    /// own, bounded by the handler shutdown timeout.
    async fn close(self) {
        self.firewall.stop().await;
        let cleanup = ExternalTabCleanup {
            pending_closes: Some(self.pending_closes),
            ..ExternalTabCleanup::default()
        };
        // ~keep The stopped firewall held the only other reference, so this is the browser itself.
        match Arc::into_inner(self.browser) {
            Some(browser) => release_browser(browser, self.handler_handle, cleanup, HANDLER_SHUTDOWN_TIMEOUT).await,
            None => self.handler_handle.abort(),
        }
        drop(self.user_data_dir);
    }
}

/// Stop the task running the CDP handler loop of a browser that has just been closed.
///
/// A browser that exited on its own ends its handler loop, so that case waits briefly for the
/// loop to finish and aborts it only if it overruns. A killed browser never will, so its handler
/// is aborted at once.
///
/// ~keep Dropping a `JoinHandle` detaches its task rather than stopping it, so simply
/// ~keep discarding the timeout result leaked one handler loop per relaunch — unbounded
/// ~keep for a domain that keeps crashing Chrome.
/// ~keep The `Killed` shortcut is not an optimisation of a wait that would have succeeded:
/// ~keep chromiumoxide 0.9.1's `Handler::poll_next` (`src/handler/mod.rs`) returns
/// ~keep `Ready(None)` only when a `Browser.close` response arrives while it is `closing`. A
/// ~keep closed websocket makes its `while let Ready(Some(_))` loop fall through to
/// ~keep `Poll::Pending`, so after a kill the loop parks for good and this wait always burned
/// ~keep the full `HANDLER_SHUTDOWN_TIMEOUT` before aborting anyway (xberg-io/crawlberg#146).
async fn stop_handler_after_close(handle: JoinHandle<()>, close_outcome: BrowserCloseOutcome) {
    if close_outcome == BrowserCloseOutcome::Killed {
        handle.abort();
        return;
    }

    let abort = handle.abort_handle();
    if tokio::time::timeout(HANDLER_SHUTDOWN_TIMEOUT, handle).await.is_err() {
        tracing::warn!(
            timeout_secs = HANDLER_SHUTDOWN_TIMEOUT.as_secs(),
            "CDP handler did not exit before the shutdown timeout; aborting it"
        );
        abort.abort();
    }
}

/// Connect to the external Chrome at a configured CDP `endpoint`. The pool, the one-shot
/// launch path and the interact backend all connect through this one function.
///
/// ~keep async-tungstenite accepts only a lower-case `ws`/`wss` scheme, and `http::Uri` refuses
/// ~keep surrounding spaces and a missing `//`, while the endpoint checks accept all of those
/// ~keep spellings. So a WebSocket endpoint is sent in the normalized form of the same parse the
/// ~keep checks use. Any other endpoint (chromiumoxide also takes an `http://` DevTools address,
/// ~keep and the pool's own field has no check) is sent as written.
///
/// The endpoint is a capability (its userinfo, its CDP path GUID or a `?token=` drives the
/// browser), and the error flows into API error bodies and MCP error payloads, so only its
/// origin prints.
///
/// ~keep The connect future is boxed. Every crawl future that can reach a connect contains this
/// ~keep one, and without the box the extra async layer pushes the generated Dart bridge's
/// ~keep crawl future past rustc's layout query depth limit (`crawlberg-dart` fails to build).
pub(crate) async fn connect_endpoint(endpoint: &str) -> Result<(Browser, chromiumoxide::Handler), CrawlError> {
    let normalized = crate::net::parse_websocket_url(endpoint);
    let address = normalized.as_ref().map_or(endpoint, url::Url::as_str);
    Box::pin(Browser::connect(address)).await.map_err(|e| {
        let redacted = crate::net::redact::redact_url_to_origin(endpoint);
        CrawlError::browser_error(format!("failed to connect to {redacted}: {e}"))
    })
}

/// Tear down `browser` and the task that runs its CDP handler.
///
/// A Chrome that crawlberg launched is closed and reaped within `shutdown_timeout` (see
/// `close_browser_within`); closing it removes every tab it has. A Chrome reached through
/// `Browser::connect` (a configured `browser.endpoint`) belongs to the caller: crawlberg closes
/// only the tabs named by `cleanup`, then disconnects by stopping the handler task that owns the
/// CDP websocket. It never sends that Chrome `Browser.close`.
///
/// ~keep chromiumoxide 0.9.1 has no disconnect call, and its handler loop runs until the
/// ~keep websocket closes, which a connected Chrome never does on its own. Aborting the task
/// ~keep drops the websocket. `get_mut_child` is `None` exactly for a connected browser.
/// ~keep `cleanup` is honoured only on the connected branch: on a launched Chrome that hangs,
/// ~keep a tab close ahead of `close_browser_within` would push the kill past `shutdown_timeout`.
pub(crate) async fn release_browser(
    mut browser: Browser,
    handler_handle: JoinHandle<()>,
    cleanup: ExternalTabCleanup,
    shutdown_timeout: Duration,
) {
    if browser.get_mut_child().is_none() {
        if let Some(pending) = cleanup.pending_closes {
            await_pending_closes(&pending, shutdown_timeout).await;
        }
        if let Some(target_id) = cleanup.open_tab {
            let _ = tokio::time::timeout(shutdown_timeout, browser.execute(CloseTargetParams::new(target_id))).await;
        }
        drop(browser);
        handler_handle.abort();
        return;
    }
    let close_outcome = close_browser_within(&mut browser, shutdown_timeout).await;
    drop(browser);
    stop_handler_after_close(handler_handle, close_outcome).await;
}

/// Wait for the page-close tasks [`PooledPage`]'s `Drop` spawned, bounding the wait by
/// `wait_timeout`.
///
/// ~keep `Drop` cannot await, so it spawns `page.close()` and records the handle here. Teardown
/// ~keep aborts the handler task that owns the CDP websocket, which cancels any close still in
/// ~keep flight and leaves that tab open in a Chrome crawlberg does not own -- the leak this
/// ~keep function exists to prevent. Awaiting the handles, not just sleeping, keeps it
/// ~keep deterministic: `Drop` registers the handle before it returns, so a caller that drops a
/// ~keep `PooledPage` and then shuts the pool down always finds the close here.
async fn await_pending_closes(pending: &PendingCloses, wait_timeout: Duration) {
    let handles = match pending.lock() {
        Ok(mut guard) => std::mem::take(&mut *guard),
        Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
    };
    if handles.is_empty() {
        return;
    }
    let pending_count = handles.len();
    let joined = tokio::time::timeout(wait_timeout, async {
        for handle in handles {
            let _ = handle.await;
        }
    })
    .await;
    if joined.is_err() {
        tracing::warn!(
            pending = pending_count,
            timeout_secs = wait_timeout.as_secs_f64(),
            "pool-opened tabs did not close before the shutdown timeout; leaving them to the browser"
        );
    }
}

/// Close `browser` and wait for its process to exit, bounding the wait by
/// `shutdown_timeout`. If the browser has not exited before the deadline, the
/// process is force-killed via [`Browser::kill`] rather than left to linger.
///
/// ~keep `Browser::wait` is a bare `child.wait().await` with no built-in limit, so a
/// ~keep Chrome instance stuck behind a blocking OS dialog (observed: a macOS "wants to
/// ~keep use your confidential information" keychain prompt) previously held this call
/// ~keep open indefinitely. `close` sends a real CDP `Browser.close` even to a browser this
/// ~keep process only connected to, so only `release_browser` calls this, for a launched one.
///
/// Returns which of the two happened, so the caller can hand it to [`stop_handler_after_close`]
/// instead of waiting on a handler loop that a killed process will never end.
async fn close_browser_within(browser: &mut Browser, shutdown_timeout: Duration) -> BrowserCloseOutcome {
    let closed = tokio::time::timeout(shutdown_timeout, async {
        let _ = browser.close().await;
        let _ = browser.wait().await;
    })
    .await;

    if closed.is_err() {
        tracing::warn!(
            timeout_secs = shutdown_timeout.as_secs_f64(),
            "browser did not close before the shutdown timeout; killing the process"
        );
        let _ = browser.kill().await;
        return BrowserCloseOutcome::Killed;
    }

    BrowserCloseOutcome::Exited
}

/// A pool that keeps a single Chrome browser alive and hands out pages (tabs),
/// limiting concurrency via a semaphore. If Chrome crashes the pool will
/// attempt to relaunch on the next [`acquire_page`](Self::acquire_page) call.
///
/// Rust-only: excluded from alef-generated polyglot bindings. Downstream
/// language clients should rely on per-call browser construction inside
/// crawlberg rather than managing a pool themselves.
pub struct BrowserPool {
    config: BrowserPoolConfig,
    state: Mutex<Option<BrowserState>>,
    page_semaphore: Arc<Semaphore>,
    shutdown: AtomicBool,
    /// Lock-free health signal updated whenever browser state changes.
    healthy: AtomicBool,
}

impl BrowserPool {
    /// Create a new pool. Chrome is **not** launched until the first call to
    /// [`acquire_page`](Self::acquire_page) or [`warm`](Self::warm).
    pub fn new(config: BrowserPoolConfig) -> Arc<Self> {
        let semaphore = Arc::new(Semaphore::new(config.max_pages));
        Arc::new(Self {
            config,
            state: Mutex::new(None),
            page_semaphore: semaphore,
            shutdown: AtomicBool::new(false),
            healthy: AtomicBool::new(false),
        })
    }

    /// Eagerly launch the Chrome process so that the first
    /// [`acquire_page`](Self::acquire_page) call does not pay the startup
    /// cost. Returns an error immediately if Chrome cannot be started.
    pub async fn warm(&self) -> Result<(), CrawlError> {
        let mut guard = self.state.lock().await;
        if guard.is_none() {
            let bs = self.launch_browser().await?;
            *guard = Some(bs);
            self.healthy.store(true, Ordering::Release);
        }
        Ok(())
    }

    /// Acquire a new blank page from the pool.
    ///
    /// Blocks asynchronously if `max_pages` pages are already open. The page
    /// should be closed via [`PooledPage::close`] when done; if dropped
    /// without calling `close`, a best-effort async cleanup is spawned.
    pub async fn acquire_page(&self) -> Result<PooledPage, CrawlError> {
        if self.shutdown.load(Ordering::SeqCst) {
            return Err(CrawlError::browser_error("pool is shut down"));
        }

        let permit = self
            .page_semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| CrawlError::browser_error("page semaphore closed"))?;

        if self.shutdown.load(Ordering::SeqCst) {
            return Err(CrawlError::browser_error("pool is shut down"));
        }

        match self.try_new_page().await {
            Ok((page, pending_closes)) => Ok(PooledPage {
                page: Some(page),
                _permit: Some(permit),
                pending_closes: Some(pending_closes),
            }),
            Err(first_err) => {
                self.relaunch_browser().await?;
                let (page, pending_closes) = self.try_new_page().await.map_err(|e| {
                    CrawlError::browser_error(format!(
                        "failed to open page after relaunch: {e} (original: {first_err})"
                    ))
                })?;
                Ok(PooledPage {
                    page: Some(page),
                    _permit: Some(permit),
                    pending_closes: Some(pending_closes),
                })
            }
        }
    }

    /// Non-blocking health check. Returns `true` when Chrome is running.
    ///
    /// This is a lock-free atomic read — safe for use in health probes and
    /// monitoring without risking contention.
    pub fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire) && !self.shutdown.load(Ordering::Acquire)
    }

    /// Gracefully shut the pool down. Safe to call multiple times.
    pub async fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
        self.healthy.store(false, Ordering::Release);

        self.page_semaphore.close();

        let mut guard = self.state.lock().await;
        if let Some(bs) = guard.take() {
            bs.close().await;
        }
    }

    /// The SSRF check of the running browser, which a page of this pool must be watched by.
    #[cfg(feature = "browser")]
    pub(crate) async fn firewall(&self) -> Result<crate::ssrf_intercept::FirewallHandle, CrawlError> {
        self.state
            .lock()
            .await
            .as_ref()
            .map(|bs| bs.firewall.handle())
            .ok_or_else(|| CrawlError::browser_error("browser pool has no running browser"))
    }

    /// Try to create a new page from the current browser. Takes the mutex
    /// briefly, creates the page, and releases.
    ///
    /// Returns the page together with the current browser's pending-close registry, so the
    /// [`PooledPage`] built around it can record a close its `Drop` spawns.
    async fn try_new_page(&self) -> Result<(chromiumoxide::Page, PendingCloses), CrawlError> {
        let mut guard = self.state.lock().await;

        if guard.is_none() || guard.as_ref().is_some_and(|bs| bs.handler_handle.is_finished()) {
            self.healthy.store(false, Ordering::Release);
            if let Some(old) = guard.take() {
                old.firewall.stop().await;
                old.handler_handle.abort();
            }
            let bs = self.launch_browser().await?;
            *guard = Some(bs);
            self.healthy.store(true, Ordering::Release);
        }

        let bs = guard.as_ref().expect("browser state was just set above");
        let page = tokio::time::timeout(PAGE_OPEN_TIMEOUT, bs.firewall.handle().new_page())
            .await
            .map_err(|_| CrawlError::browser_error("timeout opening page"))??;
        Ok((page, Arc::clone(&bs.pending_closes)))
    }

    /// Force-relaunch Chrome (used after a page-open failure).
    async fn relaunch_browser(&self) -> Result<(), CrawlError> {
        let mut guard = self.state.lock().await;

        if self.shutdown.load(Ordering::SeqCst) {
            return Err(CrawlError::browser_error("pool is shut down"));
        }

        if guard.as_ref().is_some_and(|bs| !bs.handler_handle.is_finished()) {
            return Ok(());
        }

        self.healthy.store(false, Ordering::Release);
        if let Some(old) = guard.take() {
            old.close().await;
        }

        let bs = self.launch_browser().await?;
        *guard = Some(bs);
        self.healthy.store(true, Ordering::Release);
        Ok(())
    }

    /// Launch (or connect to) a Chrome process according to the pool config.
    async fn launch_browser(&self) -> Result<BrowserState, CrawlError> {
        let (browser, mut handler, data_dir) = if let Some(ref endpoint) = self.config.browser_endpoint {
            let (browser, handler) = tokio::time::timeout(self.config.launch_timeout, connect_endpoint(endpoint))
                .await
                .map_err(|_| CrawlError::browser_error("timeout connecting to browser endpoint"))??;
            (browser, handler, None)
        } else {
            // ~keep Dropped, and so removed, on every early return below, including a launch timeout.
            let user_data_dir = ScratchProfileDir::create("crawlberg-chrome-")?;
            let builder = build_pool_launch_builder(user_data_dir.path(), &self.config)?;
            let browser_config = builder
                .build()
                .map_err(|e| CrawlError::browser_error(format!("invalid browser config: {e}")))?;

            let (browser, handler, user_data_dir) =
                tokio::time::timeout(self.config.launch_timeout, user_data_dir.launch(browser_config))
                    .await
                    .map_err(|_| CrawlError::browser_error("timeout launching Chrome"))?
                    .map_err(|e| CrawlError::browser_error(format!("failed to launch Chrome: {e}")))?;
            (browser, handler, Some(user_data_dir))
        };

        let handler_handle = tokio::spawn(async move { while handler.next().await.is_some() {} });
        let browser = Arc::new(browser);
        let firewall = match BrowserFirewall::start(
            Arc::clone(&browser),
            BrowserOrigin::of_endpoint(self.config.browser_endpoint.as_deref()),
            PageContext::of_endpoint(self.config.browser_endpoint.as_deref()),
        )
        .await
        {
            Ok(firewall) => firewall,
            Err(error) => {
                match Arc::into_inner(browser) {
                    Some(browser) => {
                        release_browser(
                            browser,
                            handler_handle,
                            ExternalTabCleanup::default(),
                            HANDLER_SHUTDOWN_TIMEOUT,
                        )
                        .await;
                    }
                    None => handler_handle.abort(),
                }
                return Err(error);
            }
        };

        Ok(BrowserState {
            browser,
            firewall,
            handler_handle,
            user_data_dir: data_dir,
            pending_closes: Arc::new(std::sync::Mutex::new(Vec::new())),
        })
    }
}

impl std::fmt::Debug for BrowserPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserPool")
            .field("config", &self.config)
            .field("healthy", &self.healthy.load(Ordering::Relaxed))
            .field("shutdown", &self.shutdown.load(Ordering::Relaxed))
            .finish()
    }
}

/// A page (tab) borrowed from a [`BrowserPool`].
///
/// The semaphore permit is released when this value is dropped, allowing
/// another caller to open a page. Prefer calling [`close`](Self::close) for
/// deterministic async cleanup.
pub struct PooledPage {
    page: Option<chromiumoxide::Page>,
    _permit: Option<OwnedSemaphorePermit>,
    /// Where `Drop` records the close it spawns, so pool teardown can await it.
    pending_closes: Option<PendingCloses>,
}

impl PooledPage {
    /// Access the underlying CDP page.
    pub fn page(&self) -> &chromiumoxide::Page {
        self.page.as_ref().expect("page already taken via close()")
    }

    /// Explicitly close the page. Sends a CDP `Target.closeTarget` command so
    /// that Chrome tears down the tab immediately. The semaphore permit is
    /// released when `self` is dropped at the end of this call.
    pub async fn close(mut self) {
        if let Some(page) = self.page.take() {
            let _ = page.close().await;
        }
    }

    // ~keep Hands the page + permit to a new owner (e.g. session affinity) without running
    // ~keep Drop's close-on-drop, which would race a still-in-flight navigation on the same target.
    /// Detach the page and its semaphore permit for handoff to another owner.
    ///
    /// Unlike [`close`](Self::close), this does not close the CDP target — the
    /// caller becomes responsible for eventually closing the page and dropping
    /// the permit. Used when a page is handed off to
    /// [`BrowserSessionPool`](crate::browser_session_pool::BrowserSessionPool)
    /// for reuse, or simply to keep the page+permit alive across a caller's
    /// `.await` boundary instead of dropping them at the end of an expression.
    // ~keep Gated on `browser`, like its only callers in browser.rs and the session pool it hands
    // ~keep off to. This module is compiled under the narrower `browser-chromiumoxide`, where the
    // ~keep method has no caller and would be dead code.
    #[cfg(feature = "browser")]
    pub(crate) fn into_parts(mut self) -> (chromiumoxide::Page, Option<OwnedSemaphorePermit>) {
        let page = self.page.take().expect("page already taken via close()");
        let permit = self._permit.take();
        (page, permit)
    }
}

impl Drop for PooledPage {
    // ~keep `tokio::spawn` panics when no runtime is active on the current thread. These
    // ~keep handles cross an FFI boundary into host GC/finalizer threads, so an unguarded
    // ~keep spawn here turns a late drop into a panic that aborts the embedding process.
    fn drop(&mut self) {
        if let Some(page) = self.page.take() {
            match tokio::runtime::Handle::try_current() {
                Ok(handle) => {
                    let close = handle.spawn(async move {
                        let _ = page.close().await;
                    });
                    match self.pending_closes.take() {
                        // ~keep Recorded, not detached: pool teardown aborts the task owning the
                        // ~keep CDP websocket, which cancels this close and leaks the tab in a
                        // ~keep Chrome crawlberg only connected to. `release_browser` awaits it.
                        Some(pending) => match pending.lock() {
                            Ok(mut guard) => guard.push(close),
                            Err(poisoned) => poisoned.into_inner().push(close),
                        },
                        None => {
                            tracing::debug!("dropping a pooled page with no close registry; its close runs detached");
                        }
                    }
                }
                Err(_) => {
                    tracing::warn!("dropping a pooled page outside a Tokio runtime; its CDP target is left to Chrome");
                }
            }
        }
    }
}

/// Assert that `builder` carries no double-dashed flag key and, on macOS, carries the
/// mock-keychain flag. Shared by the behavioral test for each of the three launch paths
/// (this file, `browser.rs`, `interact/chromiumoxide.rs`).
///
/// ~keep chromiumoxide's `Arg` derives `Debug` on its private `key` field, so this reads
/// ~keep the exact string chromiumoxide stored, before it renders it as `--{key}`.
/// ~keep `!debug.contains("----")` cannot fail here: chromiumoxide never performs that
/// ~keep render at `Debug`/`build()` time (only inside `launch()`, which spawns Chrome),
/// ~keep so a leftover `--` in `key` would show as `key: "--foo"`, one dash short of what
/// ~keep an earlier version of this check looked for. Assert on the stored key directly.
/// ~keep Kept right before `mod tests` (not up with `apply_default_args`), on purpose:
/// ~keep `test_every_known_launch_path_calls_the_shared_apply_default_args_helper` below
/// ~keep finds the boundary between production code and test code by splitting each
/// ~keep file on its first `#[cfg(test)]` marker. A `#[cfg(test)]` item placed earlier in
/// ~keep the file would move that boundary and hide a real, later production call site.
#[cfg(test)]
pub(crate) fn assert_launch_flags_are_normalized(builder: &BrowserConfigBuilder) {
    let debug = format!("{builder:?}");
    assert!(
        !debug.contains("key: \"--"),
        "a flag key still carries its own `--`, which chromiumoxide would double-prefix: {debug}"
    );
    if cfg!(target_os = "macos") {
        assert!(
            debug.contains("key: \"use-mock-keychain\""),
            "missing --use-mock-keychain on macOS: {debug}"
        );
    }
}

/// Assert that `build` hands `chrome_path` and `chrome_args` to chromiumoxide: the binary is
/// the configured one, each caller flag is normalized, and a caller flag replaces the default
/// of the same name.
/// Shared by the builder test of each of the three launch paths.
#[cfg(test)]
pub(crate) fn assert_launch_overrides_reach_the_builder(
    build: impl Fn(Option<std::path::PathBuf>, Vec<String>) -> Result<BrowserConfigBuilder, CrawlError>,
) {
    let binary = crate::types::executable_temp_file("builder");
    let result = build(
        Some(binary.clone()),
        vec!["--user-agent=crawlberg-marker".to_owned(), "--lang=fr".to_owned()],
    );
    let _ = std::fs::remove_file(&binary);
    let debug = format!("{:?}", result.expect("an executable chrome_path must be accepted"));

    assert!(
        debug.contains(&format!("executable: Some({:?})", binary)),
        "chrome_path did not reach chromiumoxide's executable: {debug}"
    );
    for caller_flag in ["user-agent=crawlberg-marker", "lang=fr"] {
        assert!(
            debug.contains(&format!("key: {caller_flag:?}")),
            "caller flag {caller_flag:?} missing or not normalized: {debug}"
        );
    }
    assert!(
        !debug.contains("key: \"lang=en_US\""),
        "the caller's --lang must replace the default --lang, not sit beside it: {debug}"
    );
    assert!(
        debug.contains("key: \"disable-sync\""),
        "defaults the caller did not name must stay: {debug}"
    );

    let missing = build(
        Some(std::path::PathBuf::from("/nonexistent/crawlberg-chrome")),
        Vec::new(),
    )
    .expect_err("a missing chrome_path must be an error, not a fallback to detection");
    assert!(
        missing.to_string().contains("/nonexistent/crawlberg-chrome"),
        "the error must name the path, got: {missing}"
    );
}

#[cfg(test)]
#[path = "browser_pool_tests.rs"]
pub(crate) mod tests;
