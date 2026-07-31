# =============================================================================
# =============================================================================
FROM rust:1.91-bookworm AS builder

WORKDIR /build

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        cmake \
        g++ \
        pkg-config \
        libssl-dev \
        && \
    apt-get clean && \
    rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./

COPY crates/crawlberg/ crates/crawlberg/
COPY crates/crawlberg-cli/ crates/crawlberg-cli/

RUN sed -i 's|^members = .*|members = ["crates/crawlberg", "crates/crawlberg-cli"]|' Cargo.toml && \
    sed -i '/^exclude/,/^\]/d' Cargo.toml

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/build/target \
    cargo build --release --package crawlberg-cli \
        --features "crawlberg-cli/api,crawlberg-cli/mcp,crawlberg-cli/otel,crawlberg/api,crawlberg/mcp,crawlberg/warc,crawlberg/ai,crawlberg/browser,crawlberg/interact" && \
    cp target/release/crawlberg /build/crawlberg && \
    strip /build/crawlberg

FROM debian:bookworm-slim

LABEL org.opencontainers.image.source="https://github.com/xberg-io/crawlberg"
LABEL org.opencontainers.image.description="Crawlberg — high-performance web crawler with browser automation, API server, and MCP"
LABEL org.opencontainers.image.licenses="MIT"

WORKDIR /app

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        chromium \
        fonts-liberation \
        fonts-noto-color-emoji \
        && \
    apt-get clean && \
    rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/crawlberg /usr/local/bin/crawlberg

ARG DISABLE_GPU=true
RUN groupadd -r crawlberg && \
    useradd -r -g crawlberg -d /app -s /sbin/nologin crawlberg && \
    chown -R crawlberg:crawlberg /app

ENV RUST_LOG=info \
    CHROME_BIN=/usr/bin/chromium \
    CRAWLBERG_DISABLE_GPU=${DISABLE_GPU} \
    CHROMIUM_FLAGS="--no-sandbox --disable-dev-shm-usage"

USER crawlberg

EXPOSE 3000

HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
    CMD ["curl", "-f", "http://localhost:3000/health"]

ENTRYPOINT ["crawlberg"]
CMD ["serve", "--host", "0.0.0.0", "--port", "3000"]
