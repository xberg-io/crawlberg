# `@xberg-io/crawlberg-linux-x64-musl`

This package is **not published**. It is a reserved name holding a `0.0.1` placeholder.

Crawlberg does not ship a **x86_64-unknown-linux-musl** build of its Node binding. The
omission is deliberate: musl is not in the `node-bindings` build matrix, so no artifact is
produced for this platform, and none is planned. See the [platform support
matrix](https://docs.crawlberg.xberg.io/getting-started/installation/#platform-support).

To run Crawlberg on Alpine, use the CLI or the Docker image, or base your application on a
glibc image such as `node:22-bookworm-slim` and install
[`@xberg-io/crawlberg`](https://www.npmjs.com/package/@xberg-io/crawlberg) there.

Note that `@xberg-io/crawlberg` lists this package as an optional dependency. On Alpine, npm
skips it silently and installs no native binary, so the install appears to succeed and the
failure only surfaces when `require()` cannot find a native module.
