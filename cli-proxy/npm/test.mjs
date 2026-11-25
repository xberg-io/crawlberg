import assert from "node:assert/strict";
import { test } from "node:test";

import { spawnSync } from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import {
  bundledPackageNames,
  expectedDigest,
  isMusl,
  isNonCliArtifact,
  releaseTag,
  resolveBundledBinary,
  selectArchiveName,
  verifyChecksum,
} from "./install.js";

const TRIPLE = "aarch64-apple-darwin";

test("isNonCliArtifact rejects bindings/native-lib/bottle artifacts", () => {
  const rejected = [
    "html-to-markdown-rs-ffi-v3.6.18-aarch64-apple-darwin.tar.gz",
    "liter_llm_ffi-v1.7.4-aarch64-apple-darwin.tar.gz",
    "libliter_llm_nif-v1.7.4-nif-2.17-aarch64-apple-darwin.so.tar.gz",
    "libtree_sitter_language_pack_nif-v1.9.1-nif-2.16-aarch64-apple-darwin.so.tar.gz",
    "crawlberg-0.1.0.bottle.tar.gz",
    "foo.dylib.tar.gz",
    "foo.dll.zip",
    "Foo.artifactbundle.zip",
    "crawlberg-node-darwin-arm64.tar.gz",
    "crawlberg-1.0-cp312-napi.whl",
  ];
  for (const name of rejected) {
    assert.equal(isNonCliArtifact(name), true, `should reject ${name}`);
  }
});

test("isNonCliArtifact accepts standalone CLI archives", () => {
  for (const name of [
    "crawlberg-cli-aarch64-apple-darwin.tar.gz",
    "cli-aarch64-apple-darwin.tar.gz",
    "liter-llm-1.7.4-aarch64-apple-darwin.tar.gz",
  ]) {
    assert.equal(isNonCliArtifact(name), false, `should accept ${name}`);
  }
});

test("selectArchiveName picks the cli archive over an ffi archive", () => {
  const names = [
    "html-to-markdown-rs-ffi-v3.6.18-aarch64-apple-darwin.tar.gz",
    "cli-aarch64-apple-darwin.tar.gz",
    "cli-x86_64-apple-darwin.tar.gz",
  ];
  assert.equal(selectArchiveName(names, TRIPLE), "cli-aarch64-apple-darwin.tar.gz");
});

test("selectArchiveName prefers a bin-name/cli archive among survivors", () => {
  const names = ["crawlberg-1.0.0-aarch64-apple-darwin.tar.gz", "crawlberg-cli-aarch64-apple-darwin.tar.gz"];
  assert.equal(selectArchiveName(names, TRIPLE), "crawlberg-cli-aarch64-apple-darwin.tar.gz");
});

test("selectArchiveName returns null when only non-CLI artifacts exist", () => {
  const names = ["crawlberg-ffi-v0.3.0-aarch64-apple-darwin.tar.gz", "libfoo-nif-2.17-aarch64-apple-darwin.so.tar.gz"];
  assert.equal(selectArchiveName(names, TRIPLE), null);
});

test("releaseTag pins the package's own version, never latest", () => {
  assert.equal(releaseTag("1.13.1", undefined), "v1.13.1");
  assert.equal(releaseTag("1.14.0-rc.1", ""), "v1.14.0-rc.1");
});

test("releaseTag lets an explicit override win and rejects a malformed tag", () => {
  assert.equal(releaseTag("1.13.1", "v1.12.0"), "v1.12.0");
  assert.throws(() => releaseTag("1.13.1", "latest"), /refusing to resolve release tag/);
});

test("the package.json version and mcpName are what the registry and the release expect", () => {
  const manifest = JSON.parse(fs.readFileSync(new URL("./package.json", import.meta.url), "utf8"));
  assert.match(manifest.version, /^\d+\.\d+\.\d+/);
  assert.equal(manifest.mcpName, "io.github.xberg-io/crawlberg");
});

const ARCHIVE = Buffer.from("archive bytes");
const ARCHIVE_NAME = "crawlberg-cli-aarch64-apple-darwin.tar.gz";
const DIGEST = crypto.createHash("sha256").update(ARCHIVE).digest("hex");

test("expectedDigest reads the entry for the archive, with or without the binary-mode star", () => {
  const sums = `${"a".repeat(64)}  other.tar.gz\n${DIGEST.toUpperCase()} *${ARCHIVE_NAME}\n`;
  assert.equal(expectedDigest(sums, ARCHIVE_NAME), DIGEST);
  assert.equal(expectedDigest(sums, "missing.tar.gz"), null);
});

test("verifyChecksum accepts the matching digest", () => {
  verifyChecksum(ARCHIVE, ARCHIVE_NAME, `${DIGEST}  ${ARCHIVE_NAME}\n`);
});

test("verifyChecksum rejects a tampered archive", () => {
  const sums = `${DIGEST}  ${ARCHIVE_NAME}\n`;
  assert.throws(() => verifyChecksum(Buffer.from("tampered"), ARCHIVE_NAME, sums), /checksum mismatch/);
});

test("verifyChecksum refuses an archive with no checksum entry", () => {
  assert.throws(() => verifyChecksum(ARCHIVE, ARCHIVE_NAME, `${DIGEST}  another.tar.gz\n`), /no checksum entry/);
});

