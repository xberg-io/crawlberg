//! Firecrawl v1-compatible REST API for crawlberg.
//!
//! This module provides an Axum-based HTTP server exposing crawl, scrape, map,
//! and batch operations through a JSON API that follows the Firecrawl v1 wire
//! format.
//!
//! # Endpoints
//!
//! - `POST /v1/scrape` - Scrape a single URL
//! - `POST /v1/crawl` - Start an asynchronous crawl job
//! - `GET  /v1/crawl/{id}` - Poll crawl job status
//! - `DELETE /v1/crawl/{id}` - Cancel a running crawl job
//! - `POST /v1/map` - Discover URLs on a site
//! - `POST /v1/batch/scrape` - Start an asynchronous batch scrape job
//! - `GET  /v1/batch/scrape/{id}` - Poll batch scrape job status
//! - `POST /v1/download` - Download a document from a URL
//! - `GET  /health` - Health check
//! - `GET  /version` - Version information

mod error;
mod handlers;
mod jobs;
pub mod openapi;
mod router;
mod startup;
mod state;
mod types;

pub use startup::serve_with_config;

// ~keep Re-exported (only under `mcp-http`, its sole consumer) so
// ~keep `mcp::server::start_mcp_http_server` (a second, independent HTTP listener
// ~keep that feature implies `api` for) can read the same env var names for its own
// ~keep auth/bind-safety check rather than duplicating the literals and risking the
// ~keep two auth surfaces drifting apart.
#[cfg(feature = "mcp-http")]
pub(crate) use state::{ALLOW_INSECURE_BIND_ENV, AUTH_TOKEN_ENV};
