//! Native browser backend for Crawlberg.
//!
//! Portions of this crate are derived from Obscura (Apache-2.0).
//! See NOTICE for attribution.

#[macro_use]
extern crate html5ever;

pub mod adapter;
pub mod redact;

#[allow(dead_code)]
pub(crate) mod context;
#[allow(dead_code)]
pub(crate) mod dom;
#[allow(dead_code)]
pub(crate) mod js;
#[allow(dead_code)]
pub(crate) mod lifecycle;
#[allow(dead_code)]
pub(crate) mod net;
#[allow(dead_code)]
pub(crate) mod page;
