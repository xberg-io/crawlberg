//! The main research agent that orchestrates the plan-crawl-synthesize loop.
#![allow(dead_code)]

use super::planner::ResearchPlanner;
use super::synthesizer::ResearchSynthesizer;
use super::types::*;
use crate::engine::CrawlEngine;
use crate::error::CrawlError;
use crate::types::CrawlConfig;

/// An autonomous research agent that crawls the web and synthesizes findings.
///
/// The agent follows a loop: plan the next step (via [`ResearchPlanner`]),
/// execute it (crawl or synthesize), and repeat until done.
pub struct ResearchAgent {
    config: ResearchConfig,
    crawl_config: CrawlConfig,
}

/// Compute a basic keyword-match relevance score in `[0.0, 1.0]`.
///
/// Counts how many whitespace-delimited words from `query` appear (case-insensitive)
/// anywhere in `content`, then divides by the total query word count.
fn simple_relevance_score(query: &str, content: &str) -> f64 {
    let query_words: Vec<&str> = query.split_whitespace().collect();
    if query_words.is_empty() {
        return 0.0;
    }
    let content_lower = content.to_lowercase();
    let matches = query_words
        .iter()
        .filter(|w| content_lower.contains(&w.to_lowercase()))
        .count();
    matches as f64 / query_words.len() as f64
}

/// Longest snippet, in characters, recorded per source in [`SourceInfo::snippet`].
const SOURCE_SNIPPET_CHARS: usize = 200;

/// Longest excerpt, in characters, kept per page in [`Finding::content`].
const FINDING_CONTENT_CHARS: usize = 500;

/// What one crawl step contributed: the pages it visited and how many of them
/// yielded a finding.
struct AbsorbedPages {
    urls_visited: Vec<String>,
    findings_count: usize,
}

impl ResearchAgent {
    /// Record every crawled page as a source, and every page with non-empty
    /// markdown as a scored finding.
    fn absorb_crawl_pages(
        &self,
        pages: &[crate::types::CrawlPageResult],
        findings: &mut Vec<Finding>,
        sources: &mut Vec<SourceInfo>,
    ) -> AbsorbedPages {
        let mut urls_visited = Vec::new();
        let mut findings_count = 0;

        for page in pages {
            urls_visited.push(page.url.clone());

            sources.push(SourceInfo {
                url: page.url.clone(),
                title: page.metadata.title.clone(),
                snippet: page
                    .markdown
                    .as_ref()
                    .map(|m| m.content.chars().take(SOURCE_SNIPPET_CHARS).collect()),
            });

            let Some(ref markdown) = page.markdown else {
                continue;
            };
            if markdown.content.is_empty() {
                continue;
            }

            findings.push(Finding {
                content: markdown.content.chars().take(FINDING_CONTENT_CHARS).collect(),
                source_url: page.url.clone(),
                relevance_score: simple_relevance_score(&self.config.query, &markdown.content),
            });
            findings_count += 1;
        }

        AbsorbedPages {
            urls_visited,
            findings_count,
        }
    }

    /// Create a new agent with the given research configuration.
    pub fn new(config: ResearchConfig) -> Self {
        Self {
            config,
            crawl_config: CrawlConfig::default(),
        }
    }

    /// Override the default crawl configuration used for each crawl step.
    pub fn with_crawl_config(mut self, config: CrawlConfig) -> Self {
        self.crawl_config = config;
        self
    }

