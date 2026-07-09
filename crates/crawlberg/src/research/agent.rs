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

impl ResearchAgent {
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
                    let mut step_config = self.crawl_config.clone();
                    step_config.max_depth = Some(*depth);
                    step_config.max_pages = Some(self.config.max_pages_per_step);

                    let engine = CrawlEngine::builder().config(step_config).build()?;

                    let result = engine.crawl(url).await;

                    match result {
                        Ok(crawl_result) => {
                            let mut step_urls = Vec::new();
                            let mut step_findings = 0;

                            for page in &crawl_result.pages {
                                pages_crawled += 1;
                                step_urls.push(page.url.clone());

                                sources.push(SourceInfo {
                                    url: page.url.clone(),
                                    title: page.metadata.title.clone(),
                                    snippet: page.markdown.as_ref().map(|m| m.content.chars().take(200).collect()),
                                });

                                if let Some(ref md) = page.markdown
                                    && !md.content.is_empty()
                                {
                                    let score = simple_relevance_score(&self.config.query, &md.content);
                                    findings.push(Finding {
                                        content: md.content.chars().take(500).collect(),
                                        source_url: page.url.clone(),
                                        relevance_score: score,
                                    });
                                    step_findings += 1;
                                }
                            }

                            steps.push(ResearchStep {
                                step_number: step_num,
                                action: action.clone(),
                                urls_visited: step_urls,
                                findings_count: step_findings,
                                error: None,
                            });
                        }
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
                    }
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
