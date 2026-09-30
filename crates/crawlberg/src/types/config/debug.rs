//! The redacting `Debug` view of [`CrawlConfig`].

use super::CrawlConfig;

impl std::fmt::Debug for CrawlConfig {
    /// Redacted: `custom_headers` often carries an `Authorization` or API key header, so
    /// its values print as `***`. `auth`, `proxy` and `browser` redact their own secrets.
    /// The exhaustive destructure makes a new field a compile error here, not a silent gap.
    // ~keep alef extracts public inherent AND trait-impl methods; `Formatter` has no
    // binding representation, so without this the surface fails generation with
    // lossy_sanitized_surface.
    #[cfg_attr(alef, alef(skip))]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self {
            max_depth,
            max_pages,
            max_links_per_page,
            max_concurrent,
            crawl_strategy,
            content_filter,
            bm25_query,
            bm25_threshold,
            respect_robots_txt,
            soft_http_errors,
            user_agent,
            stay_on_domain,
            allow_subdomains,
            include_paths,
            exclude_paths,
            path_patterns_match_query,
            path_patterns_match_url,
            dedup_include_query,
            strip_tracking_params,
            tracking_params,
            custom_headers,
            request_timeout,
            rate_limit_ms,
            max_redirects,
            retry_count,
            retry_codes,
            retry_initial_delay_ms,
            retry_max_delay_ms,
            rate_limit_jitter_ratio,
            cookies_enabled,
            auth,
            max_body_size,
            remove_tags,
            content,
            map_limit,
            map_search,
            download_assets,
            asset_types,
            max_asset_size,
            browser,
            proxy,
            user_agents,
            capture_screenshot,
            follow_document_urls,
            document_url_depth,
            download_documents,
            document_max_size,
            document_mime_types,
            document_output_dir,
            document_content_encoding,
            warc_output,
            browser_profile,
            save_browser_profile,
            ssrf,
            ssrf_deny_private_explicit,
            dispatch,
            credential_scope,
            #[cfg(feature = "browser")]
            browser_pool,
            proxy_provider,
            #[cfg(feature = "browser")]
            browser_session_pool,
        } = self;
        let mut debug = f.debug_struct("CrawlConfig");
        debug.field("max_depth", max_depth);
        debug.field("max_pages", max_pages);
        debug.field("max_links_per_page", max_links_per_page);
        debug.field("max_concurrent", max_concurrent);
        debug.field("crawl_strategy", crawl_strategy);
        debug.field("content_filter", content_filter);
        debug.field("bm25_query", bm25_query);
        debug.field("bm25_threshold", bm25_threshold);
        debug.field("respect_robots_txt", respect_robots_txt);
        debug.field("soft_http_errors", soft_http_errors);
        debug.field("user_agent", user_agent);
        debug.field("stay_on_domain", stay_on_domain);
        debug.field("allow_subdomains", allow_subdomains);
        debug.field("include_paths", include_paths);
        debug.field("exclude_paths", exclude_paths);
        debug.field("path_patterns_match_query", path_patterns_match_query);
        debug.field("path_patterns_match_url", path_patterns_match_url);
        debug.field("dedup_include_query", dedup_include_query);
        debug.field("strip_tracking_params", strip_tracking_params);
        debug.field("tracking_params", tracking_params);
        debug.field("custom_headers", &crate::net::redact::RedactedValues(custom_headers));
        debug.field("request_timeout", request_timeout);
        debug.field("rate_limit_ms", rate_limit_ms);
        debug.field("max_redirects", max_redirects);
        debug.field("retry_count", retry_count);
        debug.field("retry_codes", retry_codes);
        debug.field("retry_initial_delay_ms", retry_initial_delay_ms);
        debug.field("retry_max_delay_ms", retry_max_delay_ms);
        debug.field("rate_limit_jitter_ratio", rate_limit_jitter_ratio);
        debug.field("cookies_enabled", cookies_enabled);
        debug.field("auth", auth);
        debug.field("max_body_size", max_body_size);
        debug.field("remove_tags", remove_tags);
        debug.field("content", content);
        debug.field("map_limit", map_limit);
        debug.field("map_search", map_search);
        debug.field("download_assets", download_assets);
        debug.field("asset_types", asset_types);
        debug.field("max_asset_size", max_asset_size);
        debug.field("browser", browser);
        debug.field("proxy", proxy);
        debug.field("user_agents", user_agents);
        debug.field("capture_screenshot", capture_screenshot);
        debug.field("follow_document_urls", follow_document_urls);
        debug.field("document_url_depth", document_url_depth);
        debug.field("download_documents", download_documents);
        debug.field("document_max_size", document_max_size);
        debug.field("document_mime_types", document_mime_types);
        debug.field("document_output_dir", document_output_dir);
        debug.field("document_content_encoding", document_content_encoding);
        debug.field("warc_output", warc_output);
        debug.field("browser_profile", browser_profile);
        debug.field("save_browser_profile", save_browser_profile);
        debug.field("ssrf", ssrf);
        debug.field("ssrf_deny_private_explicit", ssrf_deny_private_explicit);
        debug.field("dispatch", dispatch);
        debug.field("credential_scope", credential_scope);
        #[cfg(feature = "browser")]
        debug.field("browser_pool", browser_pool);
        debug.field("proxy_provider", proxy_provider);
        #[cfg(feature = "browser")]
        debug.field("browser_session_pool", browser_session_pool);
        debug.finish()
    }
}
