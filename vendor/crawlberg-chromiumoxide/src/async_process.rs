//! Internal module providing an async child process abstraction.

use std::ffi::OsStr;
use std::pin::Pin;
pub use std::process::{ExitStatus, Stdio};
use std::task::{Context, Poll};

use tokio::process;

#[derive(Debug)]
pub struct Command {
    inner: process::Command,
    kill_on_drop: bool,
    #[cfg(unix)]
    debugging_pipe: bool,
}

impl Command {
    pub fn new<S: AsRef<OsStr>>(program: S) -> Self {
        let mut inner = process::Command::new(program);
        // Since the kill and/or wait methods are async, we can't call
        // explicitely in the Drop implementation. We MUST rely on the
        // runtime implemetation which is already designed to deal with
        // this case where the user didn't explicitely kill the child
        // process before dropping the handle.
        inner.kill_on_drop(true);
        Self {
            inner,
            kill_on_drop: true,
            #[cfg(unix)]
            debugging_pipe: false,
        }
    }

    pub fn arg<S: AsRef<OsStr>>(&mut self, arg: S) -> &mut Self {
        self.inner.arg(arg);
        self
    }

    pub fn get_args(&self) -> impl Iterator<Item = &OsStr> {
        self.inner.as_std().get_args()
    }

    pub fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.inner.args(args);
        self
    }

    pub fn envs<I, K, V>(&mut self, vars: I) -> &mut Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        self.inner.envs(vars);
        self
    }

    /// Whether the child process is killed when its handle is dropped. `true` by default. A
    /// caller that passes `false` stops the process itself.
    pub fn kill_on_drop(&mut self, kill_on_drop: bool) -> &mut Self {
        self.inner.kill_on_drop(kill_on_drop);
        self.kill_on_drop = kill_on_drop;
        self
    }

    pub fn stdin<T: Into<Stdio>>(&mut self, cfg: T) -> &mut Self {
        self.inner.stdin(cfg);
        self
    }

    pub fn stdout<T: Into<Stdio>>(&mut self, cfg: T) -> &mut Self {
        self.inner.stdout(cfg);
        self
    }

    pub fn stderr<T: Into<Stdio>>(&mut self, cfg: T) -> &mut Self {
        self.inner.stderr(cfg);
        self
    }

    /// Give the child a private debugging pipe on the descriptors 3 and 4, where a browser started
    /// with `--remote-debugging-pipe` reads and writes the protocol. The spawned [`Child`] holds
    /// this end of the pipe.
    #[cfg(unix)]
    pub(crate) fn debugging_pipe(&mut self) -> &mut Self {
        self.debugging_pipe = true;
        self
    }

    pub fn spawn(&mut self) -> std::io::Result<Child> {
        #[cfg(unix)]
        let pipe = if self.debugging_pipe {
            Some(self.attach_debugging_pipe()?)
        } else {
            None
        };
        let (inner, tree) = spawn_in_tree(&mut self.inner)?;
        Ok(Child::new(
            inner,
            tree,
            self.kill_on_drop,
            #[cfg(unix)]
            pipe,
        ))
    }

    /// Spawn a child whose waits do not register a process-wide signal handler on Unix.
    pub fn spawn_managed(&mut self) -> std::io::Result<ManagedChild> {
        #[cfg(unix)]
        {
            let reaper = managed::NativeReaper::reserve()?;
            let pipe = if self.debugging_pipe {
                Some(self.attach_debugging_pipe()?)
            } else {
                None
            };
            managed::normalize_reaping()?;
            let group = std::sync::Arc::new(unix_group::Group::start()?);
            let group_id =
                i32::try_from(group.id()).map_err(|_| std::io::Error::other("browser group ID is too large"))?;
            use std::os::unix::process::CommandExt;
            self.inner.as_std_mut().process_group(group_id);
            ManagedChild::new(
                self.inner.as_std_mut().spawn()?,
                ProcessTree(group),
                self.kill_on_drop,
                pipe,
                reaper,
            )
        }
        #[cfg(not(unix))]
        self.spawn()
    }

    /// Make the pipe of the next child and return this end of it.
    #[cfg(unix)]
    fn attach_debugging_pipe(&mut self) -> std::io::Result<std::os::unix::net::UnixStream> {
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
        let (parent, child) = std::os::unix::net::UnixStream::pair()?;
        // SAFETY: duplicating above Chrome's reserved fds avoids collisions during pre_exec. ~keep
        let fd = unsafe { libc::fcntl(child.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 5) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: fcntl returned a new descriptor whose sole owner is this value. ~keep
        let descriptor = unsafe { OwnedFd::from_raw_fd(fd) };
        // SAFETY: only async-signal-safe descriptor operations run between fork and exec. ~keep
        unsafe {
            self.inner.pre_exec(move || {
                if libc::dup2(descriptor.as_raw_fd(), 3) < 0 || libc::dup2(descriptor.as_raw_fd(), 4) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Ok(parent)
    }
}

#[derive(Debug)]
pub struct Child {
    pub stderr: Option<ChildStderr>,
    pub inner: process::Child,
    tree: Option<ProcessTree>,
    /// The process that spawned this child.
    spawner: u32,
    /// Whether the runtime kills the child when this handle is dropped.
    killed_on_drop: bool,
    /// This end of the private debugging pipe, for a child that was started on one.
    #[cfg(unix)]
    debugging_pipe: Option<std::os::unix::net::UnixStream>,
}

/// Wrapper for an async child process.
impl Child {
    fn new(
        mut inner: process::Child,
        tree: Option<ProcessTree>,
        killed_on_drop: bool,
        #[cfg(unix)] debugging_pipe: Option<std::os::unix::net::UnixStream>,
    ) -> Self {
        let stderr = inner.stderr.take();
        Self {
            inner,
            stderr: stderr.map(|inner| ChildStderr { inner }),
            tree,
            spawner: std::process::id(),
            killed_on_drop,
            #[cfg(unix)]
            debugging_pipe,
        }
    }

    /// Take this end of the private debugging pipe. `None` for a child that was started with a
    /// debugging port, and after the first call.
    #[cfg(unix)]
    pub(crate) fn take_debugging_pipe(&mut self) -> Option<std::os::unix::net::UnixStream> {
        self.debugging_pipe.take()
    }

    /// Whether the runtime kills the child when this handle is dropped. `false` when the caller
    /// asked for that with [`Command::kill_on_drop`]: it stops the process itself.
    pub fn killed_on_drop(&self) -> bool {
        self.killed_on_drop
    }

    /// Whether this process spawned the child. `false` in a process forked from the one that did:
    /// it holds a copy of this handle, and the child is not its own to wait for or to end.
    pub fn spawned_here(&self) -> bool {
        self.spawner == std::process::id()
    }

    /// The tree of processes this child is the root of, where the operating system keeps one.
    pub fn tree(&self) -> Option<&ProcessTree> {
        self.tree.as_ref()
    }

    /// The child ID while its exit status has not been collected.
    pub fn id(&self) -> Option<u32> {
        self.inner.id()
    }

    fn ensure_spawned_here(&self) -> std::io::Result<()> {
        if self.spawned_here() {
            Ok(())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "child belongs to another process",
            ))
        }
    }

    /// Signal this child without waiting for its exit status.
    pub fn start_kill(&mut self) -> std::io::Result<()> {
        self.ensure_spawned_here()?;
        self.inner.start_kill()
    }

    /// Kill the child process synchronously and asynchronously wait for the
    /// child to exit
    pub async fn kill(&mut self) -> std::io::Result<()> {
        self.ensure_spawned_here()?;
        // Tokio already waits internally
        self.inner.kill().await
    }

    /// Signal this launch's Unix process group, before waiting for its leader.
    ///
    /// ~keep The guardian holds the group ID even when a non-exec launcher exits. The launch
    /// ~keep handle must still be unreaped, so a completed handle cannot initiate teardown.
    #[cfg(unix)]
    pub fn start_kill_group(&mut self) -> std::io::Result<()> {
        if !self.spawned_here() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "the browser group belongs to another process",
            ));
        }
        if self.inner.id().is_none() {
            return Err(std::io::Error::other("the browser leader was already reaped"));
        }
        self.tree
            .as_ref()
            .ok_or_else(|| std::io::Error::other("no owned browser group"))?
            .0
            .signal()
    }

    /// Asynchronously wait for the child process to exit
    pub async fn wait(&mut self) -> std::io::Result<ExitStatus> {
        self.ensure_spawned_here()?;
        self.inner.wait().await
    }

    /// If the child process has exited, get its status
    pub fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        self.ensure_spawned_here()?;
        self.inner.try_wait()
    }

    /// Return a mutable reference to the inner process
    ///
    /// `stderr` may not be available.
    pub fn as_mut_inner(&mut self) -> &mut process::Child {
        &mut self.inner
    }

    /// Return the inner process
    ///
    /// The process ends when the last clone of its [`ProcessTree`] is dropped. Clone
    /// [`Self::tree`] first to keep it running.
    pub fn into_inner(self) -> process::Child {
        let mut inner = self.inner;
        inner.stderr = self.stderr.map(ChildStderr::into_inner);
        inner
    }
}

