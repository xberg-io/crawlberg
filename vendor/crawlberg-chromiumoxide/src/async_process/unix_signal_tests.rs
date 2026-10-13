use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const CHILD_MODE: &str = "CRAWLBERG_TEST_CHILD_SIGNAL_DISPOSITION";
const CHILD_LIMIT: Duration = Duration::from_secs(60);
const LAUNCH_COUNT: usize = 3;
static SIGNAL_EVENTS: AtomicUsize = AtomicUsize::new(0);

#[test]
fn should_reap_guardian_and_child_when_sigchld_is_ignored() {
    run_isolated("should_reap_guardian_and_child_when_sigchld_is_ignored", "ignored");
}

#[test]
fn should_preserve_an_existing_sigchld_handler() {
    run_isolated("should_preserve_an_existing_sigchld_handler", "custom");
}

#[test]
fn should_reap_guardian_and_child_with_default_sigchld() {
    run_isolated("should_reap_guardian_and_child_with_default_sigchld", "default");
}

fn run_isolated(test: &str, mode: &str) {
    if std::env::var(CHILD_MODE).as_deref() == Ok(mode) {
        exercise_launch(mode);
        return;
    }
    let mut child = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            &format!("async_process::unix_signal_tests::{test}"),
            "--nocapture",
        ])
        .env(CHILD_MODE, mode)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("isolated signal test");
    let deadline = Instant::now() + CHILD_LIMIT;
    while child.try_wait().expect("isolated child status").is_none() {
        if Instant::now() >= deadline {
            child.kill().expect("terminate only this test child");
            child.wait().expect("reap this test child");
            panic!("isolated signal test exceeded its deadline");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().expect("isolated test output");
    assert!(
        output.status.success(),
        "{mode}: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"));
}

fn exercise_launch(mode: &str) {
    install_disposition(mode);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("child runtime");
    let _entered = runtime.enter();
    for _ in 0..LAUNCH_COUNT {
        let mut command = super::Command::new("/bin/sleep");
        command.arg("30");
        let mut child = command.spawn().expect("guardian and child must start");
        assert!(child.try_wait().expect("live child status").is_none());
        let owner = child.spawner;
        child.spawner = owner.wrapping_add(1);
        assert_eq!(
            child
                .start_kill_group()
                .expect_err("foreign caller must not signal")
                .kind(),
            std::io::ErrorKind::PermissionDenied
        );
        assert!(child.try_wait().expect("foreign refusal leaves child live").is_none());
        child.spawner = owner;
        assert!(child.tree().expect("owned tree").stop(Duration::from_secs(2)));
        runtime
            .block_on(tokio::time::timeout(Duration::from_secs(2), child.wait()))
            .expect("child exit is bounded")
            .expect("child must be reaped");
        assert_eq!(child.inner.id(), None);
    }
    if mode == "custom" {
        assert!(
            SIGNAL_EVENTS.load(Ordering::Relaxed) > 0,
            "prior handler must remain chained"
        );
    }
}

extern "C" fn existing_handler(_: libc::c_int) {
    SIGNAL_EVENTS.fetch_add(1, Ordering::Relaxed);
}

#[allow(unsafe_code)]
fn install_disposition(mode: &str) {
    let handler = match mode {
        "ignored" => libc::SIG_IGN,
        "custom" => existing_handler as *const () as libc::sighandler_t,
        "default" => libc::SIG_DFL,
        _ => panic!("unknown disposition"),
    };
    // SAFETY: each disposition runs in its own test subprocess before starting a runtime. ~keep
    assert_ne!(unsafe { libc::signal(libc::SIGCHLD, handler) }, libc::SIG_ERR);
}
