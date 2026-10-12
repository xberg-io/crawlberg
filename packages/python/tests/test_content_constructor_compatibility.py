from __future__ import annotations

import crawlberg
from crawlberg import _crawlberg
from crawlberg.api import _to_rust_content_config


def test_public_content_config_preserves_old_eight_positional_arguments() -> None:
    config = crawlberg.ContentConfig("markdown", "minimal", False, False, [], [], [], True)
    assert config.skip_images is True
    assert config.keep_hidden is False
    native = _to_rust_content_config(config)
    assert native.skip_images is True
    assert native.keep_hidden is False


def test_public_content_config_preserves_all_thirteen_positional_arguments() -> None:
    config = crawlberg.ContentConfig("markdown", "minimal", False, False, [], [], [], True, 5, False, 80, True, False)
    assert config.skip_images is True
    assert config.max_depth == 5
    assert config.extract_metadata is False
    assert config.keep_hidden is False


def test_native_content_config_preserves_legacy_max_depth_position() -> None:
    config = _crawlberg.ContentConfig("markdown", "minimal", False, False, [], [], [], True, False, 80, True, False, 5)
    assert config.skip_images is True
    assert config.max_depth == 5
    assert config.extract_metadata is False
    assert config.keep_hidden is False


def test_native_content_config_accepts_the_added_field_by_keyword() -> None:
    config = _crawlberg.ContentConfig(
        "markdown", "minimal", False, False, [], [], [], True, False, 80, True, False, 5, keep_hidden=True
    )
    assert config.skip_images is True
    assert config.max_depth == 5
    assert config.keep_hidden is True