#[derive(Debug)]
pub struct ChildStderr {
    pub inner: process::ChildStderr,
}

impl ChildStderr {
    pub fn into_inner(self) -> process::ChildStderr {
        self.inner
    }
}

impl futures::AsyncRead for ChildStderr {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<std::io::Result<usize>> {
        let mut buf = tokio::io::ReadBuf::new(buf);
        futures::ready!(tokio::io::AsyncRead::poll_read(Pin::new(&mut self.inner), cx, &mut buf))?;
        Poll::Ready(Ok(buf.filled().len()))
    }
}

/// The processes of a spawned child, as the operating system groups them.
///
/// On Windows it is a job object that the child joins before it runs its first instruction, so
/// every process the child starts is born in it. The system ends them all when the last clone is
/// dropped and when the owning process ends, by any route. On Unix a guardian owns the
/// group and ends it when the guardian observes parent death, including signal termination.
/// Other platforms return `None` from [`Child::tree`].
#[derive(Debug, Clone)]
pub struct ProcessTree(
    #[cfg(windows)] std::sync::Arc<job::Job>,
    #[cfg(unix)] std::sync::Arc<unix_group::Group>,
    #[cfg(not(any(windows, unix)))] std::convert::Infallible,
);

impl ProcessTree {
    /// End every process in the tree and wait until each one has ended, for at most `limit`.
    /// Returns whether none is left. It starts no process, and a tree that is already empty
    /// returns at once.
    pub fn stop(&self, limit: std::time::Duration) -> bool {
        #[cfg(windows)]
        {
            self.0.stop(std::time::Instant::now() + limit)
        }
        #[cfg(unix)]
        {
            self.0.stop(limit)
        }
        #[cfg(not(any(windows, unix)))]
        {
            let _ = limit;
            match self.0 {}
        }
    }
}

