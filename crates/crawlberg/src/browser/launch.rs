//! Launching a managed one-shot Chrome process (or connecting to an external
//! CDP endpoint), including `--user-data-dir` / browser-profile resolution.
//!
//! Each ephemeral launch creates a unique user data directory to avoid Chrome's
//! `SingletonLock` conflicts when multiple instances run concurrently or a
//! previous instance crashed without cleanup. When `config.browser_profile` is
//! set, the launch uses that named profile's directory instead (see
//! [`resolve_user_data_dir`]).

use chromiumoxide::Handler;
use chromiumoxide::browser::{Browser, BrowserConfig as ChromeBrowserConfig, BrowserConfigBuilder};

use crate::browser_pool::ScratchProfileDir;
use crate::error::CrawlError;
use crate::types::CrawlConfig;

/// The Chrome `--user-data-dir` a one-shot launch uses.
///
/// ~keep A scratch directory is removed when this value drops, on every
/// ~keep exit path: a failed launch, a cancelled launch or fetch (xberg-io/crawlberg#131), and a
/// ~keep teardown task that the runtime drops before it finishes, which is how every one-shot
/// ~keep fetch at the end of a `#[tokio::test]` leaked its directory (xberg-io/crawlberg#415).
#[derive(Debug)]
pub(super) enum UserDataDir {
    /// A named profile launched with `save_browser_profile: true`, used in place and kept.
    Persistent(std::path::PathBuf),
    /// An ephemeral directory, or a scratch copy of a named profile, removed on drop.
    Scratch(ScratchProfileDir),
}

impl UserDataDir {
    fn path(&self) -> &std::path::Path {
        match self {
            Self::Persistent(path) => path,
            Self::Scratch(dir) => dir.path(),
        }
    }
}

/// Resolve the `--user-data-dir` for a one-shot Chrome launch from `config`.
///
/// - No `browser_profile`: a fresh ephemeral temp directory, deleted on exit.
/// - `browser_profile` set and `save_browser_profile: true`: the named profile's
///   own directory (created if missing), used and written to in place.
/// - `browser_profile` set and `save_browser_profile: false`: the named profile's
///   directory (created if missing) is copied into a scratch temp directory so
///   the session starts from existing profile state but any changes made during
///   the session are discarded rather than written back.
fn resolve_user_data_dir(config: &CrawlConfig) -> Result<UserDataDir, CrawlError> {
    let Some(name) = config.browser_profile.as_deref() else {
        return Ok(UserDataDir::Scratch(ScratchProfileDir::create(
            "crawlberg-browser-",
            config.browser.chrome_path.as_deref(),
        )?));
    };

    let profile = crate::browser_profile::BrowserProfile::new(name)?;
    if !profile.exists() {
        profile.create()?;
    }

    if config.save_browser_profile {
        crate::browser_pool::check_snap_can_open_profile(
            &profile.user_data_dir,
            config.browser.chrome_path.as_deref(),
        )?;
        Ok(UserDataDir::Persistent(profile.user_data_dir))
    } else {
        let scratch = ScratchProfileDir::create(
            &format!("crawlberg-profile-{name}-"),
            config.browser.chrome_path.as_deref(),
        )?;
        copy_dir_recursive(&profile.user_data_dir, scratch.path())?;
        Ok(UserDataDir::Scratch(scratch))
    }
}

/// A saved profile session's hold on its profile directory. The session keeps it until its
/// Chrome has been reaped, so no other session in this process copies the directory or launches
/// on it while that Chrome can still write to it.
pub(super) type ProfileHold = tokio::sync::OwnedRwLockWriteGuard<()>;

/// What names one profile directory in [`PROFILE_LOCKS`].
#[derive(Debug, PartialEq, Eq, Hash)]
enum ProfileKey {
    /// The directory's file identity, which every name of it shares.
    File(same_file::Handle),
    /// The path as given, when the directory cannot be opened.
    Path(std::path::PathBuf),
}

/// The key of the profile directory `dir`.
///
/// ~keep A symlink, a bind mount or a case-folding file system gives one directory several
/// ~keep names, and every name must share one lock (xberg-io/crawlberg#524).
fn profile_key(dir: &std::path::Path) -> ProfileKey {
    same_file::Handle::from_path(dir).map_or_else(|_| ProfileKey::Path(dir.to_path_buf()), ProfileKey::File)
}

type ProfileLocks = std::collections::HashMap<ProfileKey, std::sync::Arc<tokio::sync::RwLock<()>>>;