    /// Execute the research loop and return the final report.
    pub async fn research(&self) -> Result<ResearchResult, CrawlError> {
        let planner = ResearchPlanner::new();
        let synthesizer = ResearchSynthesizer::new();

        let mut findings: Vec<Finding> = Vec::new();
        let mut sources: Vec<SourceInfo> = Vec::new();
        let mut steps: Vec<ResearchStep> = Vec::new();
        let mut pages_crawled: usize = 0;

        // ~keep One engine for the whole session, not one for each step: the steps are one client,
        // ~keep and the engine holds what a client keeps between requests (its cookies, its
        // ~keep per-host throttle, the robots rules it fetched). A step that signs in and a later
        // ~keep step that reads must be the same engine.
        let mut session_config = self.crawl_config.clone();
        session_config.max_pages = Some(self.config.max_pages_per_step);
        let engine = CrawlEngine::builder().config(session_config).build()?;

        for step_num in 0..self.config.max_steps {
            let action = planner
                .plan_next_step(
                    &self.config.query,
                    &self.config.seed_urls,
                    &findings,
                    step_num,
                    self.config.max_steps,
                )
                .await?;

            match &action {
                StepAction::Crawl { url, depth } => {
                    // ~keep The depth is the one setting the planner picks for each step. A clone is
                    // ~keep the same engine (every part it holds is shared), with this step's depth.
                    let mut step_engine = engine.clone();
                    step_engine.config.max_depth = Some(*depth);

                    let crawl_result = match step_engine.crawl(url).await {
                        Ok(crawl_result) => crawl_result,
                        Err(e) => {
                            steps.push(ResearchStep {
                                step_number: step_num,
                                action: action.clone(),
                                urls_visited: vec![url.clone()],
                                findings_count: 0,
                                error: Some(e.to_string()),
                            });
                            continue;
                        }
                    };

                    let absorbed = self.absorb_crawl_pages(&crawl_result.pages, &mut findings, &mut sources);
                    pages_crawled += absorbed.urls_visited.len();

                    steps.push(ResearchStep {
                        step_number: step_num,
                        action: action.clone(),
                        urls_visited: absorbed.urls_visited,
                        findings_count: absorbed.findings_count,
                        error: None,
                    });
                }
                StepAction::Synthesize => {
                    steps.push(ResearchStep {
                        step_number: step_num,
                        action: action.clone(),
                        urls_visited: Vec::new(),
                        findings_count: 0,
                        error: None,
                    });
                    break;
                }
            }
        }

        let synthesis = synthesizer.synthesize(&self.config.query, &findings, &sources).await?;

        Ok(ResearchResult {
            query: self.config.query.clone(),
            synthesis,
            findings,
            sources,
            steps,
            pages_crawled,
            cost: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent_for(query: &str) -> ResearchAgent {
        ResearchAgent::new(ResearchConfig {
            query: query.to_owned(),
            max_steps: 1,
            max_pages_per_step: 10,
            max_depth: 1,
            seed_urls: Vec::new(),
        })
    }

    fn page(url: &str, title: Option<&str>, markdown: Option<&str>) -> crate::types::CrawlPageResult {
        crate::types::CrawlPageResult {
            url: url.to_owned(),
            metadata: crate::types::PageMetadata {
                title: title.map(str::to_owned),
                ..Default::default()
            },
            markdown: markdown.map(|content| crate::types::MarkdownResult {
                content: content.to_owned(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    /// A site where `/login` sets the cookie `session=1` and links to `/next`, and every other
    /// page sets nothing and links nowhere.
    async fn session_site() -> wiremock::MockServer {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let site = MockServer::start().await;
        let html = |body: &str| {
            ResponseTemplate::new(200).set_body_raw(format!("<html><body>{body}</body></html>"), "text/html")
        };
        Mock::given(method("GET"))
            .and(path("/login"))
            .respond_with(html(r#"<a href="/next">next</a>"#).append_header("set-cookie", "session=1; Path=/"))
            .mount(&site)
            .await;
        Mock::given(method("GET")).respond_with(html("page")).mount(&site).await;
        site
    }

    /// Run an agent over `seeds` of `site`, at most `max_pages_per_step` pages a step, and return
    /// the path and the cookie header of each request the site received.
    async fn session_requests(
        site: &wiremock::MockServer,
        seeds: &[&str],
        max_pages_per_step: usize,
    ) -> Vec<(String, String)> {
        let agent = ResearchAgent::new(ResearchConfig {
            query: "rust".to_owned(),
            max_steps: seeds.len() + 1,
            max_pages_per_step,
            max_depth: 1,
            seed_urls: seeds.iter().map(|seed| format!("{}{seed}", site.uri())).collect(),
        })
        .with_crawl_config(CrawlConfig {
            browser: crate::types::BrowserConfig {
                mode: crate::types::BrowserMode::Never,
                ..Default::default()
            },
            cookies_enabled: true,
            respect_robots_txt: false,
            max_depth: Some(0),
            ..CrawlConfig::builder().allow_private_networks(true).build()
        });

        let result = agent.research().await.expect("the research must finish");
        let errors: Vec<_> = result.steps.iter().filter_map(|step| step.error.as_deref()).collect();
        assert!(errors.is_empty(), "no step may fail: {errors:?}");

        let requests = site.received_requests().await.expect("request recording is on");
        requests
            .iter()
            .map(|request| {
                let cookie = request.headers.get("cookie").and_then(|value| value.to_str().ok());
                (request.url.path().to_owned(), cookie.unwrap_or_default().to_owned())
            })
            .collect()
    }

    #[tokio::test]
    async fn a_later_step_sends_the_cookie_that_an_earlier_step_received() {
        let site = session_site().await;

        assert_eq!(
            session_requests(&site, &["/login", "/data"], 1).await,
            [
                ("/login".to_owned(), String::new()),
                ("/data".to_owned(), "session=1".to_owned())
            ],
            "the steps of one agent are one session"
        );
    }

    #[tokio::test]
    async fn a_step_crawls_to_the_depth_of_its_plan() {
        let site = session_site().await;

        assert_eq!(
            session_requests(&site, &["/login"], 2).await,
            [
                ("/login".to_owned(), String::new()),
                ("/next".to_owned(), "session=1".to_owned())
            ],
            "the plan's depth of 1 replaces the crawl configuration's depth of 0"
        );
    }

    #[tokio::test]
    async fn a_step_fetches_no_more_pages_than_the_limit_for_a_step() {
        let site = session_site().await;

        assert_eq!(
            session_requests(&site, &["/login"], 1).await,
            [("/login".to_owned(), String::new())],
            "a step fetches no more pages than the limit for a step"
        );
    }

    #[test]
    fn absorb_crawl_pages_records_every_page_as_a_source() {
        let agent = agent_for("rust");
        let mut findings = Vec::new();
        let mut sources = Vec::new();

        let pages = vec![
            page("https://a.example/1", Some("One"), Some("rust content")),
            page("https://a.example/2", None, None),
        ];
        let absorbed = agent.absorb_crawl_pages(&pages, &mut findings, &mut sources);

        assert_eq!(
            absorbed.urls_visited,
            vec!["https://a.example/1".to_owned(), "https://a.example/2".to_owned()],
            "every crawled page must be reported as visited, with or without markdown"
        );
        assert_eq!(sources.len(), 2, "every crawled page must become a source");
        assert_eq!(sources[0].title.as_deref(), Some("One"));
        assert_eq!(sources[0].snippet.as_deref(), Some("rust content"));
        assert_eq!(sources[1].snippet, None, "a page without markdown has no snippet");
    }

    #[test]
    fn absorb_crawl_pages_skips_pages_with_empty_or_missing_markdown() {
        let agent = agent_for("rust language");
        let mut findings = Vec::new();
        let mut sources = Vec::new();

        let pages = vec![
            page("https://a.example/1", None, Some("rust is a language")),
            page("https://a.example/2", None, Some("")),
            page("https://a.example/3", None, None),
        ];
        let absorbed = agent.absorb_crawl_pages(&pages, &mut findings, &mut sources);

        assert_eq!(absorbed.findings_count, 1, "only non-empty markdown yields a finding");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source_url, "https://a.example/1");
        assert!(
            (findings[0].relevance_score - 1.0).abs() < f64::EPSILON,
            "both query words appear, so the score must be 1.0, got {}",
            findings[0].relevance_score
        );
    }

    #[test]
    fn absorb_crawl_pages_truncates_snippets_and_finding_content() {
        let agent = agent_for("rust");
        let mut findings = Vec::new();
        let mut sources = Vec::new();

        let long = "x".repeat(1000);
        let absorbed = agent.absorb_crawl_pages(
            &[page("https://a.example/1", None, Some(&long))],
            &mut findings,
            &mut sources,
        );

        assert_eq!(absorbed.findings_count, 1);
        assert_eq!(
            sources[0].snippet.as_deref().map(|s| s.chars().count()),
            Some(200),
            "the source snippet must be capped at 200 characters"
        );
        assert_eq!(
            findings[0].content.chars().count(),
            500,
            "the finding content must be capped at 500 characters"
        );
    }

    #[test]
    fn test_simple_relevance_score_full_match() {
        let score = simple_relevance_score("rust language", "Rust is a great programming language");
        assert!(score > 0.5, "expected > 0.5, got {score}");
    }

    #[test]
    fn test_simple_relevance_score_no_match() {
        let score = simple_relevance_score("quantum physics", "Rust is a great programming language");
        assert!((score - 0.0).abs() < f64::EPSILON, "expected 0.0, got {score}");
    }

    #[test]
    fn test_simple_relevance_score_empty_query() {
        let score = simple_relevance_score("", "some content");
        assert!((score - 0.0).abs() < f64::EPSILON, "expected 0.0, got {score}");
    }

    #[test]
    fn test_simple_relevance_score_partial_match() {
        let score = simple_relevance_score("rust async patterns", "Rust has many useful patterns");
        assert!(score > 0.6 && score < 0.7, "expected ~0.667, got {score}");
    }
}
