#![cfg(feature = "browser")]

mod common;

#[test]
fn a_detected_binary_that_fails_to_launch_is_not_missing_chrome() {
    for message in [
        "browser: failed to launch Chrome: Browser process exited with status 1",
        "browser: failed to launch browser: unexpected end of stream",
    ] {
        assert!(
            !common::is_missing_chrome_message(message),
            "a broken launch must fail the Chrome-backed test: {message}"
        );
    }
}

#[test]
fn a_failed_executable_detection_is_missing_chrome() {
    assert!(common::is_missing_chrome_message(
        "invalid browser config: Could not auto detect a chrome executable"
    ));
}