/// One lock per profile directory used by a session in this process; see [`profile_lock`].
static PROFILE_LOCKS: std::sync::LazyLock<std::sync::Mutex<ProfileLocks>> = std::sync::LazyLock::new(Default::default);

/// The lock that orders every session on the profile directory `dir`: a saved session holds it
/// for writing until its Chrome is reaped, an unsaved one for reading while it copies.
///
/// ~keep Chrome removes its `SingletonLock` before its last profile writes, so a copy that starts
/// ~keep once the previous session has only been asked to close fails on a file Chrome renames
/// ~keep meanwhile (xberg-io/crawlberg#524). Copies read the directory together; only a Chrome
/// ~keep writing it needs the directory alone. An entry nothing holds is dropped at the next lookup.
fn profile_lock(dir: &std::path::Path) -> std::sync::Arc<tokio::sync::RwLock<()>> {
    let mut locks = PROFILE_LOCKS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    locks.retain(|_, lock| std::sync::Arc::strong_count(lock) > 1);
    std::sync::Arc::clone(locks.entry(profile_key(dir)).or_default())
}

/// Wait until this session may use `config.browser_profile`, then resolve its `--user-data-dir`.
///
/// A saved profile comes back with the session's hold on it. An unsaved profile is copied under a
/// read hold that ends with the copy, since its Chrome writes only to the copy.
async fn claim_user_data_dir(config: &CrawlConfig) -> Result<(UserDataDir, Option<ProfileHold>), CrawlError> {
    let Some(name) = config.browser_profile.as_deref() else {
        return Ok((resolve_user_data_dir(config)?, None));
    };
    let profile = crate::browser_profile::BrowserProfile::new(name)?;
    if !profile.exists() {
        profile.create()?;
    }
    // ~keep The create comes first: the lock is keyed on the directory's file identity.
    let lock = profile_lock(&profile.user_data_dir);
    if config.save_browser_profile {
        let hold = lock.write_owned().await;
        Ok((resolve_user_data_dir(config)?, Some(hold)))
    } else {
        let _copying = lock.read_owned().await;
        Ok((resolve_user_data_dir(config)?, None))
    }
}

/// Recursively copy `src` into `dst`, creating `dst` if needed. Symlinks inside
/// `src` are skipped rather than followed or copied as links.
fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> Result<(), CrawlError> {
    std::fs::create_dir_all(dst)
        .map_err(|e| CrawlError::other(format!("failed to create profile scratch directory: {e}")))?;
    let entries =
        std::fs::read_dir(src).map_err(|e| CrawlError::other(format!("failed to read profile directory: {e}")))?;
    for entry in entries {
        let entry = entry.map_err(|e| CrawlError::other(format!("failed to read profile entry: {e}")))?;
        let file_type = entry
            .file_type()
            .map_err(|e| CrawlError::other(format!("failed to stat profile entry: {e}")))?;
        let dest_path = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &dest_path)?;
        } else if file_type.is_file() {
            std::fs::copy(entry.path(), &dest_path).map_err(|e| {
                CrawlError::other(format!("failed to copy profile file {}: {e}", entry.path().display()))
            })?;
        }
    }
    Ok(())
}

/// A browser [`launch_or_connect`] launched or connected to, with what the session keeps
/// until it is gone.
pub(super) type Launched = (
    Browser,
    Handler,
    Option<UserDataDir>,
    Option<crate::net::egress::Egress>,
    Option<ProfileHold>,
);

