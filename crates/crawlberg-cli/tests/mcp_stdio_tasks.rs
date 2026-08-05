//! End-to-end test of the SEP-2663 Tasks extension over the stdio MCP
//! transport — the transport the plugin actually spawns.
//!
//! Unlike the stateless HTTP transport (which cannot propagate per-request
//! client capabilities), stdio negotiates capabilities during `initialize`, so
//! a task-augmented `tools/call` yields a task handle the client polls to
//! completion via `tasks/get`.

#![cfg(feature = "mcp")]

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const READ_TIMEOUT: Duration = Duration::from_secs(15);

struct Server {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    stderr: Arc<Mutex<Vec<String>>>,
}

impl Server {
    fn spawn() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_crawlberg"))
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn `crawlberg mcp`");

        let stdin = child.stdin.take().expect("child stdin");
        let stdout = child.stdout.take().expect("child stdout");
        let stderr = child.stderr.take().expect("child stderr");

        let (sender, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });

        // Captured (rather than discarded) so a hang or crash can be diagnosed
        // from the child's actual stderr instead of an opaque `Disconnected`.
        let stderr_lines = Arc::new(Mutex::new(Vec::new()));
        let stderr_writer = Arc::clone(&stderr_lines);
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                stderr_writer
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(line);
            }
        });

        Self {
            child,
            stdin,
            lines,
            stderr: stderr_lines,
        }
    }

    fn send(&mut self, message: serde_json::Value) {
        writeln!(self.stdin, "{message}").expect("write request");
        self.stdin.flush().expect("flush request");
    }

    fn stderr_snapshot(&self) -> String {
        self.stderr
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .join("\n")
    }

    /// Read framed JSON-RPC lines until one carries the given response id.
    fn read_response(&self, id: i64) -> serde_json::Value {
        loop {
            let line = self.lines.recv_timeout(READ_TIMEOUT).unwrap_or_else(|err| {
                panic!(
                    "server response within timeout: {err}\n--- child stderr ---\n{}",
                    self.stderr_snapshot()
                )
            });
            if line.trim().is_empty() {
                continue;
            }
            let value: serde_json::Value = serde_json::from_str(&line).expect("server emits JSON lines");
            if value.get("id").and_then(serde_json::Value::as_i64) == Some(id) {
                return value;
            }
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn stdio_mcp_runs_tool_as_task_and_polls_to_completion() {
    let mut server = Server::spawn();

    // Negotiate capabilities, declaring tasks support.
    server.send(serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2026-07-28",
            "capabilities": { "extensions": { "io.modelcontextprotocol/tasks": {} } },
            "clientInfo": { "name": "stdio-task-test", "version": "0" },
        },
    }));
    let init = server.read_response(1);
    let capabilities = &init["result"]["capabilities"];
    assert!(
        capabilities.get("tools").is_some(),
        "server must advertise tools: {init}"
    );
    assert!(
        capabilities["extensions"]
            .get("io.modelcontextprotocol/tasks")
            .is_some(),
        "server must advertise the tasks extension: {init}"
    );

    server.send(serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));

    // A task-augmented tools/call returns a task handle, not an inline result.
    server.send(serde_json::json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {
            "name": "get_version",
            "arguments": {},
            "_meta": { "io.modelcontextprotocol/tasks": { "ttl": 60000 } },
        },
    }));
    let created = server.read_response(2);
    assert_eq!(
        created["result"]["resultType"], "task",
        "task-augmented call must return a task handle: {created}"
    );
    let task_id = created["result"]["taskId"]
        .as_str()
        .unwrap_or_else(|| panic!("task handle must carry a taskId: {created}"))
        .to_string();

    // Poll tasks/get until the task settles.
    let mut terminal = None;
    for _ in 0..100 {
        server.send(serde_json::json!({
            "jsonrpc": "2.0", "id": 3, "method": "tasks/get",
            "params": { "taskId": task_id },
        }));
        let task = server.read_response(3)["result"].clone();
        match task["status"].as_str() {
            Some("completed" | "failed" | "cancelled") => {
                terminal = Some(task);
                break;
            }
            _ => std::thread::sleep(Duration::from_millis(20)),
        }
    }

    let task = terminal.expect("task must reach a terminal status");
    assert_eq!(
        task["status"], "completed",
        "get_version must complete successfully: {task}"
    );
    let text = task["result"]["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        text.contains("version"),
        "completed task must carry the get_version result: {task}"
    );
}
