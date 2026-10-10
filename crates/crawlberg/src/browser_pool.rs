//! Browser pool for managing a persistent Chrome instance with bounded concurrency.
//!
//! ~keep This module is feature-gated behind `#[cfg(feature = "browser-chromiumoxide")]` at
//! ~keep the module level in `lib.rs` (the narrower flag -- `browser` implies it, see the
//! ~keep `~keep` there). Three methods compiled under this module, `PooledPage::into_parts`,
//! ~keep `BrowserPool::firewall` and `HandlerEnd::cause`, are only called from code gated on the
//! ~keep wider `browser` feature, so each carries its own `#[cfg(feature = "browser")]` inline
//! ~keep (`HandlerEnd::cause` adds `test`, because a test of this module reads it too); those are
//! ~keep the sanctioned in-file feature gates, not a precedent for adding more.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use chromiumoxide::Handler;
use chromiumoxide::browser::{Browser, BrowserConfig, BrowserConfigBuilder};
use chromiumoxide::cdp::browser_protocol::target::{CloseTargetParams, TargetId};
use sysinfo::{Pid, Process, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, Signal, System};
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

/// Chrome's proxy bypass rule that removes its built-in loopback bypass, so requests to
/// `localhost` and `127.0.0.1` go through the proxy like every other request.
///
/// ~keep Without it Chrome sends loopback requests direct whatever proxy is set, both for
/// ~keep `--proxy-server` and for a browser context's `proxyServer`.
pub(crate) const NO_LOOPBACK_BYPASS: &str = "<-loopback>";

/// Set Chrome's WebRTC IP handling in the profile at `user_data_dir` to send UDP only through
/// a proxy. A one-shot or interact launch calls this before Chrome starts when `deny_private`
/// is on; a pooled launch always does, as one pooled Chrome serves crawls with either policy.
///
/// ~keep WebRTC ignores every proxy and request interception; this preference is the one
/// ~keep switch that stops its UDP. The SSRF proxy carries no UDP, so no WebRTC UDP leaves.
pub(crate) fn disable_non_proxied_udp(user_data_dir: &std::path::Path) -> Result<(), CrawlError> {
    let failed = |e: &dyn std::fmt::Display| {
        CrawlError::browser_error(format!("failed to write the browser profile's WebRTC preference: {e}"))
    };
    let directory = user_data_dir.join("Default");
    let path = directory.join("Preferences");
    let mut preferences = match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice::<serde_json::Value>(&bytes).map_err(|e| failed(&e))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => return Err(failed(&e)),
    };
    let Some(root) = preferences.as_object_mut() else {
        return Err(failed(&"the Preferences file is not a JSON object"));
    };
    let webrtc = root.entry("webrtc").or_insert_with(|| serde_json::json!({}));
    let Some(webrtc) = webrtc.as_object_mut() else {
        return Err(failed(&"its webrtc entry is not a JSON object"));
    };
    webrtc.insert("ip_handling_policy".to_owned(), "disable_non_proxied_udp".into());
    std::fs::create_dir_all(&directory).map_err(|e| failed(&e))?;
    std::fs::write(&path, preferences.to_string()).map_err(|e| failed(&e))
}

/// How long [`confirm_profile_in_use`] waits for Chrome's `DevToolsActivePort` file.
const PROFILE_CONFIRM_TIMEOUT: Duration = Duration::from_secs(5);

/// Confirm that `browser`, just launched on `user_data_dir`, opened that directory, so the
/// WebRTC preference [`disable_non_proxied_udp`] wrote there is in effect. If it did not, kill
/// the browser and return an error: the crawl never runs with the policy off.
///
/// ~keep Chrome launched with `--remote-debugging-port=0` writes its DevTools port and browser
/// ~keep path into `<user-data-dir>/DevToolsActivePort` as it prints them on stderr, where
/// ~keep chromiumoxide reads the websocket address. A sandboxed Chrome, such as a strictly
/// ~keep confined snap with a private /tmp, opens another directory at the same path and writes
/// ~keep the file there instead. The browser path carries a guid unique to the launch, so a
/// ~keep stale file copied in with a saved profile never matches.
pub(crate) async fn confirm_profile_in_use(
    browser: &mut Browser,
    user_data_dir: &std::path::Path,
) -> Result<(), CrawlError> {
    let deadline = tokio::time::Instant::now() + PROFILE_CONFIRM_TIMEOUT;
    while !wrote_devtools_port(user_data_dir, browser.websocket_address()) {
        if tokio::time::Instant::now() >= deadline {
            let _ = browser.kill().await;
            return Err(CrawlError::browser_error(format!(
                "the browser did not use crawlberg's profile directory {}, so its WebRTC policy is not \
                 in effect; a sandboxed browser such as a confined Chromium snap opens its own copy of \
                 that path. Use a Chrome that is not sandboxed this way, or set chrome_path to the \
                 snap's /snap/bin/<name> command so crawlberg puts the profile where the snap reads it",
                user_data_dir.display()
            )));
        }
        tokio::time::sleep(PROFILE_USERS_POLL_INTERVAL).await;
    }
    Ok(())
}

/// Whether the `DevToolsActivePort` file in `user_data_dir` names the Chrome whose DevTools
/// websocket is at `websocket_address`.
fn wrote_devtools_port(user_data_dir: &std::path::Path, websocket_address: &str) -> bool {
    let Ok(contents) = std::fs::read_to_string(user_data_dir.join("DevToolsActivePort")) else {
        return false;
    };
    let mut lines = contents.lines().map(str::trim);
    match (lines.next(), lines.next()) {
        (Some(port), Some(path)) if !port.is_empty() && path.starts_with('/') => {
            websocket_address.ends_with(&format!(":{port}{path}"))
        }
        _ => false,
    }
}

/// The directory a scratch profile for the Chrome at `chrome_path` is made in. `None` is the
/// Chrome chromiumoxide finds for itself, by the same search.
///
/// ~keep A snap gets a private /tmp, so a profile in the system temp directory is invisible to
/// ~keep it; `$HOME/snap/<name>/common` is the one place it sees at the same path.
fn scratch_profile_parent(chrome_path: Option<&std::path::Path>) -> std::path::PathBuf {
    chrome_executable(chrome_path)
        .and_then(|executable| snap_common_dir(&executable, &dirs::home_dir()?))
        .unwrap_or_else(std::env::temp_dir)
}

/// The Chrome crawlberg launches for `chrome_path`: that path, or for `None` the one
/// chromiumoxide finds for itself.
pub(crate) fn chrome_executable(chrome_path: Option<&std::path::Path>) -> Option<std::path::PathBuf> {
    match chrome_path {
        Some(path) => Some(path.to_path_buf()),
        None => {
            #[cfg(test)]
            if let Some(path) = std::env::var_os("CRAWLBERG_TEST_CHROME_PATH") {
                return Some(path.into());
            }
            chromiumoxide::detection::default_executable(Default::default()).ok()
        }
    }
}

