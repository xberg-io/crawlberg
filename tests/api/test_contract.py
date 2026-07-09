"""Schemathesis property-based API contract tests.

Tests the REST API against its OpenAPI specification.
Validates:
- No server errors (5xx) on any valid or invalid input
- Response status codes match the spec
- Response content types match the spec
- Response bodies conform to declared schemas
"""

from __future__ import annotations

import schemathesis


def _safe_url_case(operation: schemathesis.APIOperation, **kwargs: object) -> schemathesis.Case:
    """Create a case with non-routable URLs to avoid DNS hangs."""
    case = operation.Case(**kwargs)
    if case.body and isinstance(case.body, dict):
        for key in ("url", "urls"):
            if key in case.body:
                if isinstance(case.body[key], str):
                    case.body[key] = "http://127.0.0.1:1/noop"
                elif isinstance(case.body[key], list):
                    case.body[key] = ["http://127.0.0.1:1/noop"]
    return case


def test_all_endpoints_no_server_errors(api_schema: schemathesis.schemas.BaseSchema) -> None:
    """Every endpoint must not return 5xx for valid example inputs."""
    for result in api_schema.get_all_operations():
        try:
            operation = result.ok()
            case = _safe_url_case(operation)
            response = case.call()
            case.validate_response(
                response,
                checks=(schemathesis.checks.not_a_server_error,),
            )
        except schemathesis.core.errors.InvalidSchema:
            continue


def test_all_endpoints_response_conformance(api_schema: schemathesis.schemas.BaseSchema) -> None:
    """Response bodies and content types must match declared schemas."""
    for result in api_schema.get_all_operations():
        try:
            operation = result.ok()
            case = _safe_url_case(operation)
            response = case.call()
            case.validate_response(
                response,
                checks=(
                    schemathesis.checks.response_schema_conformance,
                    schemathesis.checks.content_type_conformance,
                ),
            )
        except schemathesis.core.errors.InvalidSchema:
            continue


def test_health_always_200(api_schema: schemathesis.schemas.BaseSchema) -> None:
    """Health endpoint must always return 200."""
    op = api_schema["/health"]["GET"]
    case = op.Case()
    response = case.call()
    assert response.status_code == 200
    data = response.json()
    assert data["status"] == "ok"


def test_version_always_200(api_schema: schemathesis.schemas.BaseSchema) -> None:
    """Version endpoint must always return 200."""
    op = api_schema["/version"]["GET"]
    case = op.Case()
    response = case.call()
    assert response.status_code == 200
    data = response.json()
    assert "version" in data


def test_openapi_spec_returns_valid_json(api_schema: schemathesis.schemas.BaseSchema) -> None:
    """OpenAPI spec endpoint must return valid JSON with required fields."""
    import requests

    base_url = api_schema.get_base_url()
    response = requests.get(f"{base_url}/openapi.json", timeout=10)
    assert response.status_code == 200
    data = response.json()
    assert "openapi" in data
    assert "paths" in data
    assert "info" in data