const HERE = path.dirname(new URL(import.meta.url).pathname);

function fakePlatformPackage(root, name, binaryName) {
  const dir = path.join(root, "node_modules", ...name.split("/"));
  fs.mkdirSync(path.join(dir, "bin"), { recursive: true });
  fs.writeFileSync(path.join(dir, "package.json"), JSON.stringify({ name, version: "0.0.0" }));
  const binary = path.join(dir, "bin", binaryName);
  fs.writeFileSync(binary, '#!/bin/sh\necho bundled-binary "$@"\n');
  fs.chmodSync(binary, 0o755);
  return binary;
}

test("bundledPackageNames orders the host libc first on linux and is empty for unknown platforms", () => {
  const scope = "@xberg-io/crawlberg-cli";
  assert.deepEqual(bundledPackageNames("linux", "x64", false), [`${scope}-linux-x64-gnu`, `${scope}-linux-x64-musl`]);
  assert.deepEqual(bundledPackageNames("linux", "x64", true), [`${scope}-linux-x64-musl`, `${scope}-linux-x64-gnu`]);
  assert.deepEqual(bundledPackageNames("darwin", "arm64", false), [`${scope}-darwin-arm64`]);
  assert.deepEqual(bundledPackageNames("win32", "x64", false), [`${scope}-win32-x64`]);
  assert.deepEqual(bundledPackageNames("freebsd", "x64", false), []);
});

test("isMusl reads the glibc runtime from the process report", () => {
  assert.equal(isMusl({ platform: "linux", report: () => ({ header: { glibcVersionRuntime: "2.39" } }) }), false);
  assert.equal(isMusl({ platform: "linux", report: () => ({ header: {} }) }), true);
  assert.equal(isMusl({ platform: "darwin", report: () => ({ header: {} }) }), false);
});

test("resolveBundledBinary returns the platform package binary and skips a missing or empty package", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "crawlberg-bundled-"));
  try {
    const musl = fakePlatformPackage(root, "@xberg-io/crawlberg-cli-linux-x64-musl", "crawlberg");
    const resolve = (id) => {
      const file = path.join(root, "node_modules", ...id.split("/"));
      if (!fs.existsSync(file)) throw new Error(`Cannot find module ${id}`);
      return file;
    };
    assert.equal(resolveBundledBinary({ platform: "linux", arch: "x64", musl: false, resolve }), musl);
    assert.equal(resolveBundledBinary({ platform: "linux", arch: "arm64", musl: false, resolve }), null);
    assert.equal(resolveBundledBinary({ platform: "darwin", arch: "arm64", musl: false, resolve }), null);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});

test(
  "the launcher runs the bundled platform package and never reaches the download path",
  { skip: process.platform === "win32" },
  () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), "crawlberg-launcher-"));
    try {
      fs.mkdirSync(path.join(root, "bin"));
      fs.copyFileSync(path.join(HERE, "package.json"), path.join(root, "package.json"));
      fs.copyFileSync(path.join(HERE, "bin", "crawlberg.js"), path.join(root, "bin", "crawlberg.js"));
      const installSource = fs.readFileSync(path.join(HERE, "install.js"), "utf8");
      const marker = "export async function main() {";
      assert.ok(installSource.includes(marker), "install.js main() signature changed; update this test");
      fs.writeFileSync(
        path.join(root, "install.js"),
        installSource.replace(marker, `${marker}\n  throw new Error("download attempted");`),
      );
      const [pkg] = bundledPackageNames();
      assert.ok(pkg, `no bundled package is defined for ${process.platform}-${process.arch}`);
      fakePlatformPackage(root, pkg, "crawlberg");

      const result = spawnSync(process.execPath, [path.join(root, "bin", "crawlberg.js"), "--flag", "x"], {
        encoding: "utf8",
      });
      assert.equal(result.stdout.trim(), "bundled-binary --flag x");
      assert.equal(result.status, 0);
      assert.doesNotMatch(result.stderr, /download/);
    } finally {
      fs.rmSync(root, { recursive: true, force: true });
    }
  },
);

test(
  "the launcher falls through to the download path when no platform package is installed",
  { skip: process.platform === "win32" },
  () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), "crawlberg-launcher-"));
    try {
      fs.mkdirSync(path.join(root, "bin"));
      fs.copyFileSync(path.join(HERE, "package.json"), path.join(root, "package.json"));
      fs.copyFileSync(path.join(HERE, "bin", "crawlberg.js"), path.join(root, "bin", "crawlberg.js"));
      const installSource = fs.readFileSync(path.join(HERE, "install.js"), "utf8");
      const marker = "export async function main() {";
      fs.writeFileSync(
        path.join(root, "install.js"),
        installSource.replace(marker, `${marker}\n  throw new Error("download attempted");`),
      );
      const result = spawnSync(process.execPath, [path.join(root, "bin", "crawlberg.js")], { encoding: "utf8" });
      assert.equal(result.status, 1);
      assert.match(result.stderr, /attempting download/);
      assert.match(result.stderr, /download attempted/);
    } finally {
      fs.rmSync(root, { recursive: true, force: true });
    }
  },
);
