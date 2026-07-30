//! Model Context Protocol (MCP) server implementation.
//!
//! Provides an MCP server that exposes crawlberg's web crawling and scraping
//! capabilities as MCP tools for integration with AI assistants.
//!
//! # Implemented Tools
//!
//! - **scrape**: Scrape a single URL and extract content as markdown or JSON
//! - **crawl**: Crawl a website following links up to a configured depth
//! - **map**: Discover all pages on a website via links and sitemaps
//! - **batch_scrape**: Scrape multiple URLs concurrently
//! - **batch_crawl**: Crawl multiple seed URLs concurrently
//! - **download**: Download a document from a URL
//! - **interact**: Execute browser actions on a page
//! - **generate_citations**: Convert markdown links into numbered citations
//! - **get_version**: Get the current crawlberg library version
//!
//! # Example
//!
//! ```rust,no_run
//! use crawlberg::start_mcp_server;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
//!     start_mcp_server().await?;
//!     Ok(())
//! }
//! ```

mod errors;
pub mod format;
mod outputs;
mod params;
mod server;

#[cfg(feature = "mcp-http")]
pub use server::start_mcp_http_server;
pub use server::{CrawlbergHttpMcpService, start_mcp_server, start_mcp_server_with_config, streamable_http_service};
