//! API request handlers.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use uuid::Uuid;

use crate::types::CrawlConfig;

use super::{
    error::ApiError,
    jobs::JobState,
    state::ApiState,
    types::{
        ApiResponse, BatchScrapeRequest, CrawlRequest, DownloadRequest, HealthResponse, JobCreatedResponse,
        JobStatusResponse, MapRequest, ScrapeRequest, VersionResponse,
    },
};

/// `GET /health`
#[utoipa::path(
    get,
    path = "/health",
    tag = "system",
    responses(
        (status = 200, description = "Service is healthy", body = HealthResponse),
    )
)]
pub async fn health_handler() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}

/// `GET /version`
#[utoipa::path(
    get,
    path = "/version",
    tag = "system",
    responses(
        (status = 200, description = "Version information", body = VersionResponse),
    )
)]
pub async fn version_handler() -> Json<VersionResponse> {
    Json(VersionResponse {
        version: env!("CARGO_PKG_VERSION"),
    })
}

/// Validate that a URL is non-empty, uses an allowed scheme, and is within length limits.
fn validate_url(url: &str) -> Result<(), ApiError> {
    if url.is_empty() {
        return Err(ApiError::bad_request("url is required"));
    }
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(ApiError::bad_request("url must start with http:// or https://"));
    }
    if url.len() > 8192 {
        return Err(ApiError::bad_request("url exceeds maximum length of 8192 characters"));
    }
    Ok(())
}

/// `POST /v1/scrape`
#[utoipa::path(
    post,
    path = "/v1/scrape",
    tag = "scrape",
    request_body = ScrapeRequest,
    responses(
        (status = 200, description = "Scrape result", body = serde_json::Value),
        (status = 400, description = "Bad request"),
        (status = 500, description = "Internal server error"),
    )
)]
pub async fn scrape_handler(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<ScrapeRequest>,
) -> Result<impl IntoResponse, ApiError> {
    validate_url(&req.url)?;

    let result = state.engine.scrape(&req.url).await?;
    let value = serde_json::to_value(&result).map_err(|e| ApiError::internal(format!("serialization error: {e}")))?;

    Ok(Json(ApiResponse::ok(value)))
}

/// `POST /v1/crawl`
#[utoipa::path(
    post,
    path = "/v1/crawl",
    tag = "crawl",
    request_body = CrawlRequest,
    responses(
        (status = 202, description = "Crawl job created", body = JobCreatedResponse),
        (status = 400, description = "Bad request"),
    )
)]
pub async fn crawl_handler(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<CrawlRequest>,
) -> Result<impl IntoResponse, ApiError> {
    validate_url(&req.url)?;

    let job_id = state.jobs.create_job();
    let jobs = state.jobs.clone();
    let engine = state.engine.clone();
    let url = req.url.clone();

    let mut config = engine.config.clone();
    if let Some(depth) = req.max_depth {
        config.max_depth = Some(depth);
    }
    if let Some(pages) = req.max_pages {
        config.max_pages = Some(pages);
    }
    if let Some(true) = req.only_main_content {
        config.content.preprocessing_preset = "aggressive".to_owned();
    }
    if let Some(ref includes) = req.include_paths {
        config.include_paths = includes.clone();
    }
    if let Some(ref excludes) = req.exclude_paths {
        config.exclude_paths = excludes.clone();
    }

    let crawl_engine = rebuild_engine_with_config(&engine, config)?;

    let created_at = std::time::Instant::now();
    tokio::spawn(async move {
        jobs.update(
            &job_id,
            JobState::InProgress {
                pages_completed: 0,
                created_at,
            },
        );

        match crawl_engine.crawl(&url).await {
            Ok(result) => {
                jobs.update(
                    &job_id,
                    JobState::CrawlCompleted {
                        result: Box::new(result),
                        created_at,
                    },
                );
            }
            Err(e) => {
                jobs.update(
                    &job_id,
                    JobState::Failed {
                        message: e.to_string(),
                        created_at,
                    },
                );
            }
        }
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(JobCreatedResponse {
            success: true,
            id: job_id.to_string(),
        }),
    ))
}

