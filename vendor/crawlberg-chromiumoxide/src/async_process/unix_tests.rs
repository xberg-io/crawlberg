use std::os::unix::process::CommandExt;
use std::time::{Duration, Instant};

const SIGINT: i32 = 2;
const SIGKILL: i32 = 9;

#[tokio::test]
#[ignore = "subprocess fixture"]
async fn signal_parent_fixture() {
    let Ok(marker) = std::env::var("CRAWLBERG_SIGNAL_FIXTURE") else {
        return;
    };
    if std::env::var_os("CRAWLBERG_SIGNAL_CHROME").is_some() {
        chrome_signal_fixture(&marker).await;
    } else {
        shell_signal_fixture(&marker).await;
    }
}

async fn chrome_signal_fixture(marker: &str) {
    use futures::{AsyncBufReadExt, StreamExt};
    let mut command = super::Command::new(std::env::var_os("CHROME").expect("explicit Chrome"));
    let profile = format!("{marker}.profile");
    command
        .args([
            "--headless",
            "--no-sandbox",
            "--disable-gpu",
            "--remote-debugging-port=0",
            "--no-first-run",
            &format!("--user-data-dir={profile}"),
            "about:blank",
        ])
        .stderr(std::process::Stdio::piped());
    let mut child = command.spawn().expect("Chrome launch");
    let stderr = child.stderr.take().expect("stderr");
    let mut lines = futures::io::BufReader::new(stderr).lines();
    tokio::time::timeout(Duration::from_secs(20), async {
        while let Some(line) = lines.next().await {
            let line = line.expect("Chrome stderr");
            if line.contains("DevTools listening on") {
                return;
            }
        }
        panic!("Chrome never became ready");
    })
    .await
    .expect("Chrome launch deadline");
    tokio::time::sleep(Duration::from_millis(250)).await;
    let root = child.inner.id().expect("Chrome PID");
    let pids = group_members(root);
    assert!(pids.len() >= 3, "real Chrome must start helpers: {pids:?}");
    std::fs::write(marker, pids.iter().map(u32::to_string).collect::<Vec<_>>().join(" ")).expect("record");
    child.wait().await.expect("wait Chrome");
}

async fn shell_signal_fixture(marker: &str) {
    let mut command = super::Command::new("/bin/sh");
    let report = format!("{marker}.child");
    command.args(["-c", "sleep 300 & echo $$ $! > \"$1\"; wait", "fixture", &report]);
    let mut child = command.spawn().expect("launch");
    while !std::path::Path::new(&report).exists() {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    if std::env::var_os("CRAWLBERG_SIGNAL_FORK").is_some() {
        let fork = sleeping_fork_inheritor();
        std::fs::write(format!("{marker}.fork"), fork.to_string()).expect("fork report");
    }
    std::fs::copy(&report, marker).expect("ready report");
    std::fs::remove_file(report).expect("remove child report");
    child.wait().await.expect("wait");
}

fn alive(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        linux_process_state(pid).is_some_and(|(_, state)| state != 'Z')
    }
    #[cfg(not(target_os = "linux"))]
    {
        let output = std::process::Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .expect("process state");
        let status = String::from_utf8_lossy(&output.stdout);
        !status.trim().is_empty() && !status.trim().starts_with('Z')
    }
}

fn process_group(pid: u32) -> u32 {
    #[cfg(target_os = "linux")]
    {
        linux_process_state(pid).expect("live process group").0
    }
    #[cfg(not(target_os = "linux"))]
    {
        let output = std::process::Command::new("ps")
            .args(["-o", "pgid=", "-p", &pid.to_string()])
            .output()
            .expect("process group");
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .expect("group ID")
    }
}

