use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod lifecycle_chrome {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../crawlberg/tests/common/lifecycle_chrome.rs"
    ));
}

struct Site {
    url: String,
    hits: Arc<Mutex<Vec<String>>>,
    stopping: Arc<AtomicBool>,
    errors: Arc<Mutex<Vec<String>>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Site {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("fixture listener");
        listener.set_nonblocking(true).expect("nonblocking fixture");
        let url = format!("http://{}/", listener.local_addr().expect("fixture address"));
        let hits = Arc::new(Mutex::new(Vec::new()));
        let stopping = Arc::new(AtomicBool::new(false));
        let errors = Arc::new(Mutex::new(Vec::new()));
        let thread_errors = Arc::clone(&errors);
        let thread_hits = Arc::clone(&hits);
        let thread_stop = Arc::clone(&stopping);
        let thread = std::thread::spawn(move || {
            let mut connections = Vec::new();
            while !thread_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let hits = Arc::clone(&thread_hits);
                        let errors = Arc::clone(&thread_errors);
                        connections.push(std::thread::spawn(move || {
                            if let Err(error) = serve_connection(stream, &hits) {
                                errors.lock().expect("fixture errors").push(error.to_string());
                            }
                        }));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("fixture accept: {error}"),
                }
            }
            for connection in connections {
                connection.join().expect("fixture connection thread");
            }
        });
        Self {
            url,
            hits,
            stopping,
            errors,
            thread: Some(thread),
        }
    }
}

impl Drop for Site {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        let ended = self.thread.take().expect("fixture thread").join();
        if !std::thread::panicking() {
            ended.expect("fixture thread ends");
            assert_eq!(*self.errors.lock().expect("fixture errors"), Vec::<String>::new());
        }
    }
}

const FIXTURE_REQUEST_LIMIT: Duration = Duration::from_secs(5);
const FIXTURE_HEADER_LIMIT: usize = 16_384;

fn request_headers(stream: &mut std::net::TcpStream) -> std::io::Result<Option<Vec<u8>>> {
    let mut request = Vec::new();
    let mut chunk = [0_u8; 1024];
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        match stream.read(&mut chunk) {
            Ok(0) if request.is_empty() => return Ok(None),
            Ok(0) => return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof)),
            Ok(size) => request.extend_from_slice(&chunk[..size]),
            Err(error)
                if request.is_empty()
                    && matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::TimedOut
                            | std::io::ErrorKind::ConnectionReset
                            | std::io::ErrorKind::ConnectionAborted
                    ) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error),
        }
        if request.len() > FIXTURE_HEADER_LIMIT {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "fixture headers exceed limit",
            ));
        }
    }
    Ok(Some(request))
}

fn serve_connection(mut stream: std::net::TcpStream, hits: &Mutex<Vec<String>>) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(FIXTURE_REQUEST_LIMIT))?;
    stream.set_write_timeout(Some(FIXTURE_REQUEST_LIMIT))?;
    let Some(request) = request_headers(&mut stream)? else {
        return Ok(());
    };
    let path = String::from_utf8_lossy(&request)
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "missing fixture request path"))?
        .to_owned();
    hits.lock().expect("hits").push(path.clone());
    let known = matches!(path.as_str(), "/" | "/one" | "/two");
    let body = if known { fixture_body(&path) } else { String::new() };
    let status = if known { "200 OK" } else { "404 Not Found" };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .map_err(|error| std::io::Error::new(error.kind(), format!("fixture response {path}: {error}")))
}

fn fixture_body(path: &str) -> String {
    let links = if path == "/" {
        r#"<a href="/one">one</a><a href="/two">two</a>"#
    } else {
        ""
    };
    format!(
        r#"<html><body>{links}<script>const p=document.createElement("p");p.id="rendered";p.textContent="executed"+"-"+location.pathname;document.body.appendChild(p)</script></body></html>"#
    )
}