/// Spawn `command`, in a [`ProcessTree`] of its own where the platform has one.
#[cfg(unix)]
fn spawn_in_tree(command: &mut process::Command) -> std::io::Result<(process::Child, Option<ProcessTree>)> {
    // SAFETY: sigaction writes the live action; querying with a null new action changes no handler. ~keep
    let mut inherited: libc::sigaction = unsafe { std::mem::zeroed() };
    if unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), &mut inherited) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // ~keep Auto-reaping breaks guardian probes. Waitable host handlers need no normalization;
    // ~keep Tokio registration otherwise drops Go's SA_ONSTACK even when its Linux pidfd path needs no signal.
    if inherited.sa_sigaction == libc::SIG_IGN || inherited.sa_flags & libc::SA_NOCLDWAIT != 0 {
        let _child_signals = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::child())?;
    }
    let group = std::sync::Arc::new(unix_group::Group::start()?);
    command
        .process_group(i32::try_from(group.id()).map_err(|_| std::io::Error::other("browser group ID is too large"))?);
    Ok((command.spawn()?, Some(ProcessTree(group))))
}

#[cfg(not(any(windows, unix)))]
fn spawn_in_tree(command: &mut process::Command) -> std::io::Result<(process::Child, Option<ProcessTree>)> {
    Ok((command.spawn()?, None))
}

/// Spawn `command`, in a [`ProcessTree`] of its own where the platform has one.
///
/// The child starts suspended and is resumed once it is in the job. A child that cannot join is
/// ended before it has run: no browser runs outside a job.
#[cfg(windows)]
fn spawn_in_tree(command: &mut process::Command) -> std::io::Result<(process::Child, Option<ProcessTree>)> {
    use windows_sys::Win32::System::Threading::CREATE_SUSPENDED;
    let refused = |error: std::io::Error| {
        std::io::Error::new(
            error.kind(),
            format!("could not put the browser process in a job object: {error}"),
        )
    };
    let mut child = command.creation_flags(CREATE_SUSPENDED).spawn()?;
    let joined = match (child.raw_handle(), child.id()) {
        (Some(process), Some(pid)) => job::Job::holding(process, pid),
        _ => Err(std::io::Error::other("the browser process ended as soon as it started")),
    };
    match joined {
        Ok(job) => Ok((child, Some(ProcessTree(std::sync::Arc::new(job))))),
        Err(error) => {
            let _ = child.start_kill();
            Err(refused(error))
        }
    }
}

/// The length in bytes of the two 32-bit counts that start a `JOBOBJECT_BASIC_PROCESS_ID_LIST`.
/// The first process id follows them at once, for a 4-byte and for an 8-byte id alike.
#[cfg(any(windows, test))]
const JOB_ID_LIST_HEADER: usize = 8;