/// `home/snap/<name>/common` when `executable` runs the snap `<name>`: it lies under `/snap/`,
/// as `/snap/bin/<name>` does, or a chain of symlinks from it leads there.
pub(crate) fn snap_common_dir(executable: &std::path::Path, home: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut path = executable.to_path_buf();
    for _ in 0..16 {
        if let Ok(rest) = path.strip_prefix("/snap") {
            let mut parts = rest.iter().filter_map(|part| part.to_str());
            let name = match parts.next()? {
                // ~keep `/snap/bin/<name>` or the alias `/snap/bin/<name>.<app>`.
                "bin" => parts.next()?.split('.').next()?,
                name => name,
            };
            return (!name.is_empty()).then(|| home.join("snap").join(name).join("common"));
        }
        let target = std::fs::read_link(&path).ok()?;
        path = match path.parent() {
            Some(parent) => parent.join(target),
            None => target,
        };
    }
    None
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

/// Point `builder` at the caller's Chrome binary, if one is named, add the caller's extra
/// flags, and route the launched Chrome through `proxy`, when there is one.
/// [`apply_default_args`] has already left out any default they replace.
///
/// The configured proxy wins over the caller's flags, as it does on a pooled or connected
/// page, whose browser context is made with it: a caller `--proxy-server`,
/// `--proxy-bypass-list`, `--no-proxy-server`, `--proxy-pac-url` or `--proxy-auto-detect` is
/// dropped with a warning that names the switch but not its value, and loopback requests always
/// go through the proxy.
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
    proxy: Option<&crate::proxy::ChromeProxy>,
) -> Result<BrowserConfigBuilder, CrawlError> {
    #[cfg(test)]
    let test_chrome_path = std::env::var_os("CRAWLBERG_TEST_CHROME_PATH").map(std::path::PathBuf::from);
    #[cfg(test)]
    let chrome_path = chrome_path.or(test_chrome_path.as_deref());
    crate::types::check_chrome_args(section, chrome_args).map_err(CrawlError::browser_error)?;
    if let Some(path) = chrome_path {
        crate::types::check_chrome_executable(section, path).map_err(CrawlError::browser_error)?;
        builder = builder.chrome_executable(path);
    }
    for arg in chrome_args {
        let switch = crate::types::chrome_switch_name(arg);
        if proxy.is_some() && PROXY_SWITCHES.contains(&switch) {
            tracing::warn!(
                flag = %format!("--{switch}"),
                "{section}.chrome_args sets a proxy switch that the configured proxy replaces; the flag is dropped"
            );
            continue;
        }
        builder = builder.arg(chrome_arg_key(arg.as_str()));
    }
    if let Some(proxy) = proxy {
        // ~keep No `--` prefix: chromiumoxide adds it. With one, this rendered as
        // ~keep `----proxy-server=...` and the proxy was silently never applied.
        builder = builder
            .arg(format!("proxy-server={}", proxy.server))
            .arg(format!("proxy-bypass-list={NO_LOOPBACK_BYPASS}"));
    }
    Ok(builder)
}

/// The Chrome switches that choose a launch's proxy. The configured proxy sets the first two;
/// Chrome reads the other three before `--proxy-server`, so each of them would replace it.
const PROXY_SWITCHES: [&str; 5] = [
    "proxy-server",
    "proxy-bypass-list",
    "no-proxy-server",
    "proxy-pac-url",
    "proxy-auto-detect",
];

/// How long a [`ScratchProfileDir`]'s teardown waits for the processes it killed to exit.
const PROFILE_USERS_EXIT_TIMEOUT: Duration = Duration::from_secs(5);
const PROFILE_USERS_POLL_INTERVAL: Duration = Duration::from_millis(20);
/// How long a [`ScratchProfileDir`]'s teardown keeps trying to remove the directory.
const PROFILE_REMOVAL_TIMEOUT: Duration = Duration::from_secs(2);

/// A Chrome `--user-data-dir` in the system temp directory, or in a snap's own directory for a
/// snap Chrome, removed when dropped.
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
/// ~keep A process that exits does not wait for that thread, and it drops no value that is still
/// ~keep alive, so each directory is also listed in [`LIVE_PROFILES`] until its teardown has
/// ~keep finished, and a hook that runs when the process exits finishes the rest
/// ~keep (xberg-io/crawlberg#594).
#[derive(Debug)]
pub(crate) struct ScratchProfileDir(Option<ProfileTeardown>);

/// A [`ScratchProfileDir`] whose teardown has not finished.
struct LiveProfile {
    /// The process that created the directory. A forked child inherits the list, and the
    /// directories in it are its parent's.
    owner: u32,
    /// The process a launch started on the directory, as [`ProfileTeardown::launched`].
    launched: Option<u32>,
    /// The process tree of that launch, as [`ProfileTeardown::tree`].
    tree: Option<chromiumoxide::async_process::ProcessTree>,
    /// The Chrome launched on the directory, as [`ProfileTeardown::chrome`].
    chrome: Option<std::path::PathBuf>,
    /// The turn of the directory's teardowns, as [`ProfileTeardown::turn`].
    turn: ProfileTurn,
}

/// Held by the one teardown of a profile directory that runs.
///
/// ~keep A dropped engine's teardown runs on a thread, and an exit right after the drop runs the
/// ~keep exit hook's teardown of the same directory beside it. The second found the main process
/// ~keep gone, waited for nothing, and removed the directory while the first still waited for the
/// ~keep helpers it had killed to finish their writes.
type ProfileTurn = Arc<std::sync::Mutex<()>>;

/// Set by the exit hook before it reads [`LIVE_PROFILES`]. From then on no profile directory is
/// listed and no Chrome is started: the hook does not run again for either.
static EXITING: AtomicBool = AtomicBool::new(false);

/// How long the exit hook waits for [`LIVE_PROFILES`], which a launch holds while it starts its
/// Chrome process.
const PROFILE_LIST_EXIT_WAIT: Duration = Duration::from_secs(1);

/// Every [`ScratchProfileDir`] whose teardown has not finished, by its directory.
static LIVE_PROFILES: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, LiveProfile>>,
> = std::sync::LazyLock::new(Default::default);

fn live_profiles() -> std::sync::MutexGuard<'static, std::collections::HashMap<std::path::PathBuf, LiveProfile>> {
    match LIVE_PROFILES.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// List `dir` in [`LIVE_PROFILES`], and on the first call register the hook that tears down
/// what is left in the list when the process exits. Returns `false`, and lists nothing, once that
/// hook has started.
///
/// ~keep The hook runs on a return from `main` and on `exit`, in any language that ends its
/// ~keep process through the C library. It does not run when the process is killed by a signal,
/// ~keep aborts, or leaves through `_exit`, nor on Windows when a Rust program calls
/// ~keep `std::process::exit`, which is `ExitProcess`. On Windows the system still ends the Chrome
/// ~keep of such a process, through the launch's process tree, and the directory stays.
#[allow(unsafe_code)]
fn list_live_profile(dir: &std::path::Path, turn: &ProfileTurn) -> bool {
    // ~keep Declared here: the `libc` crate is a dependency on iOS alone. Every C runtime that
    // ~keep `std` links against has `int atexit(void (*)(void))`.
    unsafe extern "C" {
        fn atexit(hook: extern "C" fn()) -> std::ffi::c_int;
    }
    static EXIT_HOOK: std::sync::Once = std::sync::Once::new();
    EXIT_HOOK.call_once(|| {
        // ~keep SAFETY: the declaration matches C's `atexit`, and `tear_down_live_profiles` lives
        // ~keep as long as this code is loaded, takes no argument and does not unwind.
        if unsafe { atexit(tear_down_live_profiles) } != 0 {
            tracing::warn!("no exit hook for Chrome profile teardown; a Chrome still running at exit is left");
        }
    });
    let mut profiles = live_profiles();
    if EXITING.load(Ordering::Acquire) {
        return false;
    }
    profiles.insert(
        dir.to_path_buf(),
        LiveProfile {
            owner: std::process::id(),
            launched: None,
            tree: None,
            chrome: None,
            turn: Arc::clone(turn),
        },
    );
    true
}

/// Start the Chrome of `config` on the listed directory `dir`, and record its process in
/// [`LIVE_PROFILES`] before the list is released.
///
/// ~keep The exit hook reads the list under the same lock, after it has set [`EXITING`], so it sees
/// ~keep either the process, or no process, and then none is started after it. A launch takes up
/// ~keep to its timeout to report the Chrome it started; an exit inside that time left the Chrome
/// ~keep running, and removed the directory under it.
fn start_listed_chrome(
    dir: &std::path::Path,
    config: &BrowserConfig,
) -> std::io::Result<chromiumoxide::async_process::Child> {
    let mut profiles = live_profiles();
    if EXITING.load(Ordering::Acquire) {
        return Err(std::io::Error::other("the process is exiting"));
    }
    // ~keep Not killed when its handle drops: the directory's teardown stops it, and finds its
    // ~keep helpers through it, which a main process that is already dead no longer leads to.
    let child = config.command().kill_on_drop(false).spawn()?;
    if let Some(profile) = profiles.get_mut(dir) {
        profile.launched = child.inner.id();
        profile.tree = child.tree().cloned();
    }
    Ok(child)
}