/// Launch a new managed browser or connect to an external CDP endpoint.
///
/// A launch with `browser_profile` opens its page in the browser's own context, which has no
/// proxy of its own, so under `deny_private` Chrome is launched through the SSRF proxy, handed
/// back for the session to keep until the browser is gone. A launch on a saved profile also
/// returns the session's hold on that profile, which the session keeps until its Chrome is reaped.
pub(super) async fn launch_or_connect(config: &CrawlConfig) -> Result<Launched, CrawlError> {
    if let Some(ref endpoint) = config.browser.endpoint {
        crate::types::warn_ignored_launch_options(
            &config.browser,
            "connecting to an external browser.endpoint, whose Chrome process is launched externally",
        );
        if config.browser_profile.is_some() {
            tracing::warn!(
                profile = config.browser_profile.as_deref().unwrap_or_default(),
                "browser_profile is ignored when connecting to an external browser.endpoint; \
                 the remote Chrome process's profile is managed externally"
            );
        }
        let (browser, handler) = crate::browser_pool::connect_endpoint(endpoint).await?;
        Ok((browser, handler, None, None, None))
    } else {
        let mut proxy = crate::proxy::chrome_proxy_for(config)?;
        let egress = match config.browser_profile {
            Some(_) => crate::net::egress::Egress::start(&config.ssrf, proxy.as_ref()).await?,
            None => None,
        };
        if let Some(ref egress) = egress {
            proxy = Some(egress.chrome_proxy());
        }
        // ~keep Inside the caller's launch deadline, so a wait for the profile ends with it.
        let (user_data, hold) = claim_user_data_dir(config).await?;
        if config.ssrf.deny_private {
            crate::browser_pool::disable_non_proxied_udp(user_data.path())?;
        }

        let browser_config = build_one_shot_launch_builder(user_data.path(), &config.browser, proxy.as_ref())?
            .build()
            .map_err(|e| CrawlError::browser_error(format!("invalid browser config: {e}")))?;

        // ~keep A failed or cancelled launch drops `user_data`, which removes a scratch directory.
        let launched = match user_data {
            UserDataDir::Scratch(dir) => dir
                .launch(browser_config)
                .await
                .map(|(browser, handler, dir)| (browser, handler, UserDataDir::Scratch(dir))),
            UserDataDir::Persistent(path) => Browser::launch(browser_config)
                .await
                .map(|(browser, handler)| (browser, handler, UserDataDir::Persistent(path))),
        };
        let (mut browser, handler, user_data) =
            launched.map_err(|e| CrawlError::browser_error(format!("failed to launch browser: {e}")))?;
        if config.ssrf.deny_private {
            crate::browser_pool::confirm_profile_in_use(&mut browser, user_data.path()).await?;
        }
        Ok((browser, handler, Some(user_data), egress, hold))
    }
}

/// Build the [`ChromeBrowserConfig`] builder for a fresh one-shot launch (not the
/// `browser.endpoint` connect branch).
///
/// ~keep Split out from `launch_or_connect` so a test can assert on the flags this
/// ~keep path actually passes without spawning a real Chrome process.
fn build_one_shot_launch_builder(
    user_data_dir: &std::path::Path,
    browser: &crate::types::BrowserConfig,
    proxy: Option<&crate::proxy::ChromeProxy>,
) -> Result<BrowserConfigBuilder, CrawlError> {
    let mut builder = ChromeBrowserConfig::builder()
        .no_sandbox()
        .new_headless_mode()
        .user_data_dir(user_data_dir)
        .disable_default_args();
    // ~keep Mirror browser_pool's fork-safety env vars so one-shot and pooled Chrome launch paths match.
    builder = builder
        .env("OBJC_DISABLE_INITIALIZE_FORK_SAFETY", "YES")
        .env("OS_ACTIVITY_MODE", "disable");
    builder = crate::browser_pool::apply_default_args(builder, &browser.chrome_args);
    crate::browser_pool::apply_launch_overrides(
        builder,
        "browser",
        browser.chrome_path.as_deref(),
        &browser.chrome_args,
        proxy,
    )
}

/// Returns a modern Chrome user-agent string suitable for the runtime environment.
/// Used as the default UA when stealth mode is enabled.
pub(super) fn resolve_default_user_agent() -> &'static str {
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36"
}

#[cfg(test)]
mod user_data_dir_tests {
    //! Hermetic, Chrome-free unit tests for the `browser_profile` /
    //! `save_browser_profile` wiring in [`resolve_user_data_dir`] and
    //! [`copy_dir_recursive`]. End-to-end proof that Chrome actually launches
    //! against these resolved directories lives in
    //! `crates/crawlberg/tests/test_browser_profile.rs`.
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::browser_profile::BrowserProfile;
    use crate::types::CrawlConfig;

    static NAME_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A collision-free profile name so parallel test runs never race on the
    /// same on-disk profile directory.
    fn unique_profile_name(tag: &str) -> String {
        format!(
            "crawlberg-unit-test-{tag}-{}-{}",
            std::process::id(),
            NAME_COUNTER.fetch_add(1, Ordering::Relaxed)
        )
    }

    /// Deletes the backing profile directory on drop, regardless of outcome.
    struct ProfileGuard(BrowserProfile);
    impl Drop for ProfileGuard {
        fn drop(&mut self) {
            let _ = self.0.delete();
        }
    }

    #[test]
    fn no_profile_resolves_to_ephemeral_temp_dir_marked_for_cleanup() {
        let config = CrawlConfig::default();
        let resolved = resolve_user_data_dir(&config).expect("resolve must succeed without a profile configured");
        assert!(
            matches!(resolved, UserDataDir::Scratch(_)),
            "ephemeral (no browser_profile) launches must be cleaned up after the session"
        );
    }

