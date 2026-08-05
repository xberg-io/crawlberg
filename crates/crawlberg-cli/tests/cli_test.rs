use std::process::Command;

/// ~keep Runs the binary cargo already built for this test rather than shelling out
/// to `cargo run`. A nested `cargo run` builds with this package's *default* features
/// and uplifts the result over `target/debug/crawlberg` — the very path
/// `CARGO_BIN_EXE_crawlberg` resolves to — so it raced `mcp_stdio_tasks`, which runs
/// in parallel, and left it spawning a binary with no `mcp` subcommand.
fn cargo_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_crawlberg"))
}

#[test]
fn test_cli_help() {
    let output = cargo_bin().arg("--help").output().expect("failed to run");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap().to_lowercase();
    assert!(stdout.contains("scrape"));
    assert!(stdout.contains("crawl"));
    assert!(stdout.contains("map"));
}

#[test]
fn test_cli_scrape_help() {
    let output = cargo_bin().args(["scrape", "--help"]).output().expect("failed");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.to_lowercase().contains("url"));
}

#[test]
fn test_cli_crawl_help() {
    let output = cargo_bin().args(["crawl", "--help"]).output().expect("failed");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap().to_lowercase();
    assert!(stdout.contains("depth"));
    assert!(stdout.contains("max-pages"));
}

#[test]
fn test_cli_map_help() {
    let output = cargo_bin().args(["map", "--help"]).output().expect("failed");
    assert!(output.status.success());
}