/// The offset of the count of listed ids in a `JOBOBJECT_BASIC_PROCESS_ID_LIST`.
#[cfg(any(windows, test))]
const JOB_ID_LIST_COUNT: usize = 4;

/// The process ids in `list`, the bytes of a `JOBOBJECT_BASIC_PROCESS_ID_LIST` whose ids are
/// `id_width` bytes each (the pointer width of the system that wrote it: 4 or 8).
///
/// The second count gives the number of ids. Only the ids that `list` holds in full are read,
/// whatever the count says. An id that does not fit a `u32` is not a process id and is left out.
#[cfg(any(windows, test))]
fn job_process_ids(list: &[u8], id_width: usize) -> Vec<u32> {
    let Some(listed) = list.get(JOB_ID_LIST_COUNT..JOB_ID_LIST_HEADER) else {
        return Vec::new();
    };
    let listed = u32::from_le_bytes([listed[0], listed[1], listed[2], listed[3]]) as usize;
    if !matches!(id_width, 4 | 8) {
        return Vec::new();
    }
    list[JOB_ID_LIST_HEADER..]
        .chunks_exact(id_width)
        .take(listed)
        .filter_map(|id| {
            let mut wide = [0u8; 8];
            wide[..id_width].copy_from_slice(id);
            u32::try_from(u64::from_le_bytes(wide)).ok()
        })
        .collect()
}

// ~keep ERROR_MORE_DATA does not promise a usable partial list; retry with a larger aligned buffer.
#[cfg(any(windows, test))]
fn query_job_process_ids(
    id_width: usize,
    mut capacity: usize,
    mut query: impl FnMut(&mut [u64], u32) -> std::io::Result<bool>,
) -> std::io::Result<Vec<u32>> {
    loop {
        let bytes = capacity
            .checked_mul(id_width)
            .and_then(|bytes| bytes.checked_add(JOB_ID_LIST_HEADER))
            .and_then(|bytes| u32::try_from(bytes).ok())
            .ok_or_else(|| std::io::Error::other("job member list is too large"))?;
        let mut list = vec![0u64; (bytes as usize).div_ceil(size_of::<u64>())];
        if query(&mut list, bytes)? {
            let list: Vec<u8> = list.iter().flat_map(|word| word.to_ne_bytes()).collect();
            return Ok(job_process_ids(&list[..bytes as usize], id_width));
        }
        capacity = capacity
            .checked_mul(2)
            .filter(|capacity| *capacity != 0)
            .ok_or_else(|| std::io::Error::other("job member list is too large"))?;
    }
}

