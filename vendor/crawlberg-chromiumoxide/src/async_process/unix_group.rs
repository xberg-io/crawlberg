use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const DROP_TIMEOUT: Duration = Duration::from_secs(2);
const OWNER_PROBE_TIMEOUT: Duration = Duration::from_secs(1);
// ~keep Poll the guardian's current PPID: a fork can retain pipe descriptors after its parent dies.
// ~keep Foreground probes/timers are reaped before the guardian kills its held group; no cached
// ~keep auxiliary PID is signalled after a shell could have reaped and recycled it.
#[cfg(any(not(target_os = "linux"), test))]
const GUARDIAN_SCRIPT: &str = "trap 'kill -s KILL -- -$$' TERM; printf .; \
    while owner=$(/bin/ps -o ppid= -p \"$$\") && [ \"$owner\" -eq \"$1\" ]; \
    do /bin/sleep 1 || break; done; kill -s KILL -- -$$";

// ~keep Linux procfs provides the live parent without requiring procps or BusyBox ps options.
#[cfg(target_os = "linux")]
const LINUX_GUARDIAN_SCRIPT: &str = "trap 'kill -9 -$$' TERM; printf .; \
    while owner= && [ -r /proc/$$/status ]; do \
    while read -r key value rest; do case \"$key\" in PPid:) owner=$value; break;; esac; \
    done < /proc/$$/status; [ \"$owner\" = \"$1\" ] || break; \
    /bin/sleep 1 || break; done; kill -9 -$$";

#[derive(Debug)]
pub(super) struct Group {
    owner: u32,
    id: u32,
    child: Mutex<Option<Child>>,
}

impl Group {
    pub(super) fn start() -> io::Result<Self> {
        #[cfg(target_os = "linux")]
        {
            let group = Self::start_script(LINUX_GUARDIAN_SCRIPT)?;
            validate_timer("/bin/sleep")?;
            let status = std::fs::read_to_string(format!("/proc/{}/status", group.id))?;
            if status_parent(&status) != Some(group.owner) {
                return Err(io::Error::other(
                    "guardian procfs parent did not identify this process as its owner",
                ));
            }
            Ok(group)
        }
        #[cfg(not(target_os = "linux"))]
        Self::start_with_tools("/bin/ps", "/bin/sleep")
    }

    #[cfg(any(not(target_os = "linux"), test))]
    pub(super) fn start_with_tools(probe: &str, timer: &str) -> io::Result<Self> {
        let group = Self::start_script(GUARDIAN_SCRIPT)?;
        validate_timer(timer).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("could not start browser guardian timer using {timer}: {error}"),
            )
        })?;
        validate_owner(probe, group.id, group.owner).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("could not establish browser guardian ownership using {probe}: {error}"),
            )
        })?;
        Ok(group)
    }

    fn start_script(script: &str) -> io::Result<Self> {
        // ~keep The guardian starts first and retains its own group ID against reuse.
        // ~keep The acknowledgement socket closes before Chrome starts; no ownership pipe survives a fork.
        let (mut ready, child_ready) = std::os::unix::net::UnixStream::pair()?;
        ready.set_read_timeout(Some(OWNER_PROBE_TIMEOUT))?;
        let child = Command::new("/bin/sh")
            .args([
                "-c",
                script,
                "crawlberg-browser-guardian",
                &std::process::id().to_string(),
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::from(std::os::fd::OwnedFd::from(child_ready)))
            .stderr(std::process::Stdio::null())
            .process_group(0)
            .spawn()?;
        let id = child.id();
        let group = Self {
            owner: std::process::id(),
            id,
            child: Mutex::new(Some(child)),
        };
        acknowledge_ready(&mut ready)?;
        Ok(group)
    }

    pub(super) fn id(&self) -> u32 {
        self.id
    }

    pub(super) fn signal(&self) -> io::Result<()> {
        if self.owner != std::process::id() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "browser group belongs to another process",
            ));
        }
        let child = self.child.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if child.is_none() {
            return Ok(());
        }
        signal_group(self.id)
    }

    pub(super) fn stop(&self, limit: Duration) -> bool {
        if self.owner != std::process::id() {
            return false;
        }
        let deadline = Instant::now() + limit;
        let mut child = self.child.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if child.is_none() {
            return true;
        }
        if signal_group(self.id).is_err() {
            return false;
        }
        loop {
            let status = match child.as_mut() {
                Some(child) => child.try_wait(),
                None => return true,
            };
            match status {
                Ok(Some(_)) => {
                    child.take();
                    return true;
                }
                Err(_) => return false,
                Ok(None) if Instant::now() >= deadline => return false,
                Ok(None) => std::thread::sleep(POLL_INTERVAL),
            }
        }
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        if self.owner == std::process::id() && !self.stop(DROP_TIMEOUT) {
            tracing::warn!(
                group = self.id,
                "browser guardian did not stop within the cleanup deadline"
            );
        }
    }
}

