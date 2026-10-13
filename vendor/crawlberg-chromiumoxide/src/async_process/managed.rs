use std::io;
use std::process::{Child, ExitStatus};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::{ChildStderr, ProcessTree};

const WAIT_INTERVAL: Duration = Duration::from_millis(10);
const DROP_TIMEOUT: Duration = Duration::from_secs(2);

/// A Unix child with native ownership of its wait status, independent of Tokio's signal registry.
#[derive(Debug)]
pub struct ManagedChild {
    pub stderr: Option<ChildStderr>,
    inner: Option<Child>,
    status: Option<ExitStatus>,
    tree: ProcessTree,
    owner: u32,
    killed_on_drop: bool,
    debugging_pipe: Option<std::os::unix::net::UnixStream>,
    reaper: NativeReaper,
}

impl ManagedChild {
    pub(super) fn new(
        mut child: Child,
        tree: ProcessTree,
        killed_on_drop: bool,
        debugging_pipe: Option<std::os::unix::net::UnixStream>,
        reaper: NativeReaper,
    ) -> io::Result<Self> {
        let stderr = child.stderr.take();
        let mut managed = Self {
            stderr: None,
            inner: Some(child),
            status: None,
            tree,
            owner: std::process::id(),
            killed_on_drop,
            debugging_pipe,
            reaper,
        };
        if let Some(stderr) = stderr {
            managed.stderr = Some(ChildStderr {
                inner: tokio::process::ChildStderr::from_std(stderr)?,
            });
        }
        Ok(managed)
    }

    pub fn id(&self) -> Option<u32> {
        if self.status.is_some() {
            None
        } else {
            self.inner.as_ref().map(Child::id)
        }
    }

    pub fn spawned_here(&self) -> bool {
        self.owner == std::process::id()
    }

    pub fn killed_on_drop(&self) -> bool {
        self.killed_on_drop
    }

    pub fn tree(&self) -> Option<&ProcessTree> {
        Some(&self.tree)
    }

    pub(crate) fn take_debugging_pipe(&mut self) -> Option<std::os::unix::net::UnixStream> {
        self.debugging_pipe.take()
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if !self.spawned_here() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "child belongs to another process",
            ));
        }
        if self.status.is_none() {
            self.status = self
                .inner
                .as_mut()
                .ok_or_else(|| io::Error::other("child handle was taken"))?
                .try_wait()?;
        }
        Ok(self.status)
    }

    pub async fn wait(&mut self) -> io::Result<ExitStatus> {
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(status);
            }
            tokio::time::sleep(WAIT_INTERVAL).await;
        }
    }

    pub fn start_kill(&mut self) -> io::Result<()> {
        if !self.spawned_here() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "child belongs to another process",
            ));
        }
        if self.id().is_none() {
            return Err(io::Error::other("the browser leader was already reaped"));
        }
        self.inner
            .as_mut()
            .ok_or_else(|| io::Error::other("child handle was taken"))?
            .kill()
    }

    pub fn start_kill_group(&mut self) -> io::Result<()> {
        if !self.spawned_here() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "browser group belongs to another process",
            ));
        }
        if self.id().is_none() {
            return Err(io::Error::other("the browser leader was already reaped"));
        }
        self.tree.0.signal()
    }

    pub async fn kill(&mut self) -> io::Result<()> {
        if self.try_wait()?.is_none() {
            self.start_kill()?;
        }
        self.wait().await.map(|_| ())
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        if !self.spawned_here() {
            // ~keep A fork inherits the sender without its waiter; never close or signal the parent's reservation.
            std::mem::forget(self.reaper.release.take());
            return;
        }
        if self.killed_on_drop && !self.tree.stop(DROP_TIMEOUT) {
            tracing::warn!("managed browser group did not stop within the cleanup deadline");
        }
        if let Some(mut child) = self.inner.take() {
            match child.try_wait() {
                Ok(Some(_)) => {}
                Ok(None) => self.reaper.handoff(child),
                Err(error) => tracing::warn!(%error, "managed child exit status could not be collected"),
            }
        }
    }
}

#[derive(Debug)]
pub(super) struct NativeReaper {
    child: Arc<Mutex<Option<Child>>>,
    release: Option<std::sync::mpsc::Sender<()>>,
}