#[cfg(any(windows, test))]
fn opened_job_member<T>(result: std::io::Result<T>, vanished_error: i32) -> std::io::Result<Option<T>> {
    match result {
        Ok(member) => Ok(Some(member)),
        Err(error) if error.raw_os_error() == Some(vanished_error) => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::{job_process_ids, opened_job_member, query_job_process_ids};

    #[test]
    fn inaccessible_members_are_unknown_but_a_vanished_member_is_absent() {
        let denied = opened_job_member::<()>(Err(std::io::Error::from_raw_os_error(5)), 87)
            .expect_err("access denied must not hide a live member");
        assert_eq!(denied.raw_os_error(), Some(5));
        assert_eq!(
            opened_job_member::<()>(Err(std::io::Error::from_raw_os_error(87)), 87).unwrap(),
            None
        );
        assert_eq!(opened_job_member(Ok(1100), 87).unwrap(), Some(1100));
    }

    #[test]
    fn a_failed_member_query_is_not_an_empty_job() {
        let error = query_job_process_ids(8, 2, |_, _| Err(std::io::Error::from_raw_os_error(5)))
            .expect_err("an inaccessible job must remain unknown");
        assert_eq!(error.raw_os_error(), Some(5));
    }

    #[test]
    fn more_members_retry_without_using_the_partial_result() {
        for width in [4, 8] {
            let mut calls = 0;
            let ids = query_job_process_ids(width, 2, |buffer, bytes| {
                calls += 1;
                if calls == 1 {
                    assert_eq!(bytes as usize, 8 + 2 * width);
                    return Ok(false);
                }
                assert_eq!(bytes as usize, 8 + 4 * width);
                let data = list(3, 3, &[1100, 2200, 3300], width);
                for (index, chunk) in data.chunks(8).enumerate() {
                    let mut word = [0u8; 8];
                    word[..chunk.len()].copy_from_slice(chunk);
                    buffer[index] = u64::from_ne_bytes(word);
                }
                Ok(true)
            })
            .expect("the larger query succeeds");
            assert_eq!(calls, 2);
            assert_eq!(ids, [1100, 2200, 3300]);
        }
    }

    // ~keep The refusal signals a group before waiting; prove Tokio retains an exited leader.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_exited_group_leader_is_not_reaped_until_its_handle_is_waited() {
        let mut command = super::Command::new("sh");
        command.args(["-c", "exit 3"]).kill_on_drop(false);
        let mut child = command.spawn().expect("launcher must start");
        let pid = child.inner.id().expect("unwaited child must have a pid");
        let mut peer = std::process::Command::new("sleep")
            .arg("300")
            .spawn()
            .expect("peer must start");
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        let zombie = loop {
            #[cfg(target_os = "linux")]
            let zombie = std::fs::read_to_string(format!("/proc/{pid}/status"))
                .expect("process status must be readable")
                .lines()
                .any(|line| line.starts_with("State:") && line.split_whitespace().nth(1) == Some("Z"));
            #[cfg(not(target_os = "linux"))]
            let zombie = {
                let state = std::process::Command::new("ps")
                    .args(["-o", "stat=", "-p", &pid.to_string()])
                    .output()
                    .expect("process status must be readable");
                String::from_utf8_lossy(&state.stdout).trim().starts_with('Z')
            };
            if zombie {
                break true;
            }
            if tokio::time::Instant::now() >= deadline {
                break false;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        };
        let held_id = child.inner.id();
        let status = child.try_wait().expect("exited leader must be waitable");
        let cleared_id = child.inner.id();
        let after_reap = child.start_kill_group();
        let peer_running = peer.try_wait().expect("peer must be waitable").is_none();
        let _ = peer.kill();
        let _ = peer.wait();
        assert!(zombie, "runtime must leave the held leader unreaped after SIGCHLD");
        assert_eq!(held_id, Some(pid));
        assert_eq!(status.and_then(|status| status.code()), Some(3));
        assert_eq!(cleared_id, None);
        assert!(
            after_reap.is_err(),
            "reaped leader must never supply a recycled group id"
        );
        assert!(peer_running, "unrelated group must stay alive");
    }

    /// The bytes of a list with the two counts `assigned` and `listed`, then `ids` at `id_width` bytes each.
    fn list(assigned: u32, listed: u32, ids: &[u64], id_width: usize) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend(assigned.to_le_bytes());
        bytes.extend(listed.to_le_bytes());
        for id in ids {
            bytes.extend(&id.to_le_bytes()[..id_width]);
        }
        bytes
    }

    #[test]
    fn the_ids_of_a_32_bit_list_start_after_both_counts() {
        assert_eq!(job_process_ids(&list(2, 2, &[1100, 2200], 4), 4), [1100, 2200]);
    }

    #[test]
    fn the_ids_of_a_64_bit_list_start_after_both_counts() {
        assert_eq!(job_process_ids(&list(2, 2, &[1100, 2200], 8), 8), [1100, 2200]);
    }

    #[test]
    fn a_count_larger_than_the_list_holds_reads_only_the_ids_that_are_there() {
        assert_eq!(job_process_ids(&list(9, 9, &[1100, 2200], 4), 4), [1100, 2200]);
        assert_eq!(job_process_ids(&list(9, u32::MAX, &[1100, 2200], 8), 8), [1100, 2200]);
    }

    #[test]
    fn a_count_smaller_than_the_list_holds_stops_at_the_count() {
        assert_eq!(job_process_ids(&list(3, 1, &[1100, 2200], 4), 4), [1100]);
        assert_eq!(job_process_ids(&list(3, 1, &[1100, 2200], 8), 8), [1100]);
    }

    #[test]
    fn a_zero_count_gives_no_id() {
        assert!(job_process_ids(&list(0, 0, &[1100, 2200], 4), 4).is_empty());
        assert!(job_process_ids(&list(0, 0, &[], 8), 8).is_empty());
    }

    #[test]
    fn an_id_cut_short_by_the_end_of_the_list_is_not_read() {
        for id_width in [4, 8] {
            let whole = list(2, 2, &[1100, 2200], id_width);
            for cut in 1..id_width {
                assert_eq!(
                    job_process_ids(&whole[..whole.len() - cut], id_width),
                    [1100],
                    "{id_width} {cut}"
                );
            }
        }
    }

    #[test]
    fn a_list_shorter_than_its_counts_gives_no_id() {
        let whole = list(2, 2, &[], 4);
        for length in 0..whole.len() {
            assert!(job_process_ids(&whole[..length], 4).is_empty(), "{length}");
            assert!(job_process_ids(&whole[..length], 8).is_empty(), "{length}");
        }
    }

    #[test]
    fn an_id_that_is_wider_than_32_bits_is_left_out() {
        assert_eq!(job_process_ids(&list(2, 2, &[1 << 32, 2200], 8), 8), [2200]);
        assert_eq!(job_process_ids(&list(1, 1, &[u64::from(u32::MAX)], 8), 8), [u32::MAX]);
    }

    #[test]
    fn a_width_that_no_windows_has_gives_no_id() {
        for id_width in [0, 1, 2, 16] {
            assert!(
                job_process_ids(&list(2, 2, &[1100, 2200], 8), id_width).is_empty(),
                "{id_width}"
            );
        }
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod job {
    use std::io;
    use std::os::windows::io::{AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle, RawHandle};
    use std::time::Instant;

    use windows_sys::Win32::Foundation::{ERROR_MORE_DATA, INVALID_HANDLE_VALUE, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_BASIC_PROCESS_ID_LIST, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicProcessIdList,
        JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, OpenThread, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, ResumeThread,
        THREAD_SUSPEND_RESUME, WaitForSingleObject,
    };

    // ~keep Start with a modest allocation and grow on ERROR_MORE_DATA instead of trusting partial data.
    const MEMBERS_PER_ROUND: usize = 256;

    //The decoder reads the layout of the structure that the system writes, for this target.
    const _: () = {
        assert!(
            std::mem::offset_of!(JOBOBJECT_BASIC_PROCESS_ID_LIST, NumberOfProcessIdsInList) == super::JOB_ID_LIST_COUNT
        );
        assert!(std::mem::offset_of!(JOBOBJECT_BASIC_PROCESS_ID_LIST, ProcessIdList) == super::JOB_ID_LIST_HEADER);
        assert!(size_of::<JOBOBJECT_BASIC_PROCESS_ID_LIST>() == super::JOB_ID_LIST_HEADER + size_of::<usize>());
    };

    /// A job object that ends its processes when its handle is closed, and the process it was
    /// made for.
    #[derive(Debug)]
    pub(super) struct Job {
        job: OwnedHandle,
        root: OwnedHandle,
    }

    fn owned(handle: RawHandle) -> io::Result<OwnedHandle> {
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the call that returned `handle` opened it for this caller alone.
        Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
    }

    fn done(result: i32) -> io::Result<()> {
        if result == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    impl Job {
        /// Make a job, put the suspended process `process` (id `pid`) in it and resume it.
        pub(super) fn holding(process: RawHandle, pid: u32) -> io::Result<Self> {
            // SAFETY: both arguments may be null: default security, which is not inheritable, and no name.
            let job = owned(unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) })?;
            // SAFETY: the structure is plain data, and all zero is its "no limit" value.
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: `limits` is the structure this information class takes, with its own size.
            done(unsafe {
                SetInformationJobObject(
                    job.as_raw_handle(),
                    JobObjectExtendedLimitInformation,
                    (&raw const limits).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            })?;
            // SAFETY: the caller's `process` is a live process handle for the length of this call.
            let root = unsafe { BorrowedHandle::borrow_raw(process) }.try_clone_to_owned()?;
            // SAFETY: both handles are live.
            done(unsafe { AssignProcessToJobObject(job.as_raw_handle(), process) })?;
            resume(pid)?;
            Ok(Self { job, root })
        }

        /// End every process in the job, and wait until `deadline` for the root and for each
        /// member to have ended. Returns whether none is left.
        ///
        /// A process handle is signalled once the system has closed every file the process held,
        /// which is what a caller that removes the browser's profile waits for.
        pub(super) fn stop(&self, deadline: Instant) -> bool {
            loop {
                //The members are opened before the kill, so each one is waited for by its
                //own handle and never by a process id that the system may have given away.
                let members = self.members();
                // SAFETY: the job handle is live. A job with no process left is ended at once.
                unsafe { TerminateJobObject(self.job.as_raw_handle(), 1) };
                let Ok(members) = members else {
                    return false;
                };
                if !ended(&self.root, deadline) || !members.iter().all(|member| ended(member, deadline)) {
                    return false;
                }
                if members.is_empty() {
                    return true;
                }
            }
        }

        /// A handle on each process that is in the job now.
        fn members(&self) -> io::Result<Vec<OwnedHandle>> {
            let ids = process_ids(self.job.as_raw_handle(), MEMBERS_PER_ROUND)?;
            ids.into_iter()
                .map(|pid| {
                    // SAFETY: plain call; ERROR_INVALID_PARAMETER means this PID no longer exists.
                    let member = super::opened_job_member(
                        owned(unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) }),
                        windows_sys::Win32::Foundation::ERROR_INVALID_PARAMETER as i32,
                    )?;
                    let Some(member) = member else {
                        return Ok(None);
                    };
                    let mut in_job = 0;
                    // SAFETY: both handles are live and `in_job` is writable.
                    done(unsafe { IsProcessInJob(member.as_raw_handle(), self.job.as_raw_handle(), &raw mut in_job) })?;
                    Ok((in_job != 0).then_some(member))
                })
                .collect::<io::Result<Vec<_>>>()
                .map(|members| members.into_iter().flatten().collect())
        }
    }

    fn process_ids(job: RawHandle, capacity: usize) -> io::Result<Vec<u32>> {
        super::query_job_process_ids(size_of::<usize>(), capacity, |list, bytes| {
            // SAFETY: the u64 buffer is aligned and writable for the supplied byte count.
            if unsafe {
                QueryInformationJobObject(
                    job,
                    JobObjectBasicProcessIdList,
                    list.as_mut_ptr().cast(),
                    bytes,
                    std::ptr::null_mut(),
                )
            } != 0
            {
                return Ok(true);
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_MORE_DATA as i32) {
                Ok(false)
            } else {
                Err(error)
            }
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::os::windows::process::CommandExt;
        use std::process::Command;
        use std::time::Duration;
        use windows_sys::Win32::System::Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED};

        struct Children(Vec<std::process::Child>);

        impl Drop for Children {
            fn drop(&mut self) {
                for child in &mut self.0 {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }

        #[test]
        fn an_invalid_job_query_returns_an_error() {
            let error = process_ids(INVALID_HANDLE_VALUE, 2).expect_err("invalid job handle");
            assert_eq!(
                error.raw_os_error(),
                Some(windows_sys::Win32::Foundation::ERROR_INVALID_HANDLE as i32)
            );
        }

        #[test]
        fn a_job_with_more_members_than_the_initial_buffer_reports_every_member() {
            let mut children = Children(Vec::new());
            for _ in 0..3 {
                children.0.push(
                    Command::new("powershell.exe")
                        .args(["-NoProfile", "-Command", "Start-Sleep -Seconds 60"])
                        .creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW)
                        .spawn()
                        .expect("spawn suspended process"),
                );
            }
            let job = Job::holding(children.0[0].as_raw_handle(), children.0[0].id()).expect("start root in job");
            for child in &children.0[1..] {
                // SAFETY: both handles are live; the process is still suspended.
                done(unsafe { AssignProcessToJobObject(job.job.as_raw_handle(), child.as_raw_handle()) })
                    .expect("assign member to job");
                resume(child.id()).expect("resume member");
            }
            let ids = process_ids(job.job.as_raw_handle(), 2).expect("retry larger member query");
            // ~keep A resumed member can start processes of its own, and the job holds them too.
            // ~keep The list holds the three members and can hold more.
            for child in &children.0 {
                assert!(ids.contains(&child.id()), "member {} is not in {ids:?}", child.id());
            }
            // ~keep The list holds no id from outside the job: each listed process that still exists is a member.
            for &pid in &ids {
                // SAFETY: plain call; ERROR_INVALID_PARAMETER means this PID no longer exists.
                let listed = super::super::opened_job_member(
                    owned(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) }),
                    windows_sys::Win32::Foundation::ERROR_INVALID_PARAMETER as i32,
                )
                .expect("open listed process");
                let Some(listed) = listed else {
                    continue;
                };
                let mut in_job = 0;
                // SAFETY: both handles are live and `in_job` is writable.
                done(unsafe { IsProcessInJob(listed.as_raw_handle(), job.job.as_raw_handle(), &raw mut in_job) })
                    .expect("ask whether the listed process is in the job");
                assert!(in_job != 0, "listed id {pid} is not in the job: {ids:?}");
            }
            assert!(job.stop(Instant::now() + Duration::from_secs(10)));
            for child in &mut children.0 {
                child.wait().expect("reap ended member");
            }
        }
    }

    /// Whether `process` has ended by `deadline`.
    fn ended(process: &OwnedHandle, deadline: Instant) -> bool {
        let left = deadline.saturating_duration_since(Instant::now()).as_millis();
        //`u32::MAX` means no limit to the system, so the wait is capped below it.
        let left = u32::try_from(left).unwrap_or(u32::MAX - 1).min(u32::MAX - 1);
        // SAFETY: the handle is live and was opened with the right to wait on it.
        unsafe { WaitForSingleObject(process.as_raw_handle(), left) == WAIT_OBJECT_0 }
    }

    /// Resume the one thread of the suspended process `pid`.
    fn resume(pid: u32) -> io::Result<()> {
        // SAFETY: plain call. A thread snapshot covers the whole system, whatever the second argument.
        let threads = owned(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) })?;
        // SAFETY: the structure is plain data. The system reads its size from `dwSize`.
        let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
        entry.dwSize = size_of::<THREADENTRY32>() as u32;
        // SAFETY: the snapshot is live and `entry` is writable, here and in the loop.
        let mut more = unsafe { Thread32First(threads.as_raw_handle(), &raw mut entry) } != 0;
        let mut resumed = false;
        // Every thread of the process is resumed: software that watches process starts can add
        // a thread of its own before the first one has run.
        while more {
            if entry.th32OwnerProcessID == pid {
                // SAFETY: plain call.
                let thread = owned(unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) })?;
                // SAFETY: the thread handle is live. The call returns `u32::MAX` when it fails.
                if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
                    return Err(io::Error::last_os_error());
                }
                resumed = true;
            }
            // SAFETY: as above.
            more = unsafe { Thread32Next(threads.as_raw_handle(), &raw mut entry) } != 0;
        }
        if resumed {
            Ok(())
        } else {
            Err(io::Error::other(
                "the browser process could not be started: it has no thread to run",
            ))
        }
    }
}

