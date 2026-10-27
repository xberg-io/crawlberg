# Crawlberg Docker Images

This directory contains Dockerfiles for building crawlberg container images.

## Primary Image: Dockerfile.alpine

**The main, production-ready image** for crawlberg. Optimized for CLI usage and distribution.

### Features

- **Base**: Alpine Linux 3.24 (minimal)
- **Architecture**: Multi-arch (amd64, arm64)
- **Binary**: Statically compiled with musl
- **Size**: ~30-50MB (highly portable)
- **User**: Non-root (`crawlberg` user)
- **Features**: API server (`serve`), MCP mode (`mcp`), CLI commands

### Build

```bash
# Local development build
docker build -f docker/Dockerfile.alpine -t crawlberg:latest .

# Test the image
docker run --rm crawlberg:latest --version
docker run --rm crawlberg:latest --help

# Run a scrape command
docker run --rm crawlberg:latest scrape https://example.com
```

### Usage

```bash
# CLI: scrape a URL
docker run --rm crawlberg:latest scrape https://example.com

# CLI: extract from a local file
docker run -v /data:/data:ro --rm crawlberg:latest extract /data/document.pdf

# API server mode
docker run -p 8000:3000 --rm crawlberg:latest serve

# MCP server mode
docker run --rm crawlberg:latest mcp
```

### Pull from Registry

```bash
docker pull ghcr.io/xberg-io/crawlberg:latest
docker pull ghcr.io/xberg-io/crawlberg:v0.1.0
```

## Legacy Images

### Dockerfile (Full variant)

Full-featured image with Chromium for browser automation, Tesseract OCR, and optional PaddleOCR.

- **Size**: ~500MB+
- **Use case**: Complex document processing with OCR/browser features
- **Status**: Maintained for compatibility

### Dockerfile.cli

Minimal CLI image on Debian slim.

- **Base**: Debian trixie-slim
- **Size**: ~100-150MB
- **Status**: Superseded by Dockerfile.alpine (better size/portability)

### Dockerfile.musl-build

Alpine builder for extracting the CLI binary to host filesystem.

- **Output**: `dist/crawlberg` (static binary)
- **Use case**: Standalone CLI distribution without Docker

### Dockerfile.musl-ffi, Dockerfile.musl-nif

Specialized builders for language bindings (FFI, Ruby NIF).

- **Output**: Shared libraries for Python/Ruby/Node.js bindings
- **Status**: Used in binding build workflows only

## CI/CD Integration

### Publish Workflow

`.github/workflows/publish-docker.yaml`

- **Trigger**: `workflow_dispatch`, `release` (published), `repository_dispatch`
- **Variant**: Alpine CLI (single image)
- **Architecture**: Builds amd64 + arm64 with Buildx
- **Registry**: `ghcr.io/xberg-io/crawlberg`
- **Tags**: `{version}`, `latest`
- **Testing**: Runs full test suite before push

### Testing

```bash
# Local test of image
scripts/ci/docker/build_and_test.sh

# Test pre-built image
scripts/ci/docker/build_and_test.sh --image crawlberg:v0.1.0

# Verbose output
scripts/ci/docker/build_and_test.sh --verbose
```

### Test Suite

`scripts/ci/docker/test_docker.py`

Tests for Alpine CLI variant:

- Image existence
- Binary version/help output
- Feature detection (MIME types, extraction)
- Text/PDF/HTML/DOCX extraction
- Batch operations
- Read-only volume mounting
- Security (non-root user, memory limits)
- Error handling

Run full test suite:

```bash
python3 scripts/ci/docker/test_docker.py --image crawlberg:latest --variant cli
```

## Build Performance

### Multi-stage Build Optimization

- **Builder**: Alpine + Rust toolchain, optimized for size
- **Runtime**: Alpine only with CA certs + curl
- **Cache**: Leverages Docker layer caching and GitHub Actions cache

### Build Time

- **First build**: ~10-15 minutes (downloading Rust, building dependencies)
- **Incremental**: ~2-3 minutes (cached layers)

### Image Size Breakdown (Alpine variant)

- Alpine base: ~5MB
- CA certificates: ~500KB
- curl: ~3MB
- crawlberg binary (stripped): ~20-30MB
- **Total**: ~30-40MB

## Security

### Image Security Practices

- ✅ Non-root user (`crawlberg:crawlberg`)
- ✅ Read-only filesystem support (with `/tmp` exemption)
- ✅ Minimal base (Alpine, no extra utilities)
- ✅ CA certificates for HTTPS
- ✅ No hardcoded credentials
- ✅ Binary stripped of debug symbols

### Runtime Best Practices

```bash
# Read-only root filesystem + tmpfs for temporary files
docker run --rm \
  --read-only \
  --tmpfs /tmp \
  --user crawlberg:crawlberg \
  -v /data:/data:ro \
  crawlberg:latest \
  extract /data/document.pdf

# Memory limits
docker run --rm \
  --memory 512m \
  --memory-swap 512m \
  crawlberg:latest \
  scrape https://example.com
```

## Troubleshooting

### Image too large?

- Use `Dockerfile.alpine` (not legacy variants)
- Ensure binary is stripped: `strip target/release/crawlberg`

### Build fails on ARM64?

- Ensure `--platform` flag is set if cross-compiling
- Check Rust target installed: `rustc --print target-list`

### Binary crashes at startup?

- Verify static linking: `ldd crawlberg` (should show `not a dynamic executable`)
- Check feature flags: `crawlberg --version`

### Test failures in CI?

- Check available disk space: `df -h`
- Review test logs: `scripts/ci/docker/test_docker.py --verbose`
- Verify registry credentials in GitHub secrets

## Development Workflow

### Local Build & Test

```bash
# Build image
docker build -f docker/Dockerfile.alpine -t crawlberg:dev .

# Quick smoke test
docker run --rm crawlberg:dev --version

# Full test suite
python3 scripts/ci/docker/test_docker.py --image crawlberg:dev --variant cli --verbose
```

### Publish Workflow (after release)

```bash
# Option 1: Via GitHub Actions UI
# - Go to .github/workflows/publish-docker.yaml
# - Click "Run workflow"
# - Enter tag (e.g., v0.1.0)
# - Confirm

# Option 2: Via GitHub CLI
gh workflow run publish-docker.yaml \
  -f tag=v0.1.0 \
  -f force_republish=false

# Option 3: Via repository_dispatch event
gh api repos/xberg-io/crawlberg/dispatches \
  -f event_type=publish-docker \
  -f client_payload='{"tag":"v0.1.0"}'
```

## References

- [Dockerfile best practices](https://docs.docker.com/build/building/best-practices/)
- [Alpine Linux security](https://alpinelinux.org/)
- [Multi-stage Docker builds](https://docs.docker.com/build/building/multi-stage/)
- [Docker buildx multi-platform builds](https://docs.docker.com/build/building/multi-platform/)
