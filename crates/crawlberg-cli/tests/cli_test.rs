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

/// Serve `body` as HTML to every connection on a loopback port, and return the port.
fn serve_html(body: &'static str) -> u16 {
    use std::io::{Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
    let port = listener.local_addr().expect("the listener has an address").port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request);
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    port
}

#[test]
fn download_of_an_html_page_prints_the_url_without_its_password() {
    let port = serve_html("<html><body>page</body></html>");
    let output = cargo_bin()
        .args([
            "download",
            &format!("http://user:CLI-PW-73@127.0.0.1:{port}/doc"),
            "--browser-mode",
            "never",
            "--timeout",
            "10000",
            "--config",
            r#"{"ssrf":{"deny_private":false},"respect_robots_txt":false}"#,
        ])
        .output()
        .expect("failed to run");
    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
    assert!(output.status.success(), "download must succeed: {stderr}");
    assert!(
        stdout.contains(&format!("\"url\": \"http://127.0.0.1:{port}/doc\"")),
        "the HTML branch prints the URL without its userinfo: {stdout}"
    );
    assert!(
        !stdout.contains("CLI-PW-73") && !stderr.contains("CLI-PW-73"),
        "the output must not carry the password: {stdout}{stderr}"
    );
}

#[test]
fn should_report_config_paths_and_json_types_in_both_entrypoints() {
    let cases = [
        (r#"{"max_depth":"x"}"#, "max_depth: expected a JSON number"),
        (
            r#"{"browser":{"timeout":"x"}}"#,
            "browser.timeout: expected a JSON number",
        ),
        (r#"{"browser":false}"#, "browser: expected a JSON object"),
        (
            r#"{"respect_robots_txt":12}"#,
            "respect_robots_txt: expected a JSON boolean",
        ),
        (r#"{"nope":1}"#, "nope: unknown key"),
        (r#"{"content":{"nope":1}}"#, "content.nope: unknown key"),
        (r#"{"ssrf":{"nope":1}}"#, "ssrf.nope: unknown key"),
        (r#"{"browser":{"nope":1}}"#, "browser.nope: unknown key"),
    ];
    assert_eq!(cases.len(), 8);
    let mut observed = 0;
    for command in ["scrape", "interact"] {
        for (config, expected) in cases {
            let mut invocation = cargo_bin();
            invocation.args([command, "https://example.com", "--config", config]);
            if command == "interact" {
                invocation.args(["--actions", "[]"]);
            }
            let output = invocation.output().expect("the CLI must run");
            let stderr = String::from_utf8(output.stderr).expect("diagnostic is UTF-8");
            assert_eq!(output.status.code(), Some(1), "{command}: {stderr}");
            assert!(stderr.contains(expected), "{command} {config}: {stderr}");
            assert!(!stderr.contains("expected one of"), "{stderr}");
            assert!(!stderr.contains("usize") && !stderr.contains("u64"), "{stderr}");
            observed += 1;
        }
    }
    assert_eq!(observed, 16);
}