fn command(url: &str, config: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_crawlberg"));
    command.env_remove("RUST_LOG").env_remove("CRAWLBERG_LOG").args([
        "--log-level",
        "warn",
        "--log-format",
        "json",
        "crawl",
        url,
        "--browser-mode",
        "always",
        "--depth",
        "1",
        "--concurrent",
        "1",
        "--rate-limit",
        "0",
        "--format",
        "json",
        "--config",
        config,
    ]);
    command
}

fn run(command: Command) -> Output {
    run_tracking_owned_browser(command, false).0
}

fn run_tracking_owned_browser(mut command: Command, track: bool) -> (Output, Vec<u32>) {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("CLI process");
    let stdout = read_pipe(child.stdout.take().expect("stdout pipe"));
    let stderr = read_pipe(child.stderr.take().expect("stderr pipe"));
    let deadline = Instant::now() + Duration::from_secs(180);
    let mut owned = Vec::new();
    let status = loop {
        if track {
            for (pid, parent, executable) in processes() {
                if parent == child.id() && is_chrome(&executable) && !owned.contains(&pid) {
                    owned.push(pid);
                }
            }
        }
        if let Some(status) = child.try_wait().expect("CLI wait") {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().expect("kill timed out CLI");
            child.wait().expect("reap timed out CLI");
            panic!("CLI exceeded 180 seconds");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    (
        Output {
            status,
            stdout: stdout.join().expect("stdout reader"),
            stderr: stderr.join().expect("stderr reader"),
        },
        owned,
    )
}

fn read_pipe(mut pipe: impl Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        pipe.read_to_end(&mut bytes).expect("CLI output pipe");
        bytes
    })
}

fn diagnostics(stderr: &[u8]) -> Vec<serde_json::Value> {
    String::from_utf8(stderr.to_vec())
        .expect("UTF-8 diagnostics")
        .lines()
        .map(|line| serde_json::from_str(line).expect("every diagnostic must be captured JSON"))
        .collect()
}