/// The teardown of every profile directory in `profiles` that the process `owner` created.
fn live_profile_teardowns_of(
    profiles: &std::collections::HashMap<std::path::PathBuf, LiveProfile>,
    owner: u32,
) -> Vec<ProfileTeardown> {
    profiles
        .iter()
        .filter(|(_, profile)| profile.owner == owner)
        .map(|(dir, profile)| ProfileTeardown {
            owner: profile.owner,
            dir: dir.clone(),
            launched: profile.launched,
            tree: profile.tree.clone(),
            chrome: profile.chrome.clone(),
            turn: Arc::clone(&profile.turn),
        })
        .collect()
}

/// The exit hook: tear down every profile directory this process still lists.
extern "C" fn tear_down_live_profiles() {
    // ~keep The list is waited for only as long as a launch can hold it: a thread that ended
    // ~keep while it held the lock, as every other thread has when a Windows process exits, would
    // ~keep hold up the exit for good. A panic must not unwind into the C library's exit.
    let _ = std::panic::catch_unwind(|| {
        EXITING.store(true, Ordering::Release);
        let left = lock_within(&LIVE_PROFILES, PROFILE_LIST_EXIT_WAIT)
            .map(|profiles| live_profile_teardowns_of(&profiles, std::process::id()))
            .unwrap_or_default();
        // ~keep Each teardown runs as it drops, after the lock is released.
        drop(left);
    });
}

impl ScratchProfileDir {
    /// Create a fresh directory for the Chrome at `chrome_path` (`None` for the one found on the
    /// machine), where that Chrome can read it. The random suffix avoids Chrome `SingletonLock`
    /// collisions.
    pub(crate) fn create(prefix: &str, chrome_path: Option<&std::path::Path>) -> Result<Self, CrawlError> {
        let failed =
            |e: std::io::Error| CrawlError::browser_error(format!("failed to create a Chrome profile directory: {e}"));
        let parent = scratch_profile_parent(chrome_path);
        std::fs::create_dir_all(&parent).map_err(failed)?;
        let dir = tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in(&parent)
            .map_err(failed)?;
        let turn = ProfileTurn::default();
        if !list_live_profile(dir.path(), &turn) {
            // ~keep `dir` removes itself as it drops here.
            return Err(CrawlError::browser_error(
                "no Chrome profile directory is created while the process exits",
            ));
        }
        Ok(Self(Some(ProfileTeardown {
            owner: std::process::id(),
            dir: dir.keep(),
            launched: None,
            tree: None,
            chrome: None,
            turn,
        })))
    }

    pub(crate) fn path(&self) -> &std::path::Path {
        &self.teardown().dir
    }

    /// Write the WebRTC preference into this directory, as the function of the same name does,
    /// unless the process is exiting.
    ///
    /// ~keep Under the list's lock, as a launch starts its Chrome: the exit hook removes the
    /// ~keep directory, and a write after that made it again, with `Default` in it and no Chrome
    /// ~keep (1 of 20 runs of an exit right after the directory was listed, on a loaded host).
    pub(crate) fn disable_non_proxied_udp(&self) -> Result<(), CrawlError> {
        let _profiles = live_profiles();
        if EXITING.load(Ordering::Acquire) {
            return Err(CrawlError::browser_error(
                "no Chrome profile is written while the process exits",
            ));
        }
        disable_non_proxied_udp(self.path())
    }