impl NativeReaper {
    pub(super) fn reserve() -> io::Result<Self> {
        let child = Arc::new(Mutex::new(None::<Child>));
        let worker = Arc::clone(&child);
        let (release, ready) = std::sync::mpsc::channel();
        // ~keep Reserve before any child exists, so resource exhaustion cannot strand ownership during Drop.
        std::thread::Builder::new()
            .name("browser-child-wait".into())
            .spawn(move || {
                // ~keep Channel closure hands off ownership without allocating or touching Tokio's orphan queue.
                match ready.recv() {
                    Ok(()) | Err(_) => {}
                }
                let child = worker.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take();
                if let Some(mut child) = child
                    && let Err(error) = child.wait()
                {
                    tracing::warn!(%error, "managed browser child wait failed");
                }
            })?;
        Ok(Self {
            child,
            release: Some(release),
        })
    }

    fn handoff(&mut self, child: Child) {
        *self.child.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(child);
        drop(self.release.take());
    }
}

#[allow(unsafe_code)]
pub(super) fn normalize_reaping() -> io::Result<()> {
    // SAFETY: a null new action queries the live signal disposition into valid storage. ~keep
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    if unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), &mut action) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let ignored = action.sa_sigaction == libc::SIG_IGN;
    if !ignored && action.sa_flags & libc::SA_NOCLDWAIT == 0 {
        return Ok(());
    }
    if ignored {
        action.sa_sigaction = libc::SIG_DFL;
    }
    action.sa_flags &= !libc::SA_NOCLDWAIT;
    // SAFETY: install the complete action once, preserving the inherited mask and all remaining flags. ~keep
    if unsafe { libc::sigaction(libc::SIGCHLD, &action, std::ptr::null_mut()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
mod resource_tests {
    use super::*;
    use crate::async_process::{Command, Stdio};
    use std::time::Instant;

    const RESOURCE_CASE: &str = "CRAWLBERG_MANAGED_RESOURCE_CASE";

    // ~keep Process-wide counters require a fresh process even when unrelated vendor tests run concurrently.
    fn runs_in_isolated_process(test: &str) -> bool {
        if std::env::var(RESOURCE_CASE).as_deref() == Ok(test) {
            return true;
        }
        let mut child = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                &format!("async_process::managed::resource_tests::{test}"),
                "--nocapture",
            ])
            .env(RESOURCE_CASE, test)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("isolated resource test");
        let deadline = Instant::now() + Duration::from_secs(60);
        while child.try_wait().expect("isolated child status").is_none() {
            if Instant::now() >= deadline {
                child.kill().expect("terminate this test child");
                child.wait().expect("reap this test child");
                panic!("isolated resource test exceeded its deadline");
            }
            std::thread::sleep(WAIT_INTERVAL);
        }
        let output = child.wait_with_output().expect("isolated test output");
        assert!(
            output.status.success(),
            "{test}: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"));
        false
    }

    fn resources() -> (usize, usize) {
        (
            std::fs::read_dir("/proc/self/task").expect("thread list").count(),
            std::fs::read_dir("/proc/self/fd").expect("descriptor list").count(),
        )
    }

    fn resources_return_to(expected: (usize, usize)) {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let actual = resources();
            if actual == expected {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "managed resources did not return: {actual:?}, expected {expected:?}"
            );
            std::thread::sleep(WAIT_INTERVAL);
        }
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
    }

    #[test]
    fn successful_wait_and_live_drop_release_the_reserved_waiter_and_descriptors() {
        if !runs_in_isolated_process("successful_wait_and_live_drop_release_the_reserved_waiter_and_descriptors") {
            return;
        }
        let runtime = runtime();
        let _entered = runtime.enter();
        let baseline = resources();
        for waited in [true, false] {
            let mut command = Command::new("/bin/sleep");
            command.arg("30").stderr(Stdio::piped());
            let mut child = command.spawn_managed().expect("managed child");
            if waited {
                child.tree().expect("tree").stop(DROP_TIMEOUT);
                runtime
                    .block_on(tokio::time::timeout(DROP_TIMEOUT, child.wait()))
                    .expect("bounded successful wait")
                    .expect("wait");
            }
            drop(child);
            resources_return_to(baseline);
        }
    }

    #[test]
    fn caller_owned_drop_reaps_natural_exit_without_ending_the_held_tree() {
        if !runs_in_isolated_process("caller_owned_drop_reaps_natural_exit_without_ending_the_held_tree") {
            return;
        }
        let runtime = runtime();
        let _entered = runtime.enter();
        let baseline = resources();
        let mut command = Command::new("/bin/sleep");
        command.arg("0.1").kill_on_drop(false).stderr(Stdio::piped());
        let child = command.spawn_managed().expect("managed child");
        let tree = child.tree().expect("tree").clone();
        let with_child = resources();
        drop(child);
        let deadline = Instant::now() + Duration::from_secs(3);
        while resources().0 != baseline.0 {
            assert!(
                Instant::now() < deadline,
                "reserved waiter must finish after natural exit"
            );
            std::thread::sleep(WAIT_INTERVAL);
        }
        // ~keep The caller intentionally retains the tree; compare only released child stderr before tree teardown.
        assert!(
            resources().1 < with_child.1,
            "child stderr must close while caller owns the tree"
        );
        assert!(tree.stop(DROP_TIMEOUT));
        drop(tree);
        resources_return_to(baseline);
    }

    #[test]
    fn failed_program_spawn_releases_its_guardian_and_waiter_reservation() {
        if !runs_in_isolated_process("failed_program_spawn_releases_its_guardian_and_waiter_reservation") {
            return;
        }
        let runtime = runtime();
        let _entered = runtime.enter();
        // ~keep Browser initialization canonicalizes asynchronously; exclude Tokio's retained filesystem worker.
        runtime
            .block_on(tokio::fs::canonicalize("/bin/sleep"))
            .expect("warm the runtime filesystem worker");
        let baseline = resources();
        let error = Command::new("/crawlberg-no-such-managed-program")
            .spawn_managed()
            .expect_err("missing executable");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        resources_return_to(baseline);
        for pipe in [false, true] {
            let config = crate::browser::BrowserConfig::builder()
                .chrome_executable("/bin/sleep")
                .launch_timeout(Duration::from_millis(50))
                .build()
                .expect("launch config");
            let result = runtime.block_on(crate::Browser::launch_managed_with(config, |_| {
                let mut command = Command::new("/bin/sleep");
                command.arg("30").kill_on_drop(false).stderr(Stdio::piped());
                if pipe {
                    command.debugging_pipe();
                }
                command.spawn_managed()
            }));
            assert!(result.is_err(), "a stand-in cannot initialize either CDP transport");
            resources_return_to(baseline);
        }
    }

    #[test]
    fn completed_leader_rejects_signals_while_an_unrelated_process_stays_alive() {
        if !runs_in_isolated_process("completed_leader_rejects_signals_while_an_unrelated_process_stays_alive") {
            return;
        }
        let runtime = runtime();
        let _entered = runtime.enter();
        let baseline = resources();
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "exit 3"]);
        let mut child = command.spawn_managed().expect("managed child");
        assert_eq!(runtime.block_on(child.wait()).expect("exit status").code(), Some(3));
        let mut peer = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("peer");
        assert_eq!(child.id(), None);
        assert!(child.start_kill().is_err());
        assert!(child.start_kill_group().is_err());
        let peer_live = peer.try_wait().expect("peer status").is_none();
        peer.kill().expect("peer cleanup");
        peer.wait().expect("peer reap");
        assert!(peer_live, "completed handle must never signal a recycled PID or group");
        drop(child);
        resources_return_to(baseline);
    }

    #[test]
    #[allow(unsafe_code)]
    fn dropping_a_forked_handle_never_signals_or_waits_for_its_parent_child() {
        if !runs_in_isolated_process("dropping_a_forked_handle_never_signals_or_waits_for_its_parent_child") {
            return;
        }
        let runtime = runtime();
        let _entered = runtime.enter();
        let baseline = resources();
        let mut command = Command::new("/bin/sleep");
        command.arg("30").stderr(Stdio::null());
        let mut child = command.spawn_managed().expect("managed child");
        // SAFETY: the child only drops a no-stderr handle whose foreign-owner arm takes no locks, then exits. ~keep
        let fork = unsafe { libc::fork() };
        assert!(fork >= 0, "fork must start");
        if fork == 0 {
            drop(child);
            unsafe { libc::_exit(0) };
        }
        let mut status = 0;
        // SAFETY: only this test's fork child is waited for, into writable status storage. ~keep
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let waited = unsafe { libc::waitpid(fork, &mut status, libc::WNOHANG) };
            if waited == fork {
                break;
            }
            assert_eq!(waited, 0, "fork must remain waitable");
            if Instant::now() >= deadline {
                // SAFETY: this exact fork PID is retained unreaped, so it cannot name a foreign process. ~keep
                unsafe {
                    libc::kill(fork, libc::SIGKILL);
                    libc::waitpid(fork, &mut status, 0);
                }
                panic!("foreign handle drop must finish without an inherited lock");
            }
            std::thread::sleep(WAIT_INTERVAL);
        }
        assert_eq!(status, 0);
        assert!(
            child.try_wait().expect("parent child status").is_none(),
            "foreign drop must leave the parent's child alive"
        );
        child.tree().expect("tree").stop(DROP_TIMEOUT);
        runtime.block_on(child.wait()).expect("parent reaps child");
        drop(child);
        resources_return_to(baseline);
    }
}
