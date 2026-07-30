// Unpublished dev tool: its benchmark tables and progress ARE its stdout/stderr
// output, so it is exempt from the workspace `print_stdout`/`print_stderr` lints
// rather than routing results through `tracing`.
#![allow(clippy::print_stdout, clippy::print_stderr)]
//! Benchmark harness library for crawlberg.
//!
//! Provides fixtures, adapters, a benchmark runner, quality metrics, and
//! statistical helpers for evaluating scraping correctness and performance.

pub mod adapter;
pub mod adapters;
pub mod cache;
pub mod config;
pub mod dataset;
pub mod error;
pub mod fixture;
pub mod monitoring;
pub mod output;
pub mod profiling;
pub mod quality;
pub mod runner;
pub mod stats;
pub mod types;
pub mod verify;

pub use config::{BenchmarkConfig, ProfilingConfig};
pub use error::{Error, Result};
pub use types::*;
