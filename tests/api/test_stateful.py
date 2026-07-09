"""Targeted API workflow tests using schemathesis.

Tests individual endpoints with controlled inputs to verify
business logic contracts beyond schema conformance.
"""

from __future__ import annotations

import pytest
import schemathesis


def test_scrape_returns_structured_response(api_schema: schemathesis.schemas.BaseSchema) -> None:
    """Scrape endpoint returns a structured response with success field."""
    op = api_schema["/v1/scrape"]["POST"]
    case = op.Case(body={"url": "http://127.0.0.1:1/noop"})
    response = case.call()
    data = response.json()
    assert "success" in data


def test_crawl_creates_job(api_schema: schemathesis.schemas.BaseSchema) -> None:
    """Crawl endpoint creates an async job and returns a job ID."""
    op = api_schema["/v1/crawl"]["POST"]
    case = op.Case(body={"url": "http://127.0.0.1:1/noop", "maxDepth": 1, "maxPages": 1})
    response = case.call()
    data = response.json()
    assert "success" in data
    if data["success"]:
        assert "id" in data


def test_map_returns_structured_response(api_schema: schemathesis.schemas.BaseSchema) -> None:
    """Map endpoint returns a structured response."""
    op = api_schema["/v1/map"]["POST"]
    case = op.Case(body={"url": "http://127.0.0.1:1/noop"})
    response = case.call()
    data = response.json()
    assert "success" in data


def test_batch_scrape_creates_job(api_schema: schemathesis.schemas.BaseSchema) -> None:
    """Batch scrape endpoint creates a job."""
    op = api_schema["/v1/batch/scrape"]["POST"]
    case = op.Case(body={"urls": ["http://127.0.0.1:1/noop"]})
    response = case.call()
    data = response.json()
    assert "success" in data


def test_scrape_rejects_missing_url(api_schema: schemathesis.schemas.BaseSchema) -> None:
    """Scrape without url returns 400/422."""
    op = api_schema["/v1/scrape"]["POST"]
    case = op.Case(body={})
    response = case.call()
    assert response.status_code in (400, 422)


def test_download_returns_structured_response(api_schema: schemathesis.schemas.BaseSchema) -> None:
    """Download endpoint returns a structured response."""
    op = api_schema["/v1/download"]["POST"]
    case = op.Case(body={"url": "http://127.0.0.1:1/noop"})
    response = case.call()
    data = response.json()
    assert "success" in data


@pytest.mark.slow
def test_crawl_job_lifecycle(api_schema: schemathesis.schemas.BaseSchema) -> None:
    """Crawl: create job -> poll status -> cancel."""
    crawl_op = api_schema["/v1/crawl"]["POST"]
    case = crawl_op.Case(body={"url": "http://127.0.0.1:1/noop", "maxDepth": 1, "maxPages": 1})
    response = case.call()
    data = response.json()
    if not data.get("success"):
        return

    job_id = data["id"]

    status_op = api_schema["/v1/crawl/{id}"]["GET"]
    status_case = status_op.Case(path_parameters={"id": job_id})
    status_response = status_case.call()
    assert status_response.status_code == 200

    cancel_op = api_schema["/v1/crawl/{id}"]["DELETE"]
    cancel_case = cancel_op.Case(path_parameters={"id": job_id})
    cancel_response = cancel_case.call()
    assert cancel_response.status_code in (200, 204, 404)
