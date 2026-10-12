# Upstream provenance

This crate is forked from `chromiumoxide` 0.9.1, downloaded from crates.io.

- Upstream repository: <https://github.com/mattsse/chromiumoxide>
- crates.io package checksum: `26ed067eb6c1f660bdb87c05efb964421d2ca262bae0296cdfe38cf0cd949a3e`
- Upstream licenses: MIT OR Apache-2.0; the unmodified license texts are included.

The fork keeps the public API compatible and adds an opt-out for chromiumoxide's
automatic management and resumption of child targets. Crawlberg uses that opt-out
so its security controller is the sole owner of paused child-target sessions.

More changes against upstream:

- Unix launches join a process group held by a dedicated `/bin/sh` guardian. It detects parent
  death by its PPID changing, independently of descriptors inherited by a surviving fork. This
  requires `/bin/sh` and `/bin/sleep`: Linux reads the live parent from procfs with shell built-ins,
  while macOS/BSD use their system `/bin/ps`. One guardian shell and timer remain per browser,
  with a parent check every second. Startup validates the PPID probe and waits for a bounded readiness
  acknowledgement before Chrome starts. Normal teardown sends TERM to the held group, stopping
  its timer immediately; the guardian reaps that helper before killing only its held group.
  Cleanup polls for at most two seconds after acquiring its ownership lock. `Child::tree` retains ownership across transfer, and
  `Child::start_kill_group` preserves refusal cleanup for non-exec launchers.
- On Windows the browser process starts suspended, joins a job object with kill-on-close and is
  then resumed, so the system ends it and its helper processes with the launching process.
  `Child::tree` hands that job to the caller as a `ProcessTree`.
  Member enumeration retries with a larger buffer instead of trusting partial results, and
  teardown reports failure when membership cannot be queried or a member cannot be inspected.
- The browser process gets the null device as its standard input. Upstream lets it inherit the
  caller's.

- The handler passes a browser-level event to its listeners on every pass. Upstream does it only
  while it tracks a target, so the event that reports a browser's last target destroyed stays
  queued until the next target exists.
- `Browser::launch_with` starts the child process through a function of the caller, so the caller
  knows the process before the wait for its web socket url.
- `BrowserConfig::command` returns the command that `launch` spawns, and
  `async_process::Command::kill_on_drop` lets a caller keep the browser process running when its
  handle is dropped. A caller that stops the whole process family itself needs the main process
  alive until it has found the helpers. The drop of its `Browser` logs no warning of a kill.
- A `Child` records the process that spawned it. In a process forked from that one, `Browser`
  hands out no child, waits for none and kills none, and its drop leaks the copied handle. Upstream
  kills the browser of the parent when the forked process drops its copy.

The fork also reads a protocol message that holds one half of a surrogate pair. Chrome
writes that half as a lone `\ud83d` escape, which `serde_json` refuses, so upstream loses
the whole reply or event. `conn::parse_message` replaces each unpaired half with U+FFFD.