#[cfg(target_os = "linux")]
fn linux_process_state(pid: u32) -> Option<(u32, char)> {
    parse_linux_process_state(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
}

#[cfg(target_os = "linux")]
fn parse_linux_process_state(stat: &str) -> Option<(u32, char)> {
    let (_, fields) = stat.rsplit_once(") ")?;
    let mut fields = fields.split_whitespace();
    let state = fields.next()?.chars().next()?;
    let _parent = fields.next()?;
    Some((fields.next()?.parse().ok()?, state))
}

fn parent_signal_ends_its_browser_group(signal: &str, chrome: bool) {
    let marker = std::env::temp_dir().join(format!("crawlberg-signal-{}-{signal}-{chrome}", std::process::id()));
    let mut command = std::process::Command::new(std::env::current_exe().expect("test executable"));
    if chrome {
        command.env("CRAWLBERG_SIGNAL_CHROME", "1");
    }
    let fork = signal.ends_with("-FORK");
    if fork {
        command.env("CRAWLBERG_SIGNAL_FORK", "1");
    }
    let signal = signal.strip_suffix("-FORK").unwrap_or(signal);
    let parent = FixtureParent(
        command
            .args([
                "--exact",
                "async_process::unix_tests::signal_parent_fixture",
                "--ignored",
                "--nocapture",
            ])
            .env("CRAWLBERG_SIGNAL_FIXTURE", &marker)
            .process_group(0)
            .spawn()
            .expect("fixture parent"),
    );
    let mut pids = wait_for_signal_fixture(&marker, chrome);
    let guardian = process_group(pids[0]);
    if !pids.contains(&guardian) {
        pids.push(guardian);
    }
    let ready = if chrome {
        alive(pids[0]) && alive(guardian) && pids.iter().copied().filter(|pid| alive(*pid)).count() >= 3
    } else {
        pids.iter().copied().all(alive)
    };
    assert!(ready, "browser root and its owned helpers must be live");
    finish_signal_fixture(parent, &marker, &pids, guardian, signal, fork);
}

#[test]
fn foreground_sigint_ends_owned_browser_descendants() {
    parent_signal_ends_its_browser_group("-INT", false);
}

#[test]
fn parent_sigkill_ends_owned_browser_descendants() {
    parent_signal_ends_its_browser_group("-KILL", false);
}

#[tokio::test]
async fn dropping_a_child_reaps_its_guardian_and_stops_descendants() {
    let (child, leaf) = launch_sleeping_family().await;
    let guardian = child.tree().expect("owned tree").0.id();
    let root = child.inner.id().expect("root PID");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    let auxiliaries = loop {
        let pids = group_members(root);
        if pids.len() >= 4 {
            break pids
                .into_iter()
                .filter(|pid| *pid != root && *pid != leaf)
                .collect::<Vec<_>>();
        }
        assert!(tokio::time::Instant::now() < deadline, "guardian must start its timer");
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    drop(child);
    assert_family_ended(&[root, leaf, guardian]).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while auxiliaries.iter().any(|pid| process_entry_exists(*pid)) && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        auxiliaries.iter().all(|pid| !process_entry_exists(*pid)),
        "guardian auxiliary processes must be reaped: {auxiliaries:?}"
    );
}

#[tokio::test]
async fn an_inner_child_stays_alive_until_its_retained_tree_is_dropped() {
    let (child, leaf) = launch_sleeping_family().await;
    let tree = child.tree().expect("tree").clone();
    let guardian = tree.0.id();
    let mut inner = child.into_inner();
    let root = inner.id().expect("root PID");
    assert!(alive(root) && alive(leaf), "retained tree keeps both processes alive");
    drop(tree);
    assert_family_ended(&[root, leaf, guardian]).await;
    inner.wait().await.expect("reap root");
    assert!(!process_entry_exists(guardian), "retained guardian must be reaped");
}

async fn launch_sleeping_family() -> (super::Child, u32) {
    let marker = std::env::temp_dir().join(format!(
        "crawlberg-drop-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let mut command = super::Command::new("/bin/sh");
    command.args([
        "-c",
        "sleep 300 & echo $! > \"$1\"; wait",
        "fixture",
        marker.to_str().expect("marker"),
    ]);
    let child = command.spawn().expect("launch");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        if let Ok(record) = std::fs::read_to_string(&marker) {
            if let Ok(leaf) = record.trim().parse() {
                std::fs::remove_file(marker).expect("remove marker");
                return (child, leaf);
            }
        }
        assert!(tokio::time::Instant::now() < deadline, "child reports its descendant");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn assert_family_ended(pids: &[u32]) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while pids.iter().copied().any(alive) && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        pids.iter().copied().all(|pid| !alive(pid)),
        "owned family survives: {pids:?}"
    );
}

fn process_entry_exists(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let output = std::process::Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .expect("process census");
        !String::from_utf8_lossy(&output.stdout).trim().is_empty()
    }
}

#[tokio::test]
async fn failing_to_start_a_browser_reaps_its_existing_guardian() {
    let group = super::unix_group::Group::start().expect("guardian");
    let pid = group.id();
    let result = tokio::process::Command::new("/crawlberg-does-not-exist")
        .process_group(i32::try_from(pid).expect("group ID"))
        .spawn();
    assert!(result.is_err(), "browser startup must fail");
    drop(group);
    assert!(!process_entry_exists(pid), "startup failure must reap the guardian");
}

fn group_members(root: u32) -> Vec<u32> {
    let group = process_group(root);
    #[cfg(target_os = "linux")]
    let mut members: Vec<u32> = std::fs::read_dir("/proc")
        .expect("process census")
        .filter_map(|entry| {
            let pid = entry.ok()?.file_name().to_str()?.parse().ok()?;
            let (pgid, state) = linux_process_state(pid)?;
            (pgid == group && state != 'Z').then_some(pid)
        })
        .collect();
    #[cfg(not(target_os = "linux"))]
    let mut members: Vec<u32> = {
        let output = std::process::Command::new("ps")
            .args(["-axo", "pid=,pgid=,stat="])
            .output()
            .expect("group census");
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                let pid: u32 = fields.next()?.parse().ok()?;
                let pgid: u32 = fields.next()?.parse().ok()?;
                let status = fields.next()?;
                (pgid == group && !status.starts_with('Z')).then_some(pid)
            })
            .collect()
    };
    members.retain(|pid| *pid != root);
    members.insert(0, root);
    members
}

