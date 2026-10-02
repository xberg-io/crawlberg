//! `BrowserProfile::chrome_args` keeps the path and signature that v1.8.0 callers used.
#![cfg(feature = "browser")]
#![allow(deprecated)]

use std::path::PathBuf;

use crawlberg::browser_profile::BrowserProfile;

#[test]
fn chrome_args_keeps_its_v1_8_0_signature_and_flag() {
    let chrome_args: fn(&BrowserProfile) -> Vec<String> = BrowserProfile::chrome_args;
    let profile = BrowserProfile {
        name: "work".into(),
        user_data_dir: PathBuf::from("/data/crawlberg/profiles/work"),
    };
    assert_eq!(chrome_args(&profile), ["--user-data-dir=/data/crawlberg/profiles/work"]);
    assert_eq!(
        crawlberg::BrowserProfile::chrome_args(&profile),
        chrome_args(&profile),
        "the root re-export reaches the same method"
    );
}