    #[test]
    fn missing_named_profile_is_created_and_used_directly_when_saved() {
        let name = unique_profile_name("create-save");
        let profile = BrowserProfile::new(&name).expect("profile name must be valid");
        assert!(!profile.exists(), "precondition: profile must not exist yet");
        let _guard = ProfileGuard(profile.clone());

        // ~keep A Chrome that is not a snap: a snap found on the host cannot open the profile store.
        let chrome = crate::types::executable_temp_file("create-save");
        let mut config = CrawlConfig {
            browser_profile: Some(name.clone()),
            save_browser_profile: true,
            ..CrawlConfig::default()
        };
        config.browser.chrome_path = Some(chrome.clone());
        let resolved = resolve_user_data_dir(&config);
        let _ = std::fs::remove_file(&chrome);
        let resolved = resolved.expect("resolve must succeed");

        assert!(
            profile.exists(),
            "resolve_user_data_dir must create the named profile directory when missing"
        );
        assert_eq!(
            resolved.path(),
            profile.user_data_dir,
            "save_browser_profile: true must launch directly against the profile's own directory"
        );
        assert!(
            matches!(resolved, UserDataDir::Persistent(_)),
            "save_browser_profile: true must not mark the profile directory for cleanup"
        );
    }

    #[test]
    fn unsaved_profile_launches_from_a_scratch_copy_that_preserves_the_original() {
        let name = unique_profile_name("no-save");
        let profile = BrowserProfile::new(&name).expect("profile name must be valid");
        profile.create().expect("profile directory must be creatable");
        let _guard = ProfileGuard(profile.clone());
        std::fs::write(profile.user_data_dir.join("marker.txt"), b"original").expect("marker file must be writable");

        let config = CrawlConfig {
            browser_profile: Some(name.clone()),
            save_browser_profile: false,
            ..CrawlConfig::default()
        };
        let resolved = resolve_user_data_dir(&config).expect("resolve must succeed");

        assert_ne!(
            resolved.path(),
            profile.user_data_dir,
            "save_browser_profile: false must launch from a scratch copy, never the profile dir itself"
        );
        assert!(
            matches!(resolved, UserDataDir::Scratch(_)),
            "the scratch copy must be marked for cleanup after the session"
        );
        assert_eq!(
            std::fs::read(resolved.path().join("marker.txt")).expect("scratch copy must contain the marker file"),
            b"original",
            "the scratch copy must start from the existing profile state"
        );

        std::fs::write(resolved.path().join("marker.txt"), b"mutated-in-session")
            .expect("writing into the scratch copy must succeed");
        assert_eq!(
            std::fs::read(profile.user_data_dir.join("marker.txt")).expect("original marker file must still exist"),
            b"original",
            "writes into the scratch copy must never be reflected back into the saved profile"
        );

        let scratch = resolved.path().to_path_buf();
        drop(resolved);
        assert!(
            crate::browser_pool::tests::wait_for_removal(&scratch),
            "the scratch copy must be removed when it is dropped"
        );
    }

