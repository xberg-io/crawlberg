use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const CHILD_MODE: &str = "CRAWLBERG_TEST_CHILD_SIGNAL_DISPOSITION";
const CHILD_LIMIT: Duration = Duration::from_secs(60);
const LAUNCH_COUNT: usize = 3;
static SIGNAL_EVENTS: AtomicUsize = AtomicUsize::new(0);
static STACK_START: AtomicUsize = AtomicUsize::new(0);
static STACK_END: AtomicUsize = AtomicUsize::new(0);
static STACK_EVENTS: AtomicUsize = AtomicUsize::new(0);
const ALT_STACK_BYTES: usize = 64 * 1024;

#[test]
fn should_reap_guardian_and_child_when_sigchld_is_ignored() {
    run_isolated("should_reap_guardian_and_child_when_sigchld_is_ignored", "ignored");
}

#[test]
fn should_preserve_an_existing_sigchld_handler() {
    run_isolated("should_preserve_an_existing_sigchld_handler", "custom");
}

#[test]
#[cfg(target_os = "linux")]
fn should_preserve_sigchld_altstack_handler_and_mask() {
    run_isolated("should_preserve_sigchld_altstack_handler_and_mask", "custom_altstack");
}

#[test]
fn should_reap_guardian_and_child_with_no_child_wait() {
    run_isolated("should_reap_guardian_and_child_with_no_child_wait", "no_child_wait");
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
    let _stack = install_disposition(mode);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("child runtime");
    let _entered = runtime.enter();
    for _ in 0..LAUNCH_COUNT {
        let mut command = super::Command::new("/bin/sleep");
        command.arg("30");
        let mut child = command.spawn().expect("guardian and child must start");
        assert_custom_disposition(mode);
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
    if mode.starts_with("custom") {
        assert!(
            SIGNAL_EVENTS.load(Ordering::Relaxed) > 0,
            "prior handler must remain chained"
        );
    }
    if mode == "custom_altstack" {
        assert_eq!(
            STACK_EVENTS.load(Ordering::Relaxed),
            SIGNAL_EVENTS.load(Ordering::Relaxed)
        );
    }
}

extern "C" fn existing_handler(_: libc::c_int) {
    let stack_byte = 0_u8;
    let address = std::ptr::addr_of!(stack_byte) as usize;
    if (STACK_START.load(Ordering::Relaxed)..STACK_END.load(Ordering::Relaxed)).contains(&address) {
        STACK_EVENTS.fetch_add(1, Ordering::Relaxed);
    }
    SIGNAL_EVENTS.fetch_add(1, Ordering::Relaxed);
}

#[allow(unsafe_code)]
fn install_disposition(mode: &str) -> Vec<u8> {
    if mode == "custom_altstack" {
        return install_altstack_disposition();
    }
    if mode == "no_child_wait" {
        // SAFETY: the isolated test initializes and installs only its own automatic-reaping action. ~keep
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = libc::SIG_DFL;
        action.sa_flags = libc::SA_NOCLDWAIT;
        assert_eq!(
            unsafe { libc::sigaction(libc::SIGCHLD, &action, std::ptr::null_mut()) },
            0
        );
        return Vec::new();
    }
    let handler = match mode {
        "ignored" => libc::SIG_IGN,
        "custom" => existing_handler as *const () as libc::sighandler_t,
        "default" => libc::SIG_DFL,
        _ => panic!("unknown disposition"),
    };
    // SAFETY: each disposition runs in its own test subprocess before starting a runtime. ~keep
    assert_ne!(unsafe { libc::signal(libc::SIGCHLD, handler) }, libc::SIG_ERR);
    Vec::new()
}

#[allow(unsafe_code)]
fn install_altstack_disposition() -> Vec<u8> {
    let mut bytes = vec![0_u8; ALT_STACK_BYTES];
    STACK_START.store(bytes.as_ptr() as usize, Ordering::Relaxed);
    STACK_END.store(bytes.as_ptr() as usize + bytes.len(), Ordering::Relaxed);
    let stack = libc::stack_t {
        ss_sp: bytes.as_mut_ptr().cast(),
        ss_flags: 0,
        ss_size: bytes.len(),
    };
    // SAFETY: the isolated test holds the backing allocation until its handler is no longer used. ~keep
    assert_eq!(unsafe { libc::sigaltstack(&stack, std::ptr::null_mut()) }, 0);
    // SAFETY: zero initializes the platform action before setting its handler, flags and mask. ~keep
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = existing_handler as *const () as libc::sighandler_t;
    action.sa_flags = libc::SA_ONSTACK | libc::SA_RESTART;
    // SAFETY: the mask and action are live, and only this subprocess's disposition is changed. ~keep
    unsafe {
        assert_eq!(libc::sigemptyset(&mut action.sa_mask), 0);
        assert_eq!(libc::sigaddset(&mut action.sa_mask, libc::SIGUSR1), 0);
        assert_eq!(libc::sigaction(libc::SIGCHLD, &action, std::ptr::null_mut()), 0);
    }
    bytes
}

#[allow(unsafe_code)]
fn assert_custom_disposition(mode: &str) {
    if mode != "custom_altstack" {
        return;
    }
    // SAFETY: the platform fills a writable action without installing a new disposition. ~keep
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), &mut action) },
        0
    );
    assert_eq!(
        action.sa_flags & libc::SA_ONSTACK,
        libc::SA_ONSTACK,
        "host alternate-stack delivery must survive"
    );
    assert_eq!(action.sa_flags & libc::SA_RESTART, libc::SA_RESTART);
    assert_eq!(
        action.sa_flags & libc::SA_NOCLDWAIT,
        0,
        "guardian children must remain waitable"
    );
    assert_eq!(
        action.sa_sigaction, existing_handler as *const () as libc::sighandler_t,
        "host handler must stay installed"
    );
    assert_eq!(
        unsafe { libc::sigismember(&action.sa_mask, libc::SIGUSR1) },
        1,
        "host signal mask must survive"
    );
}