/// `GET /v1/crawl/{id}`
#[utoipa::path(
    get,
    path = "/v1/crawl/{id}",
    tag = "crawl",
    params(
        ("id" = String, Path, description = "Job identifier"),
    ),
    responses(
        (status = 200, description = "Job status", body = JobStatusResponse),
        (status = 400, description = "Invalid job id"),
        (status = 404, description = "Job not found"),
    )
)]
pub async fn crawl_status_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let uuid = id
        .parse::<Uuid>()
        .map_err(|_| ApiError::bad_request("invalid job id"))?;

    match state.jobs.get_status(&uuid) {
        Some(job_state) => Ok(Json(job_state_to_response(&job_state))),
        None => Err(ApiError::not_found("job not found")),
    }
}

/// `DELETE /v1/crawl/{id}`
#[utoipa::path(
    delete,
    path = "/v1/crawl/{id}",
    tag = "crawl",
    params(
        ("id" = String, Path, description = "Job identifier"),
    ),
    responses(
        (status = 200, description = "Job cancelled", body = serde_json::Value),
        (status = 404, description = "Job not found or not cancellable"),
    )
)]
pub async fn crawl_cancel_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let uuid = id
        .parse::<Uuid>()
        .map_err(|_| ApiError::bad_request("invalid job id"))?;

    if state.jobs.cancel(&uuid) {
        Ok(Json(ApiResponse::ok("cancelled")))
    } else {
        Err(ApiError::not_found("job not found or not cancellable"))
    }
}

/// `POST /v1/map`
#[utoipa::path(
    post,
    path = "/v1/map",
    tag = "crawl",
    request_body = MapRequest,
    responses(
        (status = 200, description = "Map result", body = serde_json::Value),
        (status = 400, description = "Bad request"),
        (status = 500, description = "Internal server error"),
    )
)]
pub async fn map_handler(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<MapRequest>,
) -> Result<impl IntoResponse, ApiError> {
    validate_url(&req.url)?;

    let mut result = state.engine.map(&req.url).await?;

    if let Some(ref search) = req.search {
        let term = search.to_lowercase();
        result.urls.retain(|u| u.url.to_lowercase().contains(&term));
    }

    if let Some(limit) = req.limit {
        result.urls.truncate(limit);
    }

    let value = serde_json::to_value(&result).map_err(|e| ApiError::internal(format!("serialization error: {e}")))?;

    Ok(Json(ApiResponse::ok(value)))
}

/// `POST /v1/batch/scrape`
#[utoipa::path(
    post,
    path = "/v1/batch/scrape",
    tag = "scrape",
    request_body = BatchScrapeRequest,
    responses(
        (status = 202, description = "Batch scrape job created", body = JobCreatedResponse),
        (status = 400, description = "Bad request"),
    )
)]
pub async fn batch_scrape_handler(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<BatchScrapeRequest>,
) -> Result<impl IntoResponse, ApiError> {
    if req.urls.is_empty() {
        return Err(ApiError::bad_request("urls must not be empty"));
    }

    let job_id = state.jobs.create_job();
    let jobs = state.jobs.clone();
    let engine = state.engine.clone();
    let urls = req.urls.clone();

    let created_at = std::time::Instant::now();
    tokio::spawn(async move {
        jobs.update(
            &job_id,
            JobState::InProgress {
                pages_completed: 0,
                created_at,
            },
        );

        let url_refs: Vec<&str> = urls.iter().map(String::as_str).collect();
        let results = engine.batch_scrape(&url_refs).await;

        let mapped: Vec<(String, Result<crate::types::ScrapeResult, String>)> = results
            .into_iter()
            .map(|(url, res)| (url, res.map_err(|e| e.to_string())))
            .collect();

        jobs.update(
            &job_id,
            JobState::BatchCompleted {
                results: mapped,
                created_at,
            },
        );
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(JobCreatedResponse {
            success: true,
            id: job_id.to_string(),
        }),
    ))
}