    /// Launch Chrome on this directory and record the Chrome it started, whose processes the
    /// teardown stops. A failed launch drops the directory, which removes it.
    pub(crate) async fn launch(
        mut self,
        config: BrowserConfig,
    ) -> Result<(Browser, Handler, Self), chromiumoxide::error::CdpError> {
        // ~keep Boxed: the launch future is large, and each caller's future holds it inline, which
        // ~keep pushed the generated dart binding's async dispatch past rustc's query depth limit.
        let teardown = self.0.as_mut().expect("the teardown is taken only once");
        let (browser, handler) = Box::pin(Browser::launch_with(config, |config| {
            let child = start_listed_chrome(&teardown.dir, config)?;
            teardown.launched = child.inner.id();
            teardown.tree = child.tree().cloned();
            Ok(child)
        }))
        .await?;
        if let Some(pid) = teardown.launched {
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
        if let Some(profile) = live_profiles().get_mut(&self.teardown().dir) {
            profile.chrome.clone_from(&chrome);
        }
        self.0.as_mut().expect("the teardown is taken only once").chrome = chrome;
    }

    fn teardown(&self) -> &ProfileTeardown {
        self.0.as_ref().expect("the teardown is taken only once")
    }

    /// Run the profile teardown on the blocking pool and wait until it has finished.
    async fn teardown_and_wait(mut self) {
        let Some(teardown) = self.0.take() else {
            return;
        };
        #[cfg(test)]
        tests::PROFILE_HAND_OFFS_HERE.with(|count| count.set(count.get() + 1));
        let pending = Arc::new(std::sync::Mutex::new(Some(teardown)));
        let worker_pending = Arc::clone(&pending);
        let joined = tokio::task::spawn_blocking(move || {
            drop(take_profile_teardown(&worker_pending));
        })
        .await;
        if let Err(error) = joined {
            // ~keep A blocking task cancelled before it starts leaves the teardown in `pending`;
            // run it here so explicit shutdown cannot return with Chrome still alive.
            tracing::warn!(%error, "Chrome profile teardown task failed; running it synchronously");
            drop(take_profile_teardown(&pending));
        }
    }
}

fn take_profile_teardown(pending: &std::sync::Mutex<Option<ProfileTeardown>>) -> Option<ProfileTeardown> {
    match pending.lock() {
        Ok(mut guard) => guard.take(),
        Err(poisoned) => poisoned.into_inner().take(),
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
///
/// ~keep It does that work only in the process that created the directory. A process forked from
/// ~keep that one holds a copy of every teardown and of [`LIVE_PROFILES`]. Dropped there, a copy
/// ~keep killed the Chrome of the parent by the directory's flag and removed the directory under
/// ~keep it, so in any other process the drop does nothing.
#[derive(Debug)]
struct ProfileTeardown {
    /// The process that created `dir`, as [`LiveProfile::owner`].
    owner: u32,
    dir: std::path::PathBuf,
    /// The process a launch started on `dir`, from the moment it exists. While it runs, the
    /// teardown stops it and every process below it.
    launched: Option<u32>,
    /// The process tree of that launch, where the operating system keeps one (Windows). It holds
    /// the Chrome and every helper from the moment the process exists, and the teardown stops
    /// them through it.
    tree: Option<chromiumoxide::async_process::ProcessTree>,
    /// The executables of the Chrome launched on `dir`, from [`chrome_started_by`]. `None` when no launch
    /// succeeded, so no Chrome of crawlberg's can be using the directory.
    chrome: Option<std::path::PathBuf>,
    /// Held while this teardown runs, so that a second teardown of the same directory, from the
    /// exit hook, starts only when this one has finished.
    turn: ProfileTurn,
}

/// Lock `mutex`, waiting at most `limit` for it. `None` when the time is up.
///
/// ~keep The wait has a limit wherever the exit hook can meet the lock: when a Windows process
/// ~keep exits, the system has ended the thread that held it, and nothing releases it.
fn lock_within<T>(mutex: &std::sync::Mutex<T>, limit: Duration) -> Option<std::sync::MutexGuard<'_, T>> {
    let deadline = std::time::Instant::now() + limit;
    loop {
        match mutex.try_lock() {
            Ok(held) => return Some(held),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => return Some(poisoned.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                std::thread::sleep(PROFILE_USERS_POLL_INTERVAL);
            }
            Err(std::sync::TryLockError::WouldBlock) => return None,
        }
    }
}

impl Drop for ProfileTeardown {
    fn drop(&mut self) {
        #[cfg(test)]
        tests::PROFILE_TEARDOWNS_HERE.with(|count| count.set(count.get() + 1));
        if self.owner != std::process::id() {
            return;
        }
        // ~keep With a tree the system knows every process of the launch, and stopping them
        // ~keep starts no process and searches for none. The other arms find them by the
        // ~keep directory, which on Windows meant a `taskkill.exe` child for each one: started
        // ~keep from the exit hook of a binding, after the system had ended every other thread,
        // ~keep that child never ran and the process did not exit.
        // ~keep One teardown of a directory at a time, for as long as one can take. A tree needs
        // ~keep no turn: every teardown waits on the same processes through it.
        let turn = Arc::clone(&self.turn);
        let _turn = if self.tree.is_none() {
            lock_within(&turn, PROFILE_USERS_EXIT_TIMEOUT + PROFILE_REMOVAL_TIMEOUT)
        } else {
            None
        };
        if let Some(tree) = &self.tree {
            if !tree.stop(PROFILE_USERS_EXIT_TIMEOUT) {
                tracing::warn!(dir = %self.dir.display(), "Chrome processes still run after their process tree was stopped");
            }
        } else {
            // ~keep First the launched process and every process below it, while it runs. Then
            // ~keep what still names the directory: the helpers of a Chrome whose main process
            // ~keep was gone before this teardown, after a crash or an explicit kill.
            if let Some(pid) = self.launched {
                stop_launched_family(&self.dir, pid);
            }
            if let Some(chrome) = &self.chrome {
                stop_chrome_processes_using(&self.dir, chrome);
            }
        }
        // ~keep The removal is tried again for a short time. On Windows a file stays undeletable
        // ~keep until the system has closed it for the process that held it, and a helper that
        // ~keep ended by itself when its browser was killed is in no process tree to wait on: the
        // ~keep one attempt left `Default` behind after a drop followed by an exit. A second
        // ~keep teardown of the same directory, from the exit hook, also removes entries under
        // ~keep this one.
        let deadline = std::time::Instant::now() + PROFILE_REMOVAL_TIMEOUT;
        while let Err(error) = std::fs::remove_dir_all(&self.dir) {
            if error.kind() == std::io::ErrorKind::NotFound {
                break;
            }
            if std::time::Instant::now() >= deadline {
                tracing::warn!(dir = %self.dir.display(), %error, "failed to remove the Chrome profile directory");
                break;
            }
            std::thread::sleep(PROFILE_USERS_POLL_INTERVAL);
        }
        live_profiles().remove(&self.dir);
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

/// Stop the launched process `pid` of the directory `dir` and every process below it, and wait
/// until each has ended, with all its threads.
///
/// ~keep The processes are found through their parent, from `pid` down, and each is stopped as it
/// ~keep is found, as for an explicit kill ([`ChromeFamily`]). A scan for the directory's flag
/// ~keep misses processes. The helpers of the Chromium headless shell do not carry the flag, so
/// ~keep after a dropped engine nothing waited for them, and under load one wrote into the
/// ~keep directory after its removal (6 of 40 runs). A process that is replacing its program, as
/// ~keep the launcher does when it execs Chrome, shows an empty command line, and a Chrome that is
/// ~keep starting forks helpers until it is killed (4 helpers seen only after the kill in one run).
/// ~keep `pid` counts only while it is a live child of this process: once it is reaped, the number
/// ~keep can belong to another process.
fn stop_launched_family(dir: &std::path::Path, pid: u32) {
    let launched = Pid::from_u32(pid);
    let mut system = System::new();
    ChromeFamily::refresh(&mut system, ProcessesToUpdate::Some(&[launched]));
    let this_process = Pid::from_u32(std::process::id());
    if !system
        .process(launched)
        .is_some_and(|process| process.parent() == Some(this_process) && ChromeFamily::is_running(&system, launched))
    {
        return;
    }
    let family = ChromeFamily::freeze(pid);
    family.kill();
    if !family.wait(PROFILE_USERS_EXIT_TIMEOUT) {
        tracing::warn!(dir = %dir.display(), "Chrome processes still run after their process family was killed");
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
        .manage_child_targets(false)
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
        None,
    )
}

/// Configuration for a [`BrowserPool`].
///
/// Rust-only: this type is excluded from alef-generated polyglot bindings.
/// Pool reuse is intended for long-lived Rust processes (e.g. the cloud
/// worker); language bindings construct compatible pools internally per engine.
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
    /// and port print. See `crate::net::redact::redact_url_to_origin`. A Chrome flag can carry
    /// a proxy password, so `chrome_args` print through `RedactedChromeArgs`.
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
            .field("chrome_args", &crate::chrome_args::RedactedChromeArgs(chrome_args))
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
    generation: u64,
    browser: Arc<Browser>,
    /// The SSRF check every page of this browser runs under. It holds the other reference
    /// to `browser` until it is stopped.
    firewall: BrowserFirewall,
    handler_handle: JoinHandle<()>,
    /// Whether the handler has ended, which is when the pool replaces this browser.
    handler_end: HandlerEnd,
    user_data_dir: Option<ScratchProfileDir>,
    pending_closes: PendingCloses,
    /// Set once the pool has logged that this browser's sockets go unchecked.
    remote_warned: std::sync::Once,
}

/// How a browser left [`close_browser_within`]: under its own steam, or killed.
///
/// ~keep This distinction is the whole point of the return value: a killed Chrome never
/// ~keep answers the CDP `Browser.close` its handler loop is waiting on, so teardown has to
/// ~keep treat the two cases differently (xberg-io/crawlberg#146). `#[must_use]` sits on the
/// ~keep type so a call site that discards the outcome is a warning, not a silent wait of up to
/// ~keep 5 seconds.
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BrowserCloseOutcome {
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
        if let Some(user_data_dir) = self.user_data_dir {
            user_data_dir.teardown_and_wait().await;
        }
    }

    /// Terminate a browser whose security controller disappeared. A launched Chrome is killed
    /// rather than given a graceful CDP close, because the failed handler cannot acknowledge it. ~keep
    async fn fail_closed(self) {
        self.firewall.stop().await;
        let Some(browser) = Arc::into_inner(self.browser) else {
            self.handler_handle.abort();
            if let Some(user_data_dir) = self.user_data_dir {
                user_data_dir.teardown_and_wait().await;
            }
            return;
        };
        match self.user_data_dir {
            Some(user_data_dir) => {
                let profile = user_data_dir.path().to_path_buf();
                kill_browser(browser, self.handler_handle, profile, HANDLER_SHUTDOWN_TIMEOUT).await;
                drop(user_data_dir);
            }
            None => {
                release_browser(
                    browser,
                    self.handler_handle,
                    ExternalTabCleanup {
                        pending_closes: Some(self.pending_closes),
                        ..ExternalTabCleanup::default()
                    },
                    HANDLER_SHUTDOWN_TIMEOUT,
                )
                .await;
            }
        }
    }
}

/// How often the task that runs a CDP handler polls it while the connection is quiet.
///
/// ~keep chromiumoxide 0.9.1 fails a command that got no answer from a periodic job inside
/// ~keep `Handler::poll_next` (`src/handler/mod.rs`). The job's timer (`src/handler/job.rs`) is
/// ~keep reset when it fires and is not polled again in that pass, so it has no waker until
/// ~keep something else polls the handler. On a quiet connection nothing does, and a command
/// ~keep Chrome never answered waited forever (xberg-io/crawlberg#586). A poll on this interval
/// ~keep gives the timer its waker again. The command then fails with `CdpError::Timeout` when
/// ~keep the timer next fires after the request timeout has passed, so within twice that timeout.
/// ~keep `StreamExt::next` only borrows the handler, so a poll that a tick drops loses nothing.
/// ~keep The cost is one wake each second for each browser.
const HANDLER_WAKE_INTERVAL: Duration = Duration::from_secs(1);

/// Run the CDP handler of a browser on its own task, until its websocket fails.
///
/// ~keep chromiumoxide 0.9.1's `Handler::poll_next` (`src/handler/mod.rs`) reports a broken
/// ~keep websocket, as when Chrome dies, as one `CdpError::Ws` error, and then returns
/// ~keep `Poll::Pending` for good without failing the commands still waiting on it. Its 30 s request
/// ~keep timeout is checked only when the handler wakes, which a dead connection never does again. A
/// ~keep loop that went on past the error kept the handler, and with it every pending command, alive:
/// ~keep an interact session whose Chrome died waited forever for the reply to the dispose of its
/// ~keep page's context (xberg-io/crawlberg#577). Ending the task drops the handler, so every command
/// ~keep and event stream of the browser ends with an error at once. The handler's other errors leave
/// ~keep the connection usable (a binary frame is `CdpError::UnexpectedWsMessage`, `src/conn.rs`), so
/// ~keep the loop goes on past them. Every `CdpError::Ws` is final here: async-tungstenite 0.32.1
/// ~keep ends its stream at any read error (`WebSocketStream::poll_next` sets `ended`), and the
/// ~keep write errors that leave a socket open (`Capacity`, `WriteBufferFull`) cannot happen,
/// ~keep because chromiumoxide sets no message or frame limit (`src/conn.rs`) and tungstenite
/// ~keep 0.28.0's write buffer has none. The pool launches a new browser only once this handler has
/// ~keep ended, so a parked loop also kept a crashed Chrome in the pool for good
/// ~keep (xberg-io/crawlberg#581). A websocket closed with a Close handshake gives no error: the
/// ~keep handler stays pending until the next command, whose send fails with `CdpError::Ws`, and
/// ~keep the loop ends then.
pub(crate) fn spawn_handler(handler: Handler) -> JoinHandle<()> {
    spawn_watched_handler(handler).0
}

/// [`spawn_handler`], with the state that tells whether the handler has ended.
pub(crate) fn spawn_watched_handler(handler: Handler) -> (JoinHandle<()>, HandlerEnd) {
    spawn_handler_then(handler, std::future::ready(()))
}

/// [`spawn_watched_handler`], whose task awaits `after_end` once the handler is dropped. A pool
/// test holds the task open there.
fn spawn_handler_then(
    handler: Handler,
    after_end: impl std::future::Future<Output = ()> + Send + 'static,
) -> (JoinHandle<()>, HandlerEnd) {
    let end = HandlerEnd::default();
    let mut watched = WatchedHandler {
        end: end.clone(),
        handler,
    };
    let handle = tokio::spawn(async move {
        loop {
            let Ok(event) = tokio::time::timeout(HANDLER_WAKE_INTERVAL, watched.handler.next()).await else {
                continue;
            };
            let Some(event) = event else {
                break;
            };
            if let Err(chromiumoxide::error::CdpError::Ws(error)) = &event {
                let cause = websocket_error_text(error);
                tracing::warn!(error = %cause, "the browser's CDP websocket failed; its CDP handler ends");
                let _ = watched.end.0.cause.set(cause);
                break;
            }
        }
        drop(watched);
        after_end.await;
    });
    (handle, end)
}

/// Whether the CDP handler of a browser has ended: its loop stopped or its task was aborted.
///
/// ~keep Dropping the handler is what fails the commands that wait on it, and a task counts as
/// ~keep finished only after its future is dropped. A page request woken by its failed command
/// ~keep on another thread read `JoinHandle::is_finished` as false, so the pool kept the dead
/// ~keep browser for that request and its retry failed (xberg-io/crawlberg#581). This state is
/// ~keep set before the handler drops, so a caller that sees a command fail for that reason
/// ~keep always sees it set.
#[derive(Clone, Debug, Default)]
pub(crate) struct HandlerEnd(Arc<HandlerEndState>);

#[derive(Debug, Default)]
struct HandlerEndState {
    ended: AtomicBool,
    notification: tokio::sync::Notify,
    cause: std::sync::OnceLock<String>,
}

impl HandlerEnd {
    pub(crate) fn has_ended(&self) -> bool {
        self.0.ended.load(Ordering::Acquire)
    }

    async fn ended(&self) {
        loop {
            let notified = self.0.notification.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.has_ended() {
                return;
            }
            notified.await;
        }
    }

    /// The websocket error the handler stopped on. `None` while it runs, and after an abort.
    ///
    /// ~keep Its callers are the one-shot fetch in `browser.rs`, which the wider `browser` feature
    /// ~keep compiles, and a test of this module. Under `browser-chromiumoxide` alone it has no
    /// ~keep caller and would be dead code.
    #[cfg(any(feature = "browser", test))]
    pub(crate) fn cause(&self) -> Option<&str> {
        self.0.cause.get().map(String::as_str)
    }
}

/// What a log line and an error message say about the websocket error a handler stopped on: its
/// `Display`, and the kind of the I/O error under it when there is one.
///
/// ~keep Never its `Debug`: for tungstenite's `WriteBufferFull` that prints the whole CDP message,
/// ~keep which can hold a URL with credentials. The `Display` of an error from an open connection
/// ~keep (`Io`, `Protocol`, `Capacity`, `Utf8`) holds no message text and no URL.
fn websocket_error_text(error: &(dyn std::error::Error + 'static)) -> String {
    let kind = std::iter::successors(Some(error), |error| error.source())
        .find_map(|error| error.downcast_ref::<std::io::Error>())
        .map(std::io::Error::kind);
    match kind {
        Some(kind) => format!("{error} ({kind:?})"),
        None => error.to_string(),
    }
}

/// A handler that sets its [`HandlerEnd`] before it drops.
///
/// ~keep `Drop::drop` runs before the fields drop, so `end` is set before `handler` drops and
/// ~keep fails its commands, whether the loop stopped or the task was aborted.
struct WatchedHandler {
    end: HandlerEnd,
    handler: Handler,
}

impl Drop for WatchedHandler {
    fn drop(&mut self) {
        self.end.0.ended.store(true, Ordering::Release);
        self.end.0.notification.notify_waiters();
    }
}

/// Stop the task running the CDP handler loop of a browser that has just been closed.
///
/// A browser that exited on its own ends its handler loop, so that case waits briefly for the
/// loop to finish and aborts it only if it overruns. A killed browser has nothing left to send,
/// so its handler is aborted at once.
///
/// ~keep Dropping a `JoinHandle` detaches its task rather than stopping it, so simply
/// ~keep discarding the timeout result leaked one handler loop per relaunch — unbounded
/// ~keep for a domain that keeps crashing Chrome.
/// ~keep The `Killed` shortcut does not wait on the handler to notice the kill: chromiumoxide
/// ~keep 0.9.1's `Handler::poll_next` (`src/handler/mod.rs`) returns `Ready(None)` only when a
/// ~keep `Browser.close` response arrives while it is `closing`, which a killed Chrome never sends.
/// ~keep A loop that went on past the websocket error then parked for good, and this wait burned
/// ~keep the full `HANDLER_SHUTDOWN_TIMEOUT` (xberg-io/crawlberg#146). [`spawn_handler`] now ends
/// ~keep at that error (xberg-io/crawlberg#581), but the process is gone either way, so the abort
/// ~keep does not depend on the error arriving.
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
    let handler_config = chromiumoxide::handler::HandlerConfig {
        manage_child_targets: false,
        ..chromiumoxide::handler::HandlerConfig::default()
    };
    Box::pin(Browser::connect_with_config(address, handler_config))
        .await
        .map_err(|e| {
            let redacted = crate::net::redact::redact_url_to_origin(endpoint);
            CrawlError::browser_error(format!("failed to connect to {redacted}: {e}"))
        })
}

/// Tear down `browser` and the task that runs its CDP handler.
///
/// A Chrome that crawlberg launched gets `shutdown_timeout` to close and exit, and is then
/// force-killed (see `close_browser_within`); closing it removes every tab it has. A Chrome reached through
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
/// instead of waiting on a handler loop that a killed process will never close cleanly.
pub(crate) async fn close_browser_within(browser: &mut Browser, shutdown_timeout: Duration) -> BrowserCloseOutcome {
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

/// Kill `browser`, a Chrome crawlberg launched with the throwaway profile `profile`, and every
/// process it started, then remove the profile once none of them is left, or once what is left
/// of `shutdown_timeout` has passed. A kill that fails falls back to [`release_browser`]'s close.
///
/// ~keep `Browser::kill` kills and reaps only the main process. Its renderers and helpers exit
/// ~keep on their own a moment later and keep writing into the profile until then, so a removal
/// ~keep right after the kill left the directory behind for about half of all sessions
/// ~keep (xberg-io/crawlberg#468). They are collected BEFORE the kill, through each process's
/// ~keep parent. Found afterwards by their command lines, a process whose line was not readable
/// ~keep yet, or one forked meanwhile, was missed, and the removal raced it: 1 run in 12 under
/// ~keep load left a profile of 71 files.
pub(crate) async fn kill_browser(
    mut browser: Browser,
    handler_handle: JoinHandle<()>,
    profile: std::path::PathBuf,
    shutdown_timeout: Duration,
) {
    let deadline = tokio::time::Instant::now() + shutdown_timeout;
    // ~keep The driver hands out the process only where it was launched. A process forked from
    // ~keep that one holds a copy of the browser: it lets go of the copy and leaves the Chrome
    // ~keep and the profile to its parent.
    if browser.get_mut_child().is_none() {
        release_browser(browser, handler_handle, ExternalTabCleanup::default(), shutdown_timeout).await;
        return;
    }
    let main = browser.get_mut_child().and_then(|child| child.as_mut_inner().id());
    let family = match main {
        Some(main) => tokio::task::spawn_blocking(move || {
            let family = ChromeFamily::freeze(main);
            family.kill();
            family
        })
        .await
        .unwrap_or_default(),
        None => ChromeFamily::default(),
    };
    match browser.kill().await {
        Some(Ok(())) => handler_handle.abort(),
        outcome => {
            tracing::warn!(?outcome, "failed to kill the browser; closing it instead");
            release_browser(browser, handler_handle, ExternalTabCleanup::default(), shutdown_timeout).await;
        }
    }
    let limit = deadline.saturating_duration_since(tokio::time::Instant::now());
    let gone = tokio::task::spawn_blocking(move || family.wait(limit))
        .await
        .unwrap_or(false);
    if !gone {
        tracing::warn!(
            dir = %profile.display(),
            timeout_secs = shutdown_timeout.as_secs_f64(),
            "Chrome processes were still running at the shutdown timeout"
        );
    }
    remove_profile_dir(profile).await;
}

/// The processes of a Chrome crawlberg launched: the main process and every descendant of it,
/// collected before the kill and stopped as they are found, so the set is complete when they
/// are killed.
///
/// ~keep chromiumoxide 0.9.1 spawns Chrome as a plain child, and Chrome makes no process group
/// ~keep of its own (every helper carries the spawner's group; measured on Chrome 154), so the
/// ~keep OS offers no one signal for the whole tree, and a process found after its parent died
/// ~keep has been given to `init`. The tree is walked through each process's parent instead,
/// ~keep every member is stopped as it is found, and the walks repeat until one finds no new
/// ~keep member between two reads that find every member stopped: a stopped process cannot
/// ~keep fork, so none is missed. The walks are capped for a platform without `Signal::Stop`
/// ~keep (Windows), where a process that keeps forking would never settle.
/// ~keep The wait is bounded by what is left of `shutdown_timeout` after the kill. The
/// ~keep collection, the kill and the reap of the main process before it, and the removal of the
/// ~keep profile after it, are not, so a kill can take longer than `shutdown_timeout` in all
/// ~keep (measured up to 38 s on a 5 s timeout at loads of 700 to 1180).
#[derive(Default)]
struct ChromeFamily {
    members: Vec<Pid>,
}

impl ChromeFamily {
    /// The most walks over the process table before the family is taken as complete.
    const WALKS: usize = 8;
    /// The longest wait for a stopped member to take its stop, per walk.
    const SETTLE: Duration = Duration::from_secs(1);

    fn refresh(system: &mut System, which: ProcessesToUpdate<'_>) {
        system.refresh_processes_specifics(which, true, ProcessRefreshKind::nothing().without_tasks());
    }

    /// Collect the family of the process `main`, stopping each member as it is found. Every member
    /// has taken its stop, or is gone, when this returns, with two exceptions, and each is logged:
    /// a member that did not take its stop in two waits, and a family that did not come to rest
    /// within [`Self::WALKS`] walks.
    fn freeze(main: u32) -> Self {
        Self::freeze_with(main, Self::settle)
    }

    /// [`Self::freeze`], waiting for the stops through `settle`, so that a test can act on the
    /// family between a wait and the walk after it.
    ///
    /// ~keep The family is at rest after a walk that finds no new member, when the wait before
    /// ~keep it found every member stopped and a read after it finds every member stopped. A
    /// ~keep stop does not always hold. On macOS, a stop sent to a process that is starting a
    /// ~keep program can be lost: the process never stops, or reads as stopped and then runs
    /// ~keep again. A probe that walks and stops a forking shell family the same way found a
    /// ~keep member that ran after its stop in 8 of 840 runs on macOS 14, and in 2 of them a
    /// ~keep child of that member that no walk saw (xberg-io/crawlberg#585). So a member that
    /// ~keep runs after a walk gets the stop again, and the collection waits and walks once more.
    /// ~keep A member can also be unable to take a stop. On Linux, a process inside `vfork`
    /// ~keep sleeps in the kernel until its child starts a program, and the child is stopped
    /// ~keep too. Such a member reads as running in every wait, and waiting for it in all eight
    /// ~keep walks took 7.1 s. The collection ends without it when two waits in a row ran out
    /// ~keep and the walk after each found no new member, which takes about 2 s.
    /// ~keep One window remains: a member that runs after the last read is not seen.
    fn freeze_with(main: u32, mut settle: impl FnMut(&[Pid]) -> bool) -> Self {
        let mut system = System::new();
        let mut members: Vec<Pid> = Vec::new();
        let mut stopping: Vec<Pid> = Vec::new();
        let mut waits_run_out = 0;
        for _ in 0..Self::WALKS {
            let stopped = settle(&stopping);
            Self::refresh(&mut system, ProcessesToUpdate::All);
            let mut found = vec![Pid::from_u32(main)];
            let mut next = 0;
            while next < found.len() {
                let parent = found[next];
                found.extend(
                    system
                        .processes()
                        .values()
                        .filter(|process| process.parent() == Some(parent))
                        .map(Process::pid),
                );
                next += 1;
            }
            let new: Vec<&Process> = found
                .iter()
                .filter(|pid| !members.contains(pid))
                .filter_map(|pid| system.process(*pid))
                .collect();
            // ~keep A member that runs after the walk gets the stop again here, and stays in
            // ~keep the list that the next wait and the next read are for.
            let running = Self::running(&stopping, |process| {
                process.kill_with(Signal::Stop);
            });
            if new.is_empty() && stopped && running == 0 {
                return Self { members };
            }
            waits_run_out = if stopped || !new.is_empty() {
                0
            } else {
                waits_run_out + 1
            };
            if waits_run_out == 2 {
                tracing::warn!(
                    main,
                    members = members.len(),
                    "a Chrome process did not take its stop in two waits; a child it forks later can outlive the kill"
                );
                return Self { members };
            }
            for process in new {
                // ~keep A process gone by the time it is stopped is no member: its pid can be
                // ~keep reused, and the kill must never reach a stranger. A platform without the
                // ~keep stop signal keeps the member and does not wait for a stop it cannot send.
                match process.kill_with(Signal::Stop) {
                    Some(true) => {
                        members.push(process.pid());
                        stopping.push(process.pid());
                    }
                    None => members.push(process.pid()),
                    Some(false) => {}
                }
            }
        }
        tracing::warn!(
            main,
            members = members.len(),
            walks = Self::WALKS,
            "a Chrome process family did not come to rest; a process it forked meanwhile can outlive the kill"
        );
        Self { members }
    }

    /// Wait until every process in `stopping` has taken its stop or is gone, for at most
    /// [`Self::SETTLE`]. Returns whether every one has.
    ///
    /// ~keep A stop is taken when the process next runs, and under load that is later than the
    /// ~keep next walk: 50 ms after the collection, 4 of some 60 members still ran in 4 runs of
    /// ~keep 20 at a load of 20. A member still running may be inside a fork it began; the child
    /// ~keep is in the table before the parent returns from the fork, and the parent takes the
    /// ~keep stop only on that return, so a walk taken once every member has stopped finds every
    /// ~keep child, and one taken before that can miss one. The bound covers a process in an
    /// ~keep uninterruptible sleep, which takes the stop only when it wakes. After a wait that
    /// ~keep ran out the collection goes on: see [`Self::freeze_with`] for when it ends.
    fn settle(stopping: &[Pid]) -> bool {
        let deadline = std::time::Instant::now() + Self::SETTLE;
        while Self::running(stopping, |_| {}) != 0 {
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        true
    }

    /// Call `each` with every process in `stopping` that runs: it has not taken its stop and is
    /// not gone. Returns how many run.
    ///
    /// ~keep The status is read into a new `System` each time. On macOS, sysinfo 0.39 reads the
    /// ~keep status of a process only when it first sees the process. After that it reports
    /// ~keep the state of a thread, and reports a state it cannot read as running, so a kept
    /// ~keep `System` never shows a stop there.
    fn running(stopping: &[Pid], mut each: impl FnMut(&Process)) -> usize {
        let mut system = System::new();
        Self::refresh(&mut system, ProcessesToUpdate::Some(stopping));
        stopping
            .iter()
            .filter_map(|pid| system.process(*pid))
            .filter(|process| {
                !matches!(
                    process.status(),
                    ProcessStatus::Stop | ProcessStatus::Zombie | ProcessStatus::Dead
                )
            })
            .inspect(|process| each(process))
            .count()
    }

    /// Kill every member.
    fn kill(&self) {
        let mut system = System::new();
        Self::refresh(&mut system, ProcessesToUpdate::Some(&self.members));
        for pid in &self.members {
            if let Some(process) = system.process(*pid) {
                process.kill_with(Signal::Kill);
            }
        }
    }

    /// Wait until no member is left, for at most `limit`. Returns whether none is left.
    ///
    /// ~keep A killed process is not gone while a thread of it still runs. Its main thread
    /// ~keep exits first and the process reads as a zombie from then on, while a thread blocked
    /// ~keep in a write to the profile completes that write once the disk answers: under load,
    /// ~keep 80 of 338 waits found a zombie member with threads still running or in disk sleep,
    /// ~keep cache entries were created for two seconds after every member read as a zombie,
    /// ~keep and the removal found a directory not empty. A member is gone once no thread of it
    /// ~keep is left. The threads are refreshed by their own ids: a refresh of the members alone
    /// ~keep lists their threads but does not read them. Where threads cannot be listed, the
    /// ~keep member is gone once the process itself is.
    fn wait(&self, limit: Duration) -> bool {
        let deadline = std::time::Instant::now() + limit;
        let mut system = System::new();
        loop {
            system.refresh_processes_specifics(
                ProcessesToUpdate::Some(&self.members),
                true,
                ProcessRefreshKind::nothing(),
            );
            let mut alive = self.members.iter().any(|pid| Self::is_running(&system, *pid));
            if !alive {
                let threads: Vec<Pid> = self
                    .members
                    .iter()
                    .filter_map(|pid| system.process(*pid))
                    .filter_map(Process::tasks)
                    .flatten()
                    .copied()
                    .collect();
                Self::refresh(&mut system, ProcessesToUpdate::Some(&threads));
                alive = threads.iter().any(|pid| Self::is_running(&system, *pid));
            }
            if !alive {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Whether the process or thread `pid` is in the table and neither a zombie nor dead.
    fn is_running(system: &System, pid: Pid) -> bool {
        system
            .process(pid)
            .is_some_and(|process| !matches!(process.status(), ProcessStatus::Zombie | ProcessStatus::Dead))
    }
}

/// Remove a Chrome profile directory, logging rather than ignoring a failure.
///
/// ~keep `std::fs::remove_dir_all` here ran a recursive delete on the executor thread
/// ~keep while the pool's state mutex was held, stalling every waiting `acquire_page`.
async fn remove_profile_dir(dir: std::path::PathBuf) {
    if let Err(error) = tokio::fs::remove_dir_all(&dir).await {
        tracing::warn!(
            dir = %dir.display(),
            %error,
            "failed to remove the Chrome profile directory"
        );
    }
}

/// A pool that keeps a single Chrome browser alive and hands out pages (tabs),
/// limiting concurrency via a semaphore. If Chrome crashes the pool will
/// attempt to relaunch on the next [`acquire_page`](Self::acquire_page) call.
///
/// Rust-only: excluded from alef-generated polyglot bindings. Crawlberg constructs one
/// internally for each compatible binding engine, so downstream language clients do not
/// manage a pool themselves.
pub struct BrowserPool {
    config: BrowserPoolConfig,
    state: Arc<Mutex<Option<BrowserState>>>,
    page_semaphore: Arc<Semaphore>,
    shutdown: Arc<AtomicBool>,
    /// Lock-free health signal updated whenever browser state changes.
    healthy: Arc<AtomicBool>,
    next_generation: AtomicU64,
    /// While `true`, the task of each handler this pool starts stays open after its handler
    /// has ended, so a test can ask for a page in that state.
    #[cfg(test)]
    hold_handler_end: tokio::sync::watch::Sender<bool>,
}

fn mark_unhealthy_if_current(current_generation: Option<u64>, failed_generation: u64, healthy: &AtomicBool) -> bool {
    if current_generation != Some(failed_generation) {
        return false;
    }
    healthy.store(false, Ordering::Release);
    true
}

impl BrowserPool {
    /// Create a new pool. Chrome is **not** launched until the first call to
    /// [`acquire_page`](Self::acquire_page) or [`warm`](Self::warm).
    pub fn new(config: BrowserPoolConfig) -> Arc<Self> {
        let semaphore = Arc::new(Semaphore::new(config.max_pages));
        Arc::new(Self {
            config,
            state: Arc::new(Mutex::new(None)),
            page_semaphore: semaphore,
            shutdown: Arc::new(AtomicBool::new(false)),
            healthy: Arc::new(AtomicBool::new(false)),
            next_generation: AtomicU64::new(1),
            #[cfg(test)]
            hold_handler_end: tokio::sync::watch::Sender::new(false),
        })
    }

    #[cfg_attr(not(feature = "browser"), allow(dead_code))]
    pub(crate) fn uses_launch_options(&self, browser: &crate::types::BrowserConfig) -> bool {
        // ~keep An external endpoint owns its process flags even if both configs happen to
        // ~keep retain identical values, so those values still need the ignored-option warning.
        self.config.browser_endpoint.is_none()
            && browser.endpoint.is_none()
            && self.config.chrome_path == browser.chrome_path
            && self.config.chrome_args == browser.chrome_args
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
            self.supervise_current(guard.as_ref().expect("browser state was just installed"));
        }
        Ok(())
    }

    /// Acquire a new blank page from the pool.
    ///
    /// Blocks asynchronously if `max_pages` pages are already open. The page
    /// should be closed via [`PooledPage::close`] when done; if dropped
    /// without calling `close`, a best-effort async cleanup is spawned.
    pub async fn acquire_page(&self) -> Result<PooledPage, CrawlError> {
        self.acquire_page_with_config(&crate::types::CrawlConfig::default())
            .await
    }

    /// Acquire a blank page protected by `config`'s SSRF policy.
    pub async fn acquire_page_with_config(&self, config: &crate::types::CrawlConfig) -> Result<PooledPage, CrawlError> {
        let proxy = crate::proxy::chrome_proxy_for(config)?;
        self.acquire_page_through(proxy.as_ref(), config).await
    }

    /// Acquire a new blank page whose requests go through `proxy`: the page's own browser
    /// context is made with that proxy, behind the SSRF proxy for `policy` when there is one.
    pub(crate) async fn acquire_page_through(
        &self,
        proxy: Option<&crate::proxy::ChromeProxy>,
        config: &crate::types::CrawlConfig,
    ) -> Result<PooledPage, CrawlError> {
        if self.shutdown.load(Ordering::SeqCst) {
            return Err(CrawlError::browser_error("pool is shut down"));
        }
        let _ = crate::net::egress::socket_policy(
            &config.ssrf,
            self.config.browser_endpoint.as_deref(),
            &std::sync::Once::new(),
        )?;

        let permit = self
            .page_semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| CrawlError::browser_error("page semaphore closed"))?;

        if self.shutdown.load(Ordering::SeqCst) {
            return Err(CrawlError::browser_error("pool is shut down"));
        }

        match self.try_new_page(proxy, config).await {
            Ok((page, watch, pending_closes)) => Ok(PooledPage {
                page: Some(page),
                watch: Some(watch),
                _permit: Some(permit),
                pending_closes: Some(pending_closes),
            }),
            Err(first_err) => {
                self.relaunch_browser().await?;
                let (page, watch, pending_closes) = self.try_new_page(proxy, config).await.map_err(|e| {
                    CrawlError::browser_error(format!(
                        "failed to open page after relaunch: {e} (original: {first_err})"
                    ))
                })?;
                Ok(PooledPage {
                    page: Some(page),
                    watch: Some(watch),
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
    async fn try_new_page(
        &self,
        proxy: Option<&crate::proxy::ChromeProxy>,
        config: &crate::types::CrawlConfig,
    ) -> Result<(chromiumoxide::Page, crate::ssrf_intercept::Watch, PendingCloses), CrawlError> {
        let mut guard = self.state.lock().await;

        if guard.is_none()
            || guard
                .as_ref()
                .is_some_and(|bs| bs.handler_end.has_ended() || bs.firewall.has_failed())
        {
            self.healthy.store(false, Ordering::Release);
            if let Some(old) = guard.take() {
                old.firewall.stop().await;
                old.handler_handle.abort();
            }
            let bs = self.launch_browser().await?;
            *guard = Some(bs);
            self.healthy.store(true, Ordering::Release);
            self.supervise_current(guard.as_ref().expect("browser state was just installed"));
        }

        let bs = guard.as_ref().expect("browser state was just set above");
        let sockets = crate::net::egress::socket_policy(
            &config.ssrf,
            self.config.browser_endpoint.as_deref(),
            &bs.remote_warned,
        )?;
        let page = tokio::time::timeout(
            PAGE_OPEN_TIMEOUT,
            bs.firewall.handle().new_page_with_policy(proxy, sockets, &config.ssrf),
        )
        .await
        .map_err(|_| CrawlError::browser_error("timeout opening page"))??;
        let watch = bs.firewall.handle().watch(&page, config, config.max_redirects).await?;
        Ok((page, watch, Arc::clone(&bs.pending_closes)))
    }

    /// Force-relaunch Chrome (used after a page-open failure).
    async fn relaunch_browser(&self) -> Result<(), CrawlError> {
        let mut guard = self.state.lock().await;

        if self.shutdown.load(Ordering::SeqCst) {
            return Err(CrawlError::browser_error("pool is shut down"));
        }

        if guard
            .as_ref()
            .is_some_and(|bs| !bs.handler_end.has_ended() && !bs.firewall.has_failed())
        {
            return Ok(());
        }

        self.healthy.store(false, Ordering::Release);
        if let Some(old) = guard.take() {
            old.close().await;
        }

        let bs = self.launch_browser().await?;
        *guard = Some(bs);
        self.healthy.store(true, Ordering::Release);
        self.supervise_current(guard.as_ref().expect("browser state was just installed"));
        Ok(())
    }

    /// Retire a browser as soon as either half of its security controller ends. The generation
    /// check prevents an old supervisor from taking a replacement browser. ~keep
    ///
    /// ~keep The task holds the pool's state weakly. A strong reference kept the browser of a
    /// ~keep dropped pool, and so its Chrome, for as long as that Chrome ran.
    fn supervise_current(&self, current: &BrowserState) {
        let generation = current.generation;
        let handler_end = current.handler_end.clone();
        let firewall = current.firewall.handle();
        let state = Arc::downgrade(&self.state);
        let healthy = Arc::clone(&self.healthy);
        tokio::spawn(async move {
            tokio::select! {
                () = handler_end.ended() => {}
                () = firewall.failed() => {}
            }
            let Some(state) = state.upgrade() else {
                return;
            };
            let failed = {
                let mut guard = state.lock().await;
                if mark_unhealthy_if_current(guard.as_ref().map(|browser| browser.generation), generation, &healthy) {
                    guard.take()
                } else {
                    None
                }
            };
            if let Some(failed) = failed {
                failed.fail_closed().await;
            }
        });
    }

    /// Launch (or connect to) a Chrome process according to the pool config.
    async fn launch_browser(&self) -> Result<BrowserState, CrawlError> {
        let (browser, handler, data_dir) = if let Some(ref endpoint) = self.config.browser_endpoint {
            let (browser, handler) = tokio::time::timeout(self.config.launch_timeout, connect_endpoint(endpoint))
                .await
                .map_err(|_| CrawlError::browser_error("timeout connecting to browser endpoint"))??;
            (browser, handler, None)
        } else {
            // ~keep Dropped, and so removed, on every early return below, including a launch timeout.
            let user_data_dir = ScratchProfileDir::create("crawlberg-chrome-", self.config.chrome_path.as_deref())?;
            user_data_dir.disable_non_proxied_udp()?;
            let builder = build_pool_launch_builder(user_data_dir.path(), &self.config)?;
            let browser_config = builder
                .build()
                .map_err(|e| CrawlError::browser_error(format!("invalid browser config: {e}")))?;

            let (mut browser, handler, user_data_dir) =
                tokio::time::timeout(self.config.launch_timeout, user_data_dir.launch(browser_config))
                    .await
                    .map_err(|_| CrawlError::browser_error("timeout launching Chrome"))?
                    .map_err(|e| CrawlError::browser_error(format!("failed to launch Chrome: {e}")))?;
            confirm_profile_in_use(&mut browser, user_data_dir.path()).await?;
            (browser, handler, Some(user_data_dir))
        };

        #[cfg(not(test))]
        let (handler_handle, handler_end) = spawn_watched_handler(handler);
        #[cfg(test)]
        let (handler_handle, handler_end) = {
            let mut hold = self.hold_handler_end.subscribe();
            spawn_handler_then(handler, async move {
                let _ = hold.wait_for(|held| !*held).await;
            })
        };
        let browser = Arc::new(browser);
        let firewall = match BrowserFirewall::start_supervised(
            Arc::clone(&browser),
            BrowserOrigin::of_endpoint(self.config.browser_endpoint.as_deref()),
            PageContext::of_endpoint(self.config.browser_endpoint.as_deref()),
            handler_handle.abort_handle(),
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
            generation: self.next_generation.fetch_add(1, Ordering::Relaxed),
            browser,
            firewall,
            handler_handle,
            handler_end,
            user_data_dir: data_dir,
            pending_closes: Arc::new(std::sync::Mutex::new(Vec::new())),
            remote_warned: std::sync::Once::new(),
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
    watch: Option<crate::ssrf_intercept::Watch>,
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
        if let Some(watch) = self.watch.take() {
            self.page.take();
            watch.close().await;
        } else if let Some(page) = self.page.take() {
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
    pub(crate) fn into_parts(
        mut self,
    ) -> (
        chromiumoxide::Page,
        crate::ssrf_intercept::Watch,
        Option<OwnedSemaphorePermit>,
    ) {
        let page = self.page.take().expect("page already taken via close()");
        let watch = self.watch.take().expect("watched page already taken via close()");
        let permit = self._permit.take();
        (page, watch, permit)
    }
}

impl Drop for PooledPage {
    // ~keep `tokio::spawn` panics when no runtime is active on the current thread. These
    // ~keep handles cross an FFI boundary into host GC/finalizer threads, so an unguarded
    // ~keep spawn here turns a late drop into a panic that aborts the embedding process.
    fn drop(&mut self) {
        if let Some(page) = self.page.take() {
            let watch = self.watch.take();
            match tokio::runtime::Handle::try_current() {
                Ok(handle) => {
                    let close = handle.spawn(async move {
                        if let Some(watch) = watch {
                            drop(page);
                            watch.close().await;
                        } else {
                            let _ = page.close().await;
                        }
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
