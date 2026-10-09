# Upstream provenance

This crate is forked from `chromiumoxide` 0.9.1, downloaded from crates.io.

- Upstream repository: <https://github.com/mattsse/chromiumoxide>
- crates.io package checksum: `26ed067eb6c1f660bdb87c05efb964421d2ca262bae0296cdfe38cf0cd949a3e`
- Upstream licenses: MIT OR Apache-2.0; the unmodified license texts are included.

The fork keeps the public API compatible and adds an opt-out for chromiumoxide's
automatic management and resumption of child targets. Crawlberg uses that opt-out
so its security controller is the sole owner of paused child-target sessions.

More changes against upstream:

- On Windows the browser process starts suspended, joins a job object with kill-on-close and is
  then resumed, so the system ends it and its helper processes with the launching process.
  `Child::tree` hands that job to the caller as a `ProcessTree`.
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
  alive until it has found the helpers.