#[cfg(target_os = "linux")]
#[test]
fn procfs_state_parser_keeps_parentheses_and_spaces_in_process_names() {
    assert_eq!(
        parse_linux_process_state("123 (name with ) spaces) S 1 456 0"),
        Some((456, 'S'))
    );
    assert_eq!(parse_linux_process_state("123 (zombie) Z 1 456 0"), Some((456, 'Z')));
    assert_eq!(parse_linux_process_state("123 (missing group) S 1"), None);
}

#[test]
fn foreground_sigint_ends_actual_chrome_and_its_helpers() {
    if std::env::var_os("CHROME").is_none() {
        return;
    }
    parent_signal_ends_its_browser_group("-INT", true);
}

#[test]
fn parent_sigkill_ends_actual_chrome_and_its_helpers() {
    if std::env::var_os("CHROME").is_none() {
        return;
    }
    parent_signal_ends_its_browser_group("-KILL", true);
}

#[tokio::test]
async fn a_child_from_another_spawner_cannot_signal_its_group() {
    let (mut child, leaf) = launch_sleeping_family().await;
    let owner = child.spawner;
    child.spawner = owner.wrapping_add(1);
    let error = child.start_kill_group().expect_err("foreign spawner cannot signal");
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(alive(leaf), "foreign refusal must not signal descendants");
    child.spawner = owner;
    drop(child);
    assert_family_ended(&[leaf]).await;
}

#[tokio::test]
async fn killing_an_inherited_child_leaves_its_owners_family_running() {
    let (mut child, leaf) = launch_sleeping_family().await;
    let root = child.id().expect("owned root");
    let owner = child.spawner;
    child.spawner = owner.wrapping_add(1);
    let start_result = child.start_kill();
    let wait_result = tokio::time::timeout(Duration::from_millis(100), child.wait()).await;
    let try_result = child.try_wait();
    let result = child.kill().await;
    let untouched = alive(root) && alive(leaf);
    child.spawner = owner;
    let owner_result = child.kill().await;
    let owner_reaped = child.id().is_none();
    drop(child);
    assert_family_ended(&[root, leaf]).await;
    assert_eq!(
        result
            .expect_err("foreign process cannot kill its owner's family")
            .kind(),
        std::io::ErrorKind::PermissionDenied
    );
    assert_eq!(
        start_result
            .expect_err("foreign start_kill must refuse before signaling")
            .kind(),
        std::io::ErrorKind::PermissionDenied
    );
    assert_eq!(
        wait_result
            .expect("foreign wait must refuse immediately")
            .expect_err("foreign wait must not reap")
            .kind(),
        std::io::ErrorKind::PermissionDenied
    );
    assert_eq!(
        try_result.expect_err("foreign try_wait must not reap").kind(),
        std::io::ErrorKind::PermissionDenied
    );
    assert!(untouched, "foreign refusal must preserve the owner's whole family");
    owner_result.expect("the original owner must still be able to kill its child");
    assert!(owner_reaped, "the original owner must collect its child's exit status");
}

#[allow(unsafe_code)]
fn sleeping_fork_inheritor() -> i32 {
    unsafe extern "C" {
        fn fork() -> i32;
        fn sleep(seconds: u32) -> u32;
        fn _exit(status: i32) -> !;
    }
    // SAFETY: the child calls only async-signal-safe libc operations until _exit, never Rust code.
    let child = unsafe { fork() };
    assert!(child >= 0, "fork must succeed");
    if child == 0 {
        unsafe {
            sleep(30);
            _exit(0);
        }
    }
    child
}

#[test]
fn parent_death_stops_browser_despite_a_surviving_fork() {
    parent_signal_ends_its_browser_group("-KILL-FORK", false);
}