/// `GET /v1/batch/scrape/{id}`
#[utoipa::path(
    get,
    path = "/v1/batch/scrape/{id}",
    tag = "scrape",
    params(
        ("id" = String, Path, description = "Job identifier"),
    ),
    responses(
        (status = 200, description = "Batch job status", body = JobStatusResponse),
        (status = 400, description = "Invalid job id"),
        (status = 404, description = "Job not found"),
    )
)]
pub async fn batch_status_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let uuid = id
        .parse::<Uuid>()
        .map_err(|_| ApiError::bad_request("invalid job id"))?;

    match state.jobs.get_status(&uuid) {
        Some(job_state) => Ok(Json(job_state_to_response(&job_state))),
        None => Err(ApiError::not_found("job not found")),
    }
}

/// `POST /v1/download`
#[utoipa::path(
    post,
    path = "/v1/download",
    tag = "download",
    request_body = DownloadRequest,
    responses(
        (status = 200, description = "Download result", body = serde_json::Value),
        (status = 400, description = "Bad request"),
        (status = 500, description = "Internal server error"),
    )
)]
pub async fn download_handler(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<DownloadRequest>,
) -> Result<impl IntoResponse, ApiError> {
    validate_url(&req.url)?;

    let result = state.engine.scrape(&req.url).await?;

    let value = serde_json::to_value(&result).map_err(|e| ApiError::internal(format!("serialization error: {e}")))?;

    Ok(Json(ApiResponse::ok(value)))
}

/// Convert a [`JobState`] into a [`JobStatusResponse`].
fn job_state_to_response(state: &JobState) -> JobStatusResponse {
    match state {
        JobState::Pending { .. } => JobStatusResponse {
            status: "pending".to_string(),
            total: 0,
            completed: 0,
            data: None,
            error: None,
        },
        JobState::InProgress { pages_completed, .. } => JobStatusResponse {
            status: "in_progress".to_string(),
            total: 0,
            completed: *pages_completed,
            data: None,
            error: None,
        },
        JobState::CrawlCompleted { result, .. } => {
            let total = result.pages.len();
            let data: Vec<serde_json::Value> = result
                .pages
                .iter()
                .filter_map(|p| serde_json::to_value(p).ok())
                .collect();
            JobStatusResponse {
                status: "completed".to_string(),
                total,
                completed: total,
                data: Some(data),
                error: None,
            }
        }
        JobState::BatchCompleted { results, .. } => {
            let total = results.len();
            let data: Vec<serde_json::Value> = results
                .iter()
                .filter_map(|(url, res)| {
                    let value = match res {
                        Ok(r) => serde_json::to_value(r).ok()?,
                        Err(e) => serde_json::json!({ "url": url, "error": e }),
                    };
                    Some(value)
                })
                .collect();
            JobStatusResponse {
                status: "completed".to_string(),
                total,
                completed: total,
                data: Some(data),
                error: None,
            }
        }
        JobState::Failed { message, .. } => JobStatusResponse {
            status: "failed".to_string(),
            total: 0,
            completed: 0,
            data: None,
            error: Some(message.clone()),
        },
        JobState::Cancelled { .. } => JobStatusResponse {
            status: "cancelled".to_string(),
            total: 0,
            completed: 0,
            data: None,
            error: None,
        },
    }
}

/// Build a new [`CrawlEngine`] with an overridden [`CrawlConfig`], keeping all
/// trait implementations from the original engine.
///
/// This is used to apply per-request config overrides (max_depth, max_pages, etc.)
/// without mutating the shared engine.
fn rebuild_engine_with_config(
    _engine: &crate::engine::CrawlEngine,
    config: CrawlConfig,
) -> Result<crate::engine::CrawlEngine, ApiError> {
    crate::engine::CrawlEngine::builder()
        .config(config)
        .build()
        .map_err(|e| ApiError::bad_request(format!("invalid config override: {e}")))
}
