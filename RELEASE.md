# Release Notes

## Publishing (CI/CD)

### Node Platform Packages OIDC Configuration

The Node platform sub-package `@xberg-io/crawlberg-linux-x64-musl` requires manual OIDC trusted-publisher setup on npm.

**Issue**: The package publishes `0.0.0-bootstrap` only because npm doesn't recognize the GitHub Actions workflow as a trusted publisher.

**Fix**: Go to <https://www.npmjs.com/package/@xberg-io/crawlberg-linux-x64-musl/access> → Settings → Trusted Publishers → **Add a new trusted publisher**:

- Provider: GitHub
- Repository: xberg-io/crawlberg
- Workflow: .github/workflows/publish.yaml
- Job: Publish Node packages

All other 7 platform packages (`linux-x64-gnu`, `linux-arm64-gnu`, `linux-arm64-musl`, `darwin-arm64`, `darwin-x64`, `win32-x64`, `win32-arm64`) publish via OIDC without issue.

This is a one-time setup; npm will cache the trusted relationship per release workflow.

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