fn assert_rendered_pages(stdout: &[u8], url: &str) {
    let result: serde_json::Value = serde_json::from_slice(stdout).expect("crawl JSON");
    assert_eq!(result["error"], serde_json::Value::Null);
    let pages = result["pages"].as_array().expect("crawl pages");
    assert_eq!(pages.len(), 3);
    let mut paths = Vec::new();
    for page in pages {
        let path = page["url"]
            .as_str()
            .expect("page URL")
            .strip_prefix(url.trim_end_matches('/'))
            .expect("fixture origin");
        paths.push(path);
        assert_eq!(page["status_code"], 200);
        assert!(
            page["html"]
                .as_str()
                .expect("HTML")
                .contains(&format!(r#"<p id="rendered">executed-{path}</p>"#)),
            "actual JavaScript must render {path}"
        );
    }
    paths.sort_unstable();
    assert_eq!(paths, ["/", "/one", "/two"]);
}

#[test]
fn should_capture_invalid_cli_configuration_as_an_error() {
    let output = run(command(
        "http://127.0.0.1:1/",
        r#"{"cli_exit_capture_probe_unknown_field":true}"#,
    ));
    assert_eq!(output.status.code(), Some(1));
    let events = diagnostics(&output.stderr);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["level"], "ERROR");
    assert_eq!(
        events[0]["fields"]["message"],
        "invalid config: cli_exit_capture_probe_unknown_field: unknown key"
    );
}

fn assert_cli_job(site: &Site, config: &str, job: usize) -> Vec<serde_json::Value> {
    let first_hit = site.hits.lock().expect("hits").len();
    let output = run(command(&site.url, config));
    assert_eq!(
        output.status.code(),
        Some(0),
        "job {job}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let url = &site.url;
    let stdout = &output.stdout;
    let rendered = std::panic::catch_unwind(|| assert_rendered_pages(stdout, url));
    assert!(
        rendered.is_ok(),
        "job {job}: stdout={} stderr={} hits={:?} fixture_errors={:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        *site.hits.lock().expect("hits"),
        *site.errors.lock().expect("fixture errors")
    );
    let hits = site.hits.lock().expect("hits");
    let mut routes: Vec<_> = hits[first_hit..]
        .iter()
        .filter(|path| matches!(path.as_str(), "/" | "/one" | "/two"))
        .cloned()
        .collect();
    routes.sort_unstable();
    assert_eq!(routes, ["/", "/one", "/two"], "job {job}");
    diagnostics(&output.stderr)
        .into_iter()
        .filter(|event| matches!(event["level"].as_str(), Some("WARN" | "ERROR")))
        .collect()
}

#[test]
fn should_finish_five_browser_cli_crawls_without_failure_diagnostics() {
    let chrome = std::env::var_os("CRAWLBERG_TEST_CHROME_PATH")
        .or_else(|| std::env::var_os("CRAWLBERG_TEST_CHROME"))
        .expect("real Chrome path must be configured");
    assert!(
        std::path::Path::new(&chrome).is_file(),
        "configured real Chrome must exist"
    );
    let config = serde_json::json!({
        "ssrf":{"deny_private":false,"denylist":[]}, "ssrf_deny_private_explicit":false,
        "respect_robots_txt":false,"retry_count":0,"browser":{"chrome_path":chrome.to_string_lossy(),
            "chrome_args":lifecycle_chrome::args(&["--disable-features=TranslateUI".to_owned()])}
    })
    .to_string();
    let site = Site::start();
    let mut failures = Vec::new();
    let mut completed = 0;
    for job in 0..5 {
        failures.extend(
            assert_cli_job(&site, &config, job)
                .into_iter()
                .map(|event| (job, event)),
        );
        completed += 1;
    }
    assert_eq!(completed, 5);
    assert_eq!(
        failures.len(),
        0,
        "normal browser CLI exit must not report failures: {failures:?}"
    );
}

fn external_devtools_endpoint(text: &str) -> Option<String> {
    let mut lines = text.lines();
    let port = lines.next()?.parse::<u16>().ok()?;
    let path = lines.next()?;
    let browser_id = path.strip_prefix("/devtools/browser/")?;
    if port == 0 || browser_id.is_empty() || path.chars().any(char::is_whitespace) || lines.next().is_some() {
        return None;
    }
    Some(format!("ws://127.0.0.1:{port}{path}"))
}

#[test]
fn should_retry_an_empty_devtools_port_record() {
    assert_eq!(external_devtools_endpoint(""), None);
}

#[test]
fn should_retry_a_port_only_devtools_record() {
    assert_eq!(external_devtools_endpoint("43123"), None);
    assert_eq!(external_devtools_endpoint("43123\n"), None);
}

#[test]
fn should_retry_an_invalid_devtools_port_record() {
    for port in ["not-a-port", "0", "65536"] {
        assert_eq!(
            external_devtools_endpoint(&format!("{port}\n/devtools/browser/browser-id")),
            None
        );
    }
}

#[test]
fn should_retry_an_incomplete_or_invalid_devtools_endpoint_path() {
    for path in [
        "",
        "/devtools/browser/",
        "not-a-path",
        "/devtools/browser/invalid path",
        "/devtools/browser/browser-id\nextra",
    ] {
        assert_eq!(external_devtools_endpoint(&format!("43123\n{path}")), None);
    }
}

#[test]
fn should_parse_a_complete_two_line_devtools_port_record() {
    let expected = Some("ws://127.0.0.1:43123/devtools/browser/browser-id".to_owned());
    assert_eq!(
        external_devtools_endpoint("43123\n/devtools/browser/browser-id"),
        expected
    );
    assert_eq!(
        external_devtools_endpoint("43123\n/devtools/browser/browser-id\n"),
        expected
    );
}

struct ExternalChrome {
    child: std::process::Child,
    profile: std::path::PathBuf,
    endpoint: String,
}

fn external_profile_parent(
    chrome: &std::path::Path,
    home: Option<&std::path::Path>,
    temporary: &std::path::Path,
) -> std::path::PathBuf {
    let Ok(relative) = chrome.strip_prefix("/snap") else {
        return temporary.to_path_buf();
    };
    let mut parts = relative.iter().filter_map(|part| part.to_str());
    let name = match parts.next() {
        Some("bin") => parts.next().and_then(|application| application.split('.').next()),
        name => name,
    };
    let Some(name) = name.filter(|name| !name.is_empty()) else {
        return temporary.to_path_buf();
    };
    // ~keep Snap's private /tmp hides DevToolsActivePort from this test; its common directory is shared.
    home.expect("snap Chrome requires HOME for its shared profile")
        .join("snap")
        .join(name)
        .join("common")
}

#[test]
fn should_put_snap_chrome_profile_in_shared_home() {
    let home = std::path::Path::new("/home/fixture-owner");
    for executable in [
        "/snap/bin/chromium",
        "/snap/bin/chromium.chromium",
        "/snap/chromium/current/usr/bin/chromium",
        "/snap/chromium/123/usr/bin/chromium",
    ] {
        assert_eq!(
            external_profile_parent(
                std::path::Path::new(executable),
                Some(home),
                std::path::Path::new("/tmp/fixture"),
            ),
            home.join("snap/chromium/common"),
            "for {executable}"
        );
    }
}

#[test]
fn should_keep_ordinary_chrome_profile_in_temp() {
    let temporary = std::path::Path::new("/tmp/fixture");
    for executable in [
        "/usr/bin/chromium",
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/snapshots/chromium",
    ] {
        assert_eq!(
            external_profile_parent(std::path::Path::new(executable), None, temporary),
            temporary,
            "for {executable}"
        );
    }
}

#[test]
#[should_panic(expected = "snap Chrome requires HOME for its shared profile")]
fn should_refuse_snap_chrome_without_home() {
    external_profile_parent(
        std::path::Path::new("/snap/bin/chromium"),
        None,
        std::path::Path::new("/tmp/fixture"),
    );
}

impl ExternalChrome {
    fn launch() -> Self {
        let chrome = std::env::var_os("CRAWLBERG_TEST_CHROME_PATH")
            .or_else(|| std::env::var_os("CRAWLBERG_TEST_CHROME"))
            .expect("real Chrome path must be configured");
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        let parent = external_profile_parent(std::path::Path::new(&chrome), home.as_deref(), &std::env::temp_dir());
        std::fs::create_dir_all(&parent).expect("external Chrome profile parent");
        let profile = parent.join(format!("crawlberg-cli-borrowed-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&profile).expect("external Chrome profile");
        let mut command = Command::new(chrome);
        // ~keep Headless cookie initialization must not wait for interactive macOS keychain access.
        #[cfg(target_os = "macos")]
        command.arg("--use-mock-keychain");
        let child = command
            .args([
                "--headless=new",
                "--no-sandbox",
                "--no-first-run",
                "--no-default-browser-check",
                "--remote-debugging-port=0",
                "--remote-allow-origins=*",
            ])
            .arg(format!("--user-data-dir={}", profile.display()))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("externally owned Chrome");
        let mut browser = Self {
            child,
            profile,
            endpoint: String::new(),
        };
        browser.wait_until_ready();
        browser
    }

    fn wait_until_ready(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            assert_eq!(
                self.child.try_wait().expect("Chrome state"),
                None,
                "owner Chrome must be alive"
            );
            if let Ok(text) = std::fs::read_to_string(self.profile.join("DevToolsActivePort"))
                && let Some(endpoint) = external_devtools_endpoint(&text)
            {
                self.endpoint = endpoint;
                return;
            }
            assert!(Instant::now() < deadline, "external Chrome must publish endpoint");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

const PROFILE_REMOVAL_LIMIT: Duration = Duration::from_secs(15);
const PROFILE_REMOVAL_POLL_INTERVAL: Duration = Duration::from_millis(10);

fn remove_external_profile(profile: &std::path::Path) -> std::io::Result<()> {
    let deadline = Instant::now() + PROFILE_REMOVAL_LIMIT;
    loop {
        match std::fs::remove_dir_all(profile) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) if Instant::now() >= deadline => return Err(error),
            Err(_) => std::thread::sleep(PROFILE_REMOVAL_POLL_INTERVAL),
        }
    }
}

impl Drop for ExternalChrome {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let removed = remove_external_profile(&self.profile);
        if !std::thread::panicking() {
            removed.expect("remove owned external profile before deadline");
        }
    }
}

#[test]
fn should_leave_borrowed_chrome_usable_after_cli_shutdown() {
    let mut browser = ExternalChrome::launch();
    let site = Site::start();
    let config = serde_json::json!({
        "ssrf":{"deny_private":false,"denylist":[]}, "ssrf_deny_private_explicit":false,
        "respect_robots_txt":false,"retry_count":0,"browser":{"endpoint":browser.endpoint}
    })
    .to_string();
    for job in 0..2 {
        let events = assert_cli_job(&site, &config, job);
        assert_eq!(events.len(), 0, "borrowed CLI job {job}: {events:?}");
        assert_eq!(browser.child.try_wait().expect("external Chrome state"), None);
    }
}

#[test]
fn should_serve_a_fixture_request_while_an_idle_preconnection_stays_open() {
    let site = Site::start();
    let address = site.url.trim_start_matches("http://").trim_end_matches('/');
    let _idle = std::net::TcpStream::connect(address).expect("idle Chrome-like preconnection");
    let mut request = std::net::TcpStream::connect(address).expect("fixture request connection");
    request
        .set_read_timeout(Some(Duration::from_secs(1)))
        .expect("response bound");
    request
        .write_all(b"GET /one HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .expect("complete request");
    let mut response = String::new();
    request
        .read_to_string(&mut response)
        .expect("an idle connection must not prevent the fixture response");
    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(response.ends_with(&fixture_body("/one")));
    assert_eq!(*site.hits.lock().expect("hits"), ["/one"]);
}

#[test]
fn should_keep_an_accepted_connection_open_until_the_complete_request_arrives() {
    let site = Site::start();
    let address = site.url.trim_start_matches("http://").trim_end_matches('/');
    let mut request = std::net::TcpStream::connect(address).expect("new Chrome-like connection");
    request
        .set_read_timeout(Some(Duration::from_millis(100)))
        .expect("idle observation bound");
    let mut byte = [0_u8; 1];
    let idle = request.read(&mut byte);
    assert!(
        matches!(idle, Err(ref error) if matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut)),
        "the fixture must await the request rather than close an accepted connection: {idle:?}"
    );
    request
        .set_read_timeout(Some(Duration::from_secs(1)))
        .expect("response bound");
    request
        .write_all(b"GET /one HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .expect("complete request");
    let mut response = String::new();
    request.read_to_string(&mut response).expect("complete response");
    assert!(response.ends_with(&fixture_body("/one")));
    assert_eq!(*site.hits.lock().expect("hits"), ["/one"]);
}

fn is_chrome(executable: &str) -> bool {
    let name = executable.to_ascii_lowercase();
    name.contains("chrome") || name.contains("chromium")
}

fn processes() -> Vec<(u32, u32, String)> {
    let output = Command::new("ps")
        .args(["-axo", "pid=,ppid=,comm="])
        .output()
        .expect("process inventory");
    assert_eq!(output.status.code(), Some(0));
    String::from_utf8(output.stdout)
        .expect("process inventory UTF-8")
        .lines()
        .map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next().expect("process id").parse().expect("numeric process id");
            let parent = fields.next().expect("parent id").parse().expect("numeric parent id");
            (pid, parent, fields.collect::<Vec<_>>().join(" "))
        })
        .collect()
}

#[cfg(unix)]
fn non_crawl_command(operation: &str, url: &str, config: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_crawlberg"));
    command.env_remove("RUST_LOG").env_remove("CRAWLBERG_LOG").args([
        "--log-level",
        "warn",
        "--log-format",
        "json",
        operation,
        url,
        "--browser-mode",
        "always",
        "--config",
        config,
    ]);
    if operation == "interact" {
        command.args(["--actions", "[]"]);
    }
    command
}

#[cfg(unix)]
fn assert_non_crawl_result(operation: &str, stdout: &[u8]) {
    let result: serde_json::Value = serde_json::from_slice(stdout).expect("command JSON");
    let html = match operation {
        "scrape" => {
            assert_eq!(result["status_code"], 200);
            &result["html"]
        }
        "interact" => &result["interaction"]["final_html"],
        "batch-scrape" | "batch-crawl" => {
            assert_eq!(result["completed_count"], 1);
            assert_eq!(result["failed_count"], 0);
            assert_eq!(result["results"].as_array().expect("batch results").len(), 1);
            let entry = &result["results"][0]["result"];
            if operation == "batch-crawl" {
                &entry["pages"][0]["html"]
            } else {
                &entry["html"]
            }
        }
        "download" => {
            assert_eq!(result["status_code"], 200);
            assert_eq!(result["content_type"], "text/html");
            assert_eq!(result["note"], "URL returned HTML content, not a downloadable document");
            return;
        }
        _ => unreachable!(),
    };
    assert!(
        html.as_str()
            .expect("rendered HTML")
            .contains(r#"<p id="rendered">executed-/</p>"#),
        "{operation} must return the executed JavaScript result"
    );
}

#[cfg(unix)]
fn assert_no_owned_root_or_diagnostics(operation: &str, owned: &[u32], stderr: &[u8]) {
    assert_eq!(owned.len(), 1, "{operation} must actually launch one owned Chrome");
    let remaining: Vec<_> = processes()
        .into_iter()
        .filter(|(pid, _, _)| owned.contains(pid))
        .collect();
    assert_eq!(
        remaining,
        Vec::<(u32, u32, String)>::new(),
        "{operation} returned with owned Chrome alive"
    );
    assert_eq!(
        diagnostics(stderr),
        Vec::<serde_json::Value>::new(),
        "{operation} diagnostics"
    );
}

#[cfg(unix)]
#[test]
fn should_finish_non_crawl_browser_commands_without_diagnostics_or_owned_chrome_roots() {
    let chrome = std::env::var_os("CRAWLBERG_TEST_CHROME_PATH")
        .or_else(|| std::env::var_os("CRAWLBERG_TEST_CHROME"))
        .expect("real Chrome path must be configured");
    assert!(std::path::Path::new(&chrome).is_file());
    let config = serde_json::json!({
        "ssrf":{"deny_private":false,"denylist":[]}, "ssrf_deny_private_explicit":false,
        "respect_robots_txt":false,"retry_count":0,"browser":{"chrome_path":chrome.to_string_lossy(),
            "chrome_args":lifecycle_chrome::args(&["--disable-features=TranslateUI".to_owned()])},
        "max_depth":0,"max_concurrent":1,"rate_limit_ms":0
    })
    .to_string();
    let mut completed = Vec::new();
    for operation in ["scrape", "interact", "batch-scrape", "batch-crawl", "download"] {
        let site = Site::start();
        let (output, owned) = run_tracking_owned_browser(non_crawl_command(operation, &site.url, &config), true);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{operation}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_non_crawl_result(operation, &output.stdout);
        assert!(site.hits.lock().expect("fixture hits").iter().any(|path| path == "/"));
        assert_no_owned_root_or_diagnostics(operation, &owned, &output.stderr);
        completed.push(operation);
    }
    assert_eq!(
        completed,
        ["scrape", "interact", "batch-scrape", "batch-crawl", "download"]
    );
}