#[tokio::test]
async fn a_missing_owner_probe_reports_the_failed_guardian_dependency() {
    let error = super::unix_group::Group::start_with_tools("/crawlberg-missing-ps", "/bin/sleep")
        .expect_err("missing owner probe must prevent browser startup");
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    assert!(
        error
            .to_string()
            .contains("could not establish browser guardian ownership using /crawlberg-missing-ps")
    );
}

#[tokio::test]
async fn an_owner_probe_with_an_unexpected_format_prevents_browser_startup() {
    let error = super::unix_group::Group::start_with_tools("/bin/echo", "/bin/sleep")
        .expect_err("invalid owner probe must prevent browser startup");
    assert!(
        error
            .to_string()
            .contains("guardian PPID probe did not identify this process as its owner")
    );
}

#[test]
fn individual_parent_sigint_stops_browser_despite_a_surviving_fork() {
    parent_signal_ends_its_browser_group("-INT-FORK", false);
}

fn wait_for_signal_fixture(marker: &std::path::Path, chrome: bool) -> Vec<u32> {
    let ready = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(record) = std::fs::read_to_string(marker) {
            let pids: Vec<u32> = record.split_whitespace().map(|pid| pid.parse().expect("pid")).collect();
            if (!chrome && pids.len() == 2) || (chrome && pids.len() >= 3) {
                return pids;
            }
        }
        assert!(Instant::now() < ready, "fixture must report both owned processes");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn finish_signal_fixture(
    mut parent: FixtureParent,
    marker: &std::path::Path,
    pids: &[u32],
    guardian: u32,
    signal: &str,
    fork: bool,
) {
    let recipient = i32::try_from(parent.id()).expect("fixture pid");
    let recipient = if fork { recipient } else { -recipient };
    let signal_number = match signal {
        "-INT" => SIGINT,
        "-KILL" => SIGKILL,
        other => panic!("unsupported fixture signal: {other}"),
    };
    signal_fixture(recipient, signal_number).expect("signal fixture parent");
    parent.wait().expect("parent exits");
    let deadline = Instant::now() + Duration::from_secs(5);
    while pids.iter().copied().any(alive) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let remaining: Vec<_> = pids.iter().copied().filter(|pid| alive(*pid)).collect();
    if !remaining.is_empty() {
        let _ = signal_fixture(-i32::try_from(guardian).expect("guardian pid"), SIGKILL);
    }
    if fork {
        finish_fork_inheritor(marker);
    }
    std::fs::remove_file(marker).expect("remove marker");
    assert!(
        remaining.is_empty(),
        "parent {signal} left browser processes {remaining:?}"
    );
}

fn finish_fork_inheritor(marker: &std::path::Path) {
    let report = format!("{}.fork", marker.display());
    let inheritor = std::fs::read_to_string(&report).expect("inheritor PID");
    assert!(
        alive(inheritor.parse().expect("fork PID")),
        "the fork inheritor must survive parent death"
    );
    signal_fixture(inheritor.parse().expect("fork pid"), SIGKILL).expect("end inheritor");
    std::fs::remove_file(report).expect("remove fork report");
}

#[allow(unsafe_code)]
fn signal_fixture(pid: i32, signal: i32) -> std::io::Result<()> {
    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    // SAFETY: callers pass only fixture-owned process or retained group identities.
    if unsafe { kill(pid, signal) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[tokio::test]
async fn a_missing_timer_reports_the_failed_guardian_dependency() {
    let error = super::unix_group::Group::start_with_tools("/bin/ps", "/crawlberg-missing-sleep")
        .expect_err("missing timer must prevent browser startup");
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    assert!(
        error
            .to_string()
            .contains("could not start browser guardian timer using /crawlberg-missing-sleep")
    );
}

#[tokio::test]
async fn a_failed_timer_prevents_browser_startup() {
    let error = super::unix_group::Group::start_with_tools("/bin/ps", "/usr/bin/false")
        .expect_err("failed timer must prevent browser startup");
    assert!(error.to_string().contains("guardian timer refused its interval"));
}

struct FixtureParent(std::process::Child);

impl FixtureParent {
    fn id(&self) -> u32 {
        self.0.id()
    }
    fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.0.wait()
    }
}

impl Drop for FixtureParent {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn procfs_parent_parser_accepts_only_the_ppid_field() {
    for (status, parent) in [
        ("Name:\tname with spaces\nPid:\t999\nPPid:\t123\n", Some(123)),
        ("PPid:\t0\n", Some(0)),
        ("Pid:\t123\n", None),
        ("PPid:\t123 extra\n", None),
        ("PPid:\t-1\n", None),
    ] {
        assert_eq!(super::unix_group::status_parent(status), parent);
    }
}