#[cfg(all(unix, test))]
#[path = "async_process/unix_tests.rs"]
mod unix_tests;

#[cfg(unix)]
#[path = "async_process/unix_group.rs"]
mod unix_group;

#[cfg(all(test, unix))]
mod unix_signal_tests;

#[cfg(unix)]
#[path = "async_process/managed.rs"]
mod managed;
#[cfg(unix)]
pub use managed::ManagedChild;
#[cfg(not(unix))]
pub type ManagedChild = Child;

#[derive(Debug)]
pub(crate) enum SpawnedChild {
    Runtime(Child),
    #[cfg(unix)]
    Managed(ManagedChild),
}

impl SpawnedChild {
    pub(crate) fn runtime_mut(&mut self) -> Option<&mut Child> {
        match self {
            Self::Runtime(child) => Some(child),
            #[cfg(unix)]
            Self::Managed(_) => None,
        }
    }

    pub(crate) fn managed_mut(&mut self) -> Option<&mut ManagedChild> {
        match self {
            #[cfg(unix)]
            Self::Runtime(_) => None,
            #[cfg(not(unix))]
            Self::Runtime(child) => Some(child),
            #[cfg(unix)]
            Self::Managed(child) => Some(child),
        }
    }

    pub(crate) fn spawned_here(&self) -> bool {
        match self {
            Self::Runtime(child) => child.spawned_here(),
            #[cfg(unix)]
            Self::Managed(child) => child.spawned_here(),
        }
    }