    /// The error of a profile copy that fails names the file it could not copy.
    #[cfg(unix)]
    #[test]
    fn a_failed_profile_copy_names_the_file() {
        use std::os::unix::fs::PermissionsExt;

        let name = unique_profile_name("copy-error");
        let profile = BrowserProfile::new(&name).expect("profile name must be valid");
        profile.create().expect("profile directory must be creatable");
        let _guard = ProfileGuard(profile.clone());
        let unreadable = profile.user_data_dir.join("unreadable-marker");
        std::fs::write(&unreadable, b"x").expect("marker file must be writable");
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000))
            .expect("permissions must be settable");
        if std::fs::read(&unreadable).is_ok() {
            // ~keep Root reads a mode 000 file, so the copy cannot fail here.
            return;
        }

        let config = CrawlConfig {
            browser_profile: Some(name),
            save_browser_profile: false,
            ..CrawlConfig::default()
        };
        let error = resolve_user_data_dir(&config)
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
        let _ = std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o600));

        assert!(
            error.contains("unreadable-marker") && error.contains("failed to copy profile file"),
            "the copy error must name the file: {error:?}"
        );
    }

    #[test]
    fn copy_dir_recursive_copies_nested_files_and_skips_symlinks() {
        let root = std::env::temp_dir().join(unique_profile_name("copy"));
        let src = root.join("src");
        let dst = root.join("dst");
        std::fs::create_dir_all(src.join("nested")).expect("nested src dir must be creatable");
        std::fs::write(src.join("top.txt"), b"top").expect("top-level file must be writable");
        std::fs::write(src.join("nested").join("deep.txt"), b"deep").expect("nested file must be writable");

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let _ = symlink(src.join("top.txt"), src.join("link.txt"));
        }

        copy_dir_recursive(&src, &dst).expect("recursive copy must succeed");

        assert_eq!(std::fs::read(dst.join("top.txt")).unwrap(), b"top");
        assert_eq!(std::fs::read(dst.join("nested").join("deep.txt")).unwrap(), b"deep");
        assert!(
            !dst.join("link.txt").exists(),
            "symlinks in the source directory must not be copied"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn one_profile_directory_has_one_lock_until_nothing_holds_it() {
        let held_dir = std::env::temp_dir().join(unique_profile_name("lock-held"));
        let other_dir = std::env::temp_dir().join(unique_profile_name("lock-other"));
        let is_listed = |dir: &std::path::Path| {
            PROFILE_LOCKS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains_key(&profile_key(dir))
        };

        let held = profile_lock(&held_dir);
        assert!(
            std::sync::Arc::ptr_eq(&held, &profile_lock(&held_dir)),
            "every session on one profile directory must share one lock"
        );
        drop(profile_lock(&other_dir));
        assert!(is_listed(&held_dir), "a lock a session holds must stay listed");

        drop(held);
        drop(profile_lock(&other_dir));
        assert!(
            !is_listed(&held_dir),
            "a lock nothing holds must be dropped at the next lookup"
        );
    }

    /// Every name of one directory gets its lock, and another directory gets its own.
    #[cfg(unix)]
    #[test]
    fn a_directory_and_a_symlink_to_it_share_one_lock() {
        let real = std::env::temp_dir().join(unique_profile_name("lock-real"));
        let alias = std::env::temp_dir().join(unique_profile_name("lock-alias"));
        let other = std::env::temp_dir().join(unique_profile_name("lock-other-dir"));
        std::fs::create_dir_all(&real).expect("the directory must be creatable");
        std::fs::create_dir_all(&other).expect("the directory must be creatable");
        std::os::unix::fs::symlink(&real, &alias).expect("the symlink must be creatable");

        let held = profile_lock(&real);
        let shares_lock = std::sync::Arc::ptr_eq(&held, &profile_lock(&alias));
        let other_shares_lock = std::sync::Arc::ptr_eq(&held, &profile_lock(&other));

        let _ = std::fs::remove_file(&alias);
        let _ = std::fs::remove_dir_all(&real);
        let _ = std::fs::remove_dir_all(&other);
        assert!(shares_lock, "a directory and a symlink to it must share one lock");
        assert!(!other_shares_lock, "another directory must get its own lock");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_one_shot_launch_builder_carries_no_double_dashed_flag_and_the_macos_keychain_flag() {
        // ~keep Behavioral, not textual: this calls the exact function `launch_or_connect`
        // ~keep uses to build its `BrowserConfig`, so a path that stops calling
        // ~keep `apply_default_args` (even behind a comment claiming it still does) fails
        // ~keep here because the returned flags actually change.
        let builder = build_one_shot_launch_builder(
            std::path::Path::new("/tmp/browser-rs-test-profile"),
            &crate::types::BrowserConfig::default(),
            None,
        )
        .expect("the default browser config names no binary to check");
        crate::browser_pool::assert_launch_flags_are_normalized(&builder);
    }

    #[test]
    fn the_one_shot_launch_routes_chrome_through_the_configured_proxy() {
        let config = CrawlConfig {
            browser: crate::types::BrowserConfig {
                proxy: Some(crate::types::ProxyConfig {
                    url: "127.0.0.1:3128".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        let proxy = crate::proxy::chrome_proxy_for(&config).expect("a usable proxy");
        let debug = format!(
            "{:?}",
            build_one_shot_launch_builder(
                std::path::Path::new("/tmp/browser-rs-test-profile"),
                &config.browser,
                proxy.as_ref()
            )
            .expect("the config names no binary to check")
        );
        assert!(
            debug.contains("key: \"proxy-server=http://127.0.0.1:3128\""),
            "the render's Chrome must be launched through the proxy: {debug}"
        );
        assert!(
            debug.contains("proxy-bypass-list=<-loopback>"),
            "loopback requests must not bypass the proxy: {debug}"
        );
    }

    #[test]
    fn the_configured_proxy_replaces_a_caller_proxy_flag() {
        let proxy = crate::proxy::chrome_proxy(&crate::types::ProxyConfig {
            url: "http://127.0.0.1:9".into(),
            ..Default::default()
        })
        .expect("an http proxy is a Chrome proxy");
        for (caller_flag, caller_value) in [
            ("--proxy-server=http://127.0.0.1:7", "127.0.0.1:7"),
            ("--proxy-bypass-list=*.internal", "*.internal"),
            ("--proxy-pac-url=http://127.0.0.1:7/p.pac", "127.0.0.1:7/p.pac"),
            ("--no-proxy-server", "no-proxy-server"),
            ("--proxy-auto-detect", "proxy-auto-detect"),
        ] {
            let browser = crate::types::BrowserConfig {
                chrome_args: vec![caller_flag.to_owned()],
                ..Default::default()
            };
            let (built, fields) = crate::tracing_capture::capture_events(|| {
                build_one_shot_launch_builder(
                    std::path::Path::new("/tmp/browser-rs-test-profile"),
                    &browser,
                    Some(&proxy),
                )
            });
            let debug = format!("{:?}", built.expect("the config names no binary to check"));
            for configured in ["proxy-server=http://127.0.0.1:9", "proxy-bypass-list=<-loopback>"] {
                assert!(
                    debug.contains(&format!("key: \"{configured}\"")),
                    "{caller_flag}: the configured proxy's {configured} is missing: {debug}"
                );
            }
            assert!(
                !debug.contains(caller_value),
                "{caller_flag}: the caller's flag must be dropped: {debug}"
            );
            let switch = caller_flag.split('=').next().expect("a switch name");
            // ~keep A switch with no value has no secret to hide; its name is what the warning prints.
            if caller_flag.contains('=') {
                crate::tracing_capture::assert_logged_without_secret(&fields, caller_value, switch);
            }
        }
    }

    #[test]
    fn a_caller_proxy_flag_reaches_chrome_when_no_proxy_is_configured() {
        let browser = crate::types::BrowserConfig {
            chrome_args: vec![
                "--proxy-server=http://127.0.0.1:7".to_owned(),
                "--proxy-bypass-list=*.internal".to_owned(),
            ],
            ..Default::default()
        };
        let debug = format!(
            "{:?}",
            build_one_shot_launch_builder(std::path::Path::new("/tmp/browser-rs-test-profile"), &browser, None)
                .expect("the config names no binary to check")
        );
        for flag in ["proxy-server=http://127.0.0.1:7", "proxy-bypass-list=*.internal"] {
            assert!(
                debug.contains(&format!("key: \"{flag}\"")),
                "without a configured proxy the caller's {flag} must reach Chrome: {debug}"
            );
        }
    }

    #[test]
    fn the_one_shot_launch_builder_uses_the_configured_chrome_path_and_args() {
        crate::browser_pool::assert_launch_overrides_reach_the_builder(|chrome_path, chrome_args| {
            build_one_shot_launch_builder(
                std::path::Path::new("/tmp/browser-rs-test-profile"),
                &crate::types::BrowserConfig {
                    chrome_path,
                    chrome_args,
                    ..Default::default()
                },
                None,
            )
        });
    }

    /// A scratch profile directory that never reaches a launched Chrome is removed when it drops.
    ///
    /// ~keep This is the failed-launch and cancelled-launch case stated as a unit: `launch_or_connect`
    /// ~keep drops `user_data` without returning it, exactly as here. Proven at this level rather than
    /// ~keep through a real Chrome because an integration test cannot reliably choose which window a
    /// ~keep cancellation lands in -- see xberg-io/crawlberg#198.
    #[test]
    fn an_unclaimed_scratch_profile_directory_is_removed_when_it_drops() {
        let resolved = resolve_user_data_dir(&CrawlConfig::default()).expect("resolve must succeed");
        let path = resolved.path().to_path_buf();
        assert!(path.is_dir(), "the scratch directory must exist before it drops");

        drop(resolved);

        assert!(
            crate::browser_pool::tests::wait_for_removal(&path),
            "an unclaimed scratch profile directory must be removed"
        );
    }

    /// A one-shot launch records the Chrome it starts, so dropping its profile directory stops
    /// that Chrome and removes the directory.
    #[tokio::test(flavor = "multi_thread")]
    #[allow(clippy::print_stderr, reason = "test-only skip announcement")]
    async fn dropping_a_one_shot_launchs_profile_directory_stops_its_chrome() {
        let (browser, handler, user_data, _egress, _hold) = match launch_or_connect(&CrawlConfig::default()).await {
            Ok(launched) => launched,
            Err(error) => {
                eprintln!("skipping: no usable Chrome: {error}");
                return;
            }
        };
        let user_data = user_data.expect("a launched Chrome must have a profile directory");
        let path = user_data.path().to_path_buf();

        crate::browser_pool::tests::assert_dropping_the_profile_stops_its_chrome(browser, handler, user_data, path)
            .await;
    }

    /// A refused `chrome_path` removes the scratch directory `resolve_user_data_dir` created for
    /// the launch it never made.
    ///
    /// ~keep Pins the call site in `launch_or_connect`: `resolve_user_data_dir(config)?` then
    /// ~keep `build_one_shot_launch_builder(..)?`, whose `?` drops the guard on a refusal. No
    /// ~keep Chrome is needed: the check on the path fails before any process would be spawned.
    #[tokio::test]
    async fn a_one_shot_launch_refused_by_a_missing_chrome_path_leaves_no_profile_directory() {
        let config = CrawlConfig {
            browser: crate::types::BrowserConfig {
                chrome_path: Some(std::path::PathBuf::from("/nonexistent/crawlberg-one-shot-chrome")),
                ..Default::default()
            },
            ..Default::default()
        };
        let error =
            crate::browser_pool::tests::assert_refused_launch_leaves_no_scratch_dir(|| launch_or_connect(&config))
                .await;
        assert!(
            error.contains("cannot be used"),
            "the error must name the path, got: {error}"
        );
    }

    /// The same call site refused by a `chrome_args` entry instead of `chrome_path`.
    #[tokio::test]
    async fn a_one_shot_launch_refused_by_a_user_data_dir_flag_leaves_no_profile_directory() {
        let config = CrawlConfig {
            browser: crate::types::BrowserConfig {
                chrome_args: vec!["--user-data-dir=/tmp/crawlberg-one-shot-elsewhere".to_owned()],
                ..Default::default()
            },
            ..Default::default()
        };
        let error =
            crate::browser_pool::tests::assert_refused_launch_leaves_no_scratch_dir(|| launch_or_connect(&config))
                .await;
        assert!(
            error.contains("must not set --user-data-dir"),
            "the error must name the refused flag, got: {error}"
        );
    }

    /// A one-shot launch cut off by the overall deadline hands its profile teardown off the
    /// executor thread.
    ///
    /// ~keep No Chrome is needed: without one the launch fails before the deadline, and the
    /// ~keep profile directory drops on the same path.
    #[tokio::test]
    async fn a_cancelled_one_shot_launch_tears_its_profile_down_off_the_executor_thread() {
        let mut config = CrawlConfig::default();
        config.browser.overall_timeout = std::time::Duration::from_millis(1);
        let before = crate::browser_pool::tests::profile_drops_here();

        let fetched = super::super::one_shot_fetch("about:blank", &config, None, false).await;

        assert!(fetched.is_err(), "a Chrome launch cannot finish within a millisecond");
        crate::browser_pool::tests::assert_profile_teardown_left_this_thread(before);
    }

    /// `launch_or_connect`'s connect-error message must never carry a `browser.endpoint`
    /// password or path token, though the failing origin must still be readable for debugging.
    ///
    /// ~keep A closed local port refuses the connection immediately, so this needs no real
    /// ~keep Chrome and stays fast. `ws://` skips chromiumoxide's `json/version` HTTP probe
    /// ~keep and goes straight to the WebSocket handshake. The endpoint-listener test just
    /// ~keep below reaches the same error path with a local socket that answers HTTP 418, so
    /// ~keep a closed port is not the only way here; it stays because it needs no listener at all.
    #[tokio::test]
    async fn connect_error_prints_only_the_endpoint_origin() {
        let config = CrawlConfig {
            browser: crate::types::BrowserConfig {
                endpoint: Some("ws://user:hunter2@127.0.0.1:1/devtools/browser/b1946ac9-guid".into()),
                ..Default::default()
            },
            ..Default::default()
        };

        let err = launch_or_connect(&config)
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

    /// Every spelling of `browser.endpoint` that the config check accepts must reach the browser.
    #[tokio::test]
    async fn connects_every_endpoint_spelling_the_checks_accept() {
        crate::browser_pool::tests::assert_every_accepted_endpoint_reaches_the_browser(|endpoint| async move {
            let config = CrawlConfig {
                browser: crate::types::BrowserConfig {
                    endpoint: Some(endpoint),
                    ..Default::default()
                },
                ..Default::default()
            };
            launch_or_connect(&config).await
        })
        .await;
    }

    /// A saved named profile is never removed when its value drops.
    #[test]
    fn a_persistent_profile_directory_is_never_removed() {
        let dir = tempfile::tempdir().expect("the directory must be creatable");

        drop(UserDataDir::Persistent(dir.path().to_path_buf()));

        assert!(
            dir.path().is_dir(),
            "a persistent profile directory must survive its value"
        );
    }

    /// Dropping a saved named profile neither removes it nor stops a Chrome still using it.
    #[cfg(unix)]
    #[test]
    fn a_persistent_profile_in_use_is_neither_removed_nor_its_user_killed() {
        let dir = tempfile::tempdir().expect("the directory must be creatable");
        let flag = crate::browser_pool::user_data_dir_flag(dir.path());
        let mut user = crate::browser_pool::tests::spawn_bystander(&flag);

        drop(UserDataDir::Persistent(dir.path().to_path_buf()));

        let running = user.try_wait().expect("the status must be readable").is_none();
        let _ = user.kill();
        let _ = user.wait();
        assert!(
            running,
            "a Chrome using a saved profile must not be killed when its value drops"
        );
        assert!(dir.path().is_dir(), "a saved profile directory must survive its value");
    }

    /// A one-shot session whose teardown task runs to the end removes its profile directory.
    #[tokio::test(flavor = "multi_thread")]
    #[allow(clippy::print_stderr, reason = "test-only skip announcement")]
    async fn a_one_shot_session_torn_down_by_its_task_leaves_no_profile_directory() {
        use tokio_stream::StreamExt;

        let (browser, mut handler, data_dir, egress, profile_hold) =
            match launch_or_connect(&CrawlConfig::default()).await {
                Ok(launched) => launched,
                Err(error) => {
                    eprintln!("skipping: no usable Chrome: {error}");
                    return;
                }
            };
        let path = data_dir
            .as_ref()
            .map(|dir| dir.path().to_path_buf())
            .expect("a launched Chrome must have a profile directory");
        let handler_handle = tokio::spawn(async move { while handler.next().await.is_some() {} });
        drop(super::super::OneShotSession {
            browser: Some(std::sync::Arc::new(browser)),
            firewall: None,
            open_tab: None,
            handler_handle: Some(handler_handle),
            data_dir,
            egress,
            profile_hold,
            shutdown_timeout: std::time::Duration::from_secs(5),
            origin: crate::ssrf_intercept::BrowserOrigin::Killed,
        });

        tokio::task::spawn_blocking(move || {
            crate::browser_pool::tests::assert_profile_directory_is_gone_for_good(&path)
        })
        .await
        .expect("the teardown task must stop Chrome and remove the profile directory");
    }

    /// A one-shot session dropped just before its runtime stops still removes its profile directory.
    ///
    /// ~keep The session's `Drop` spawns its teardown, and a runtime that stops right after, as every
    /// ~keep `#[tokio::test]` ending on a one-shot fetch does, drops that task unfinished. That left one
    /// ~keep `crawlberg-browser-*` directory per fetch in the temp directory (xberg-io/crawlberg#415).
    #[test]
    #[allow(clippy::print_stderr, reason = "test-only skip announcement")]
    fn a_one_shot_session_dropped_as_its_runtime_stops_leaves_no_profile_directory() {
        use tokio_stream::StreamExt;

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("a runtime must build");
        let path = runtime.block_on(async {
            let (browser, mut handler, data_dir, egress, profile_hold) =
                match launch_or_connect(&CrawlConfig::default()).await {
                    Ok(launched) => launched,
                    Err(error) => {
                        eprintln!("skipping: no usable Chrome: {error}");
                        return None;
                    }
                };
            let path = data_dir
                .as_ref()
                .map(|dir| dir.path().to_path_buf())
                .expect("a launched Chrome must have a profile directory");
            let handler_handle = tokio::spawn(async move { while handler.next().await.is_some() {} });
            drop(super::super::OneShotSession {
                browser: Some(std::sync::Arc::new(browser)),
                firewall: None,
                open_tab: None,
                handler_handle: Some(handler_handle),
                data_dir,
                egress,
                profile_hold,
                shutdown_timeout: std::time::Duration::from_secs(5),
                origin: crate::ssrf_intercept::BrowserOrigin::Killed,
            });
            Some(path)
        });
        drop(runtime);
        let Some(path) = path else {
            return;
        };
        crate::browser_pool::tests::assert_profile_directory_is_gone_for_good(&path);
    }
}
