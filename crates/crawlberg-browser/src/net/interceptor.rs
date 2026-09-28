use std::collections::HashMap;

use crate::net::client::{RequestInfo, Response};

pub enum InterceptAction {
    Continue,
    Block,
    Fulfill(Response),
    ModifyHeaders(HashMap<String, String>),
}

#[async_trait::async_trait]
pub trait RequestInterceptor {
    async fn intercept(&self, request: &RequestInfo) -> InterceptAction;
}

/// Whether `url` matches one of the interception block patterns: `*` blocks everything,
/// `*text*` matches a URL containing `text`, `*suffix` and `prefix*` match its end and start,
/// and any other pattern, including one with `*` only in the middle such as `a*b`, matches a URL
/// that contains it literally, star included.
pub(crate) fn matches_block_pattern(patterns: &[String], url: &str) -> bool {
    patterns.iter().any(|pattern| {
        if pattern == "*" {
            true
        } else if pattern.starts_with('*') && pattern.ends_with('*') {
            url.contains(&pattern[1..pattern.len() - 1])
        } else if let Some(suffix) = pattern.strip_prefix('*') {
            url.ends_with(suffix)
        } else if let Some(prefix) = pattern.strip_suffix('*') {
            url.starts_with(prefix)
        } else {
            url.contains(pattern.as_str())
        }
    })
}