#[allow(unsafe_code)]
fn signal_group(id: u32) -> io::Result<()> {
    const SIGTERM: i32 = 15;
    const ESRCH: i32 = 3;
    let id = i32::try_from(id).map_err(|_| io::Error::other("browser group ID is too large"))?;
    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    // SAFETY: the mutex retains the unreaped guardian, so this group ID cannot be recycled.
    // TERM stops the foreground helper promptly; the guardian reaps it before its KILL trap.
    if unsafe { kill(-id, SIGTERM) } != 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ESRCH) {
            return Err(error);
        }
    }
    Ok(())
}

#[cfg(any(not(target_os = "linux"), test))]
fn validate_owner(probe: &str, guardian: u32, expected: u32) -> io::Result<()> {
    use std::io::Read;
    let mut child = Command::new(probe)
        .args(["-o", "ppid=", "-p", &guardian.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    let status = wait_probe(&mut child)?;
    let mut output = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        stdout.read_to_string(&mut output)?;
    }
    if !status.success() || output.trim().parse::<u32>() != Ok(expected) {
        return Err(io::Error::other(
            "guardian PPID probe did not identify this process as its owner",
        ));
    }
    Ok(())
}

fn acknowledge_ready(ready: &mut std::os::unix::net::UnixStream) -> io::Result<()> {
    use std::io::Read;
    let mut acknowledgement = [0u8; 1];
    ready.read_exact(&mut acknowledgement).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("browser guardian did not acknowledge startup within its deadline: {error}"),
        )
    })?;
    if acknowledgement != [b'.'] {
        return Err(io::Error::other("invalid browser guardian startup acknowledgement"));
    }
    Ok(())
}

fn validate_timer(timer: &str) -> io::Result<()> {
    let mut child = Command::new(timer)
        .arg("0")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    if !wait_probe(&mut child)?.success() {
        return Err(io::Error::other("guardian timer refused its interval"));
    }
    Ok(())
}

fn wait_probe(child: &mut Child) -> io::Result<std::process::ExitStatus> {
    let deadline = Instant::now() + OWNER_PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::new(io::ErrorKind::TimedOut, "guardian PPID probe timed out"));
            }
            Ok(None) => std::thread::sleep(POLL_INTERVAL),
        }
    }
}

#[cfg(any(target_os = "linux", test))]
pub(super) fn status_parent(status: &str) -> Option<u32> {
    status
        .lines()
        .find_map(|line| line.strip_prefix("PPid:")?.trim().parse().ok())
}

#[cfg(test)]
mod ownership_tests {
    use super::*;

    #[test]
    fn an_inherited_group_refuses_signal_and_stop_without_touching_its_owner() {
        let mut group = Group::start().expect("guardian");
        let mut leaf = Command::new("/bin/sleep")
            .arg("30")
            .process_group(i32::try_from(group.id()).expect("group fits pid"))
            .spawn()
            .expect("group member");
        let owner = group.owner;
        group.owner = owner.wrapping_add(1);
        let refused = group.signal().expect_err("foreign process cannot signal this group");
        let stopped = group.stop(Duration::from_millis(100));
        let untouched = leaf.try_wait().expect("leaf status").is_none();
        group.owner = owner;
        assert!(group.stop(DROP_TIMEOUT), "actual owner must clean its group");
        leaf.wait().expect("reap group member");
        assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
        assert!(!stopped, "foreign stop must refuse ownership");
        assert!(untouched, "neither foreign operation may signal the owner's member");
    }
}
