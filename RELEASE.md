# Release Notes

## Publishing (CI/CD)

### Release Order: the Swift Checksum Is Pinned Before the Tag

SwiftPM resolves `from:`/`exact:` requirements to the core `vX.Y.Z` tag, so the root `Package.swift`
at that tag must already carry the artifact bundle's checksum, and the tag never moves (#816). Every
release takes two dispatches after the version bump (`mise run version:bump:<part>`) lands on `main`:

1. **Prepare Swift release** (`prepare-swift-release.yaml`) on `main`. It calls the shared
   [Swift release preparation workflow](https://github.com/xberg-io/actions/blob/v1/prepare-swift-release/README.md),
   which builds the bundle once, pins its checksum, pushes that commit together with the `v<Cargo version>`
   tag and attaches the ZIP and its receipt to the draft release. Recover a failed run with "Re-run failed
   jobs", which reuses the same bytes; `dry-run` builds and verifies without pushing.
2. **Publish Release** with `tag=vX.Y.Z`. It verifies the tagged manifest against the attached bundle
   before any registry publishes, then creates `release/swift/<version>` at the tagged commit. It never
   rebuilds the bundle: the ZIP is not reproducible.

Publish Release rejects `republish` and `force_republish` on real runs before it checks anything
out: the tag is never deleted or re-created, and a version a registry already holds is never
re-published. Fix a broken release with a new version; a rerun of the same tag completes only what
is still missing.

### Node musl Platform Packages Are Intentionally Not Published

`@xberg-io/crawlberg-linux-x64-musl` and `@xberg-io/crawlberg-linux-arm64-musl` are **not
published, by design**. Both names are registered on npm and hold a `0.0.1` placeholder whose
description reads "Placeholder to reserve …". Neither has ever carried a real release.

This is not an OIDC or credentials problem. The `node-bindings` matrix in
`.github/workflows/publish.yaml` builds six targets — `aarch64-apple-darwin`,
`x86_64-apple-darwin`, `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`,
`x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc` — and **no musl target**. Nothing is built,
so nothing is published. The publish step skips platform directories that have no `.node`
binary rather than failing (`fd712707e`), which is why the release stays green while two of the
eight declared platform packages never ship.

**Do not "fix" this by adding a trusted publisher.** Adding one publishes nothing, because
there is no artifact. Reversing the decision means adding the two musl targets to the
`node-bindings` matrix first.

**Two things stay stale as long as this holds**, and both are deliberate rather than pending
work:

- `crates/crawlberg-node/package.json` still lists both musl targets under `napi.targets` and
  pins both as `optionalDependencies` at the current version. Those versions 404 on npm.
  npm treats an unresolvable optional dependency as a skip, so `pnpm install` on Alpine exits 0
  and installs no native binary; the failure surfaces at `require()` time. Verified with
  `pnpm add @xberg-io/crawlberg@1.3.3 --os=linux --cpu=x64 --libc=musl`, which adds one
  package (the glibc equivalent adds two).
- `crates/crawlberg-node/npm/linux-{x64,arm64}-musl/` still exist as package directories.

The user-facing statement of all of this lives in the [platform support
matrix](https://docs.crawlberg.xberg.io/getting-started/installation/#platform-support), which
is the document to update if the decision changes. Python (no `musllinux` wheels), Go, and PHP
omit musl on the same grounds.

### PHP Extension PIE Configuration

The PHP extension composer.json template (managed by Alef at `src/scaffold/languages/php.rs`) generates a PIE (PHP Installer for Extensions) binary URL.

**Asset naming convention** (Alef publishes):

```text
php_{extension_name}-{bare_version}_php{phpver}-{arch}-{os}-{libc}-{tsmode}.tgz
```

Example (release v0.3.0-rc.45, PHP 8.4, macOS arm64, NTS):

```text
php_crawlberg-0.3.0-rc.45_php8.4-arm64-darwin-bsdlibc-nts.tgz
```

**PIE url-template** (in Alef scaffold, generates into composer.json):

```json
"url-template": "{repository}/releases/download/v{Version}/php_{extension_name}-{Version}_php{PhpVersion}-{Arch}-{OS}-{Libc}-{TSMode}.tgz"
```

When Alef regenerates composer.json (e.g., crawlberg/packages/php/composer.json), PIE's template substitution:

- `{Version}` → bare version extracted from tag (e.g., `0.3.0-rc.45` from tag `v0.3.0-rc.45`)
- `{PhpVersion}` → PHP version (e.g., `8.4`)
- `{Arch}` / `{OS}` / `{Libc}` / `{TSMode}` → platform identifiers

Final resolved URL:

```text
https://github.com/xberg-io/crawlberg/releases/download/v0.3.0-rc.45/php_crawlberg-0.3.0-rc.45_php8.4-arm64-darwin-bsdlibc-nts.tgz
```

This matches Alef's published asset.
