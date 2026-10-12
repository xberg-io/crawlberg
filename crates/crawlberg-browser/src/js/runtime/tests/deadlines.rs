use super::setup_runtime;
use std::io::Read;

const CHILD_FLAG: &str = "CRAWLBERG_EVENT_LOOP_DEADLINE_CHILD";

#[test]
fn should_interrupt_a_non_yielding_module_timer_and_recover() {
    run_child("module_timer", 15);
}

#[test]
fn should_bound_a_pending_timer_without_waiting_for_its_delay() {
    run_child("pending_timer", 9);
}

fn run_child(scenario: &str, seconds: u64) {
    if std::env::var(CHILD_FLAG).as_deref() == Ok(scenario) {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(check_deadline(scenario));
        return;
    }
    let test = match scenario {
        "module_timer" => "should_interrupt_a_non_yielding_module_timer_and_recover",
        "pending_timer" => "should_bound_a_pending_timer_without_waiting_for_its_delay",
        _ => panic!("unknown scenario {scenario}"),
    };
    let mut child = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            &format!("js::runtime::tests::deadlines::{test}"),
            "--nocapture",
        ])
        .env(CHILD_FLAG, scenario)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn deadline regression");
    let started = std::time::Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("poll child") {
            assert_eq!(status.code(), Some(0), "scenario {scenario} failed: {status}");
            let mut output = String::new();
            child
                .stdout
                .take()
                .expect("child output")
                .read_to_string(&mut output)
                .expect("read child output");
            assert!(
                output.contains("1 passed; 0 failed; 0 ignored"),
                "scenario {scenario} ran no test: {output}"
            );
            return;
        }
        if started.elapsed() >= std::time::Duration::from_secs(seconds) {
            child.kill().expect("stop wedged child");
            child.wait().expect("reap child");
            panic!("scenario {scenario} exceeded its external {seconds}s deadline");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

async fn check_deadline(scenario: &str) {
    let mut rt = setup_runtime("<html><body></body></html>");
    let started = std::time::Instant::now();
    let result = if scenario == "module_timer" {
        rt.load_inline_module(
            "setTimeout(() => { while (true) {} }, 0); const until = Date.now() + 30; while (Date.now() < until) {}",
            "http://example.com/watchdog",
        )
        .await
    } else {
        rt.execute_script(
            "pending timer",
            "globalThis.timerRan = false; setTimeout(() => { globalThis.timerRan = true; }, 30000);",
        )
        .expect("schedule timer");
        rt.run_event_loop().await
    };
    let error = result.expect_err("event loop deadline must return an error");
    if scenario == "module_timer" {
        assert!(error.contains("terminated"), "{error}");
        assert!(started.elapsed() < std::time::Duration::from_secs(14));
    } else {
        assert_eq!(error, "Event loop did not settle within 5s");
        assert!(started.elapsed() < std::time::Duration::from_secs(8));
        assert_eq!(
            rt.evaluate("globalThis.timerRan").expect("timer state"),
            serde_json::json!(false)
        );
    }
    assert_eq!(rt.evaluate("42").expect("isolate recovers"), serde_json::json!(42.0));
}