    pub(crate) fn killed_on_drop(&self) -> bool {
        match self {
            Self::Runtime(child) => child.killed_on_drop(),
            #[cfg(unix)]
            Self::Managed(child) => child.killed_on_drop(),
        }
    }

    pub(crate) fn id(&self) -> Option<u32> {
        match self {
            Self::Runtime(child) => child.id(),
            #[cfg(unix)]
            Self::Managed(child) => child.id(),
        }
    }

    pub(crate) fn take_stderr(&mut self) -> Option<ChildStderr> {
        match self {
            Self::Runtime(child) => child.stderr.take(),
            #[cfg(unix)]
            Self::Managed(child) => child.stderr.take(),
        }
    }

    #[cfg(unix)]
    pub(crate) fn take_debugging_pipe(&mut self) -> Option<std::os::unix::net::UnixStream> {
        match self {
            Self::Runtime(child) => child.take_debugging_pipe(),
            Self::Managed(child) => child.take_debugging_pipe(),
        }
    }

    pub(crate) fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        match self {
            Self::Runtime(child) => child.try_wait(),
            #[cfg(unix)]
            Self::Managed(child) => child.try_wait(),
        }
    }

    pub(crate) async fn wait(&mut self) -> std::io::Result<ExitStatus> {
        match self {
            Self::Runtime(child) => child.wait().await,
            #[cfg(unix)]
            Self::Managed(child) => child.wait().await,
        }
    }

    pub(crate) async fn kill(&mut self) -> std::io::Result<()> {
        match self {
            Self::Runtime(child) => child.kill().await,
            #[cfg(unix)]
            Self::Managed(child) => child.kill().await,
        }
    }
}
