import assert from "node:assert/strict";
import { test } from "node:test";

import crypto from "node:crypto";
import fs from "node:fs";

import { expectedDigest, isNonCliArtifact, releaseTag, selectArchiveName, verifyChecksum } from "./install.js";

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
