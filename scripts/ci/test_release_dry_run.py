"""Ensure release rehearsals cannot move tags or bypass the real-tag guard."""

from __future__ import annotations

import ast
import itertools
import os
import re
import subprocess
import unittest
from pathlib import Path

import yaml
from test_plugin_dispatch import evaluate_condition

ROOT = Path(__file__).resolve().parents[2]


def release_condition(expression: str, values: dict[str, bool | str | None]) -> bool:
    source = expression.replace("!cancelled()", "true")
    for name, value in values.items():
        source = source.replace(name, repr(value))
    source = re.sub(r"(?<!['\"])\b(true|false)\b(?!['\"])", lambda match: match[0].title(), source)
    return evaluate_condition(source, event="workflow_dispatch", plugins_only=False, ref="refs/heads/main")


def resolve_checkout_ref(expression: str, values: dict[str, object]) -> object:
    """Evaluate checkout fallback operands with their values, not boolean coercion."""
    source = " ".join(expression.removeprefix("${{").removesuffix("}}").split())
    source = source.replace("&&", "and").replace("||", "or")

    def resolve(node: ast.expr) -> object:
        if isinstance(node, ast.BoolOp):
            result = resolve(node.values[0])
            for value in node.values[1:]:
                if isinstance(node.op, ast.And) and not result:
                    return result
                if isinstance(node.op, ast.Or) and result:
                    return result
                result = resolve(value)
            return result
        if isinstance(node, ast.Compare):
            assert len(node.ops) == len(node.comparators) == 1
            assert isinstance(node.ops[0], ast.Eq)
            return resolve(node.left) == resolve(node.comparators[0])
        if isinstance(node, ast.Constant):
            return node.value
        if isinstance(node, ast.Name) and node.id in ("true", "false"):
            return node.id == "true"
        if isinstance(node, ast.Attribute):
            return values[ast.unparse(node)]
        raise AssertionError(f"Unsupported checkout expression: {ast.dump(node)}")

    return resolve(ast.parse(source, mode="eval").body)


class ReleaseDryRunTests(unittest.TestCase):
    def setUp(self) -> None:
        self.jobs = yaml.safe_load((ROOT / ".github/workflows/publish.yaml").read_text())["jobs"]

    def test_go_subdirectory_tags_require_a_real_release(self) -> None:
        for dry_run, is_tag, expected in (
            (True, True, False),
            (True, False, False),
            (False, True, True),
            (False, False, False),
        ):
            with self.subTest(dry_run=dry_run, is_tag=is_tag):
                values = {
                    "needs.prepare.outputs.release_go": "true",
                    "needs.prepare.outputs.is_tag": str(is_tag).lower(),
                    "needs.prepare.outputs.dry_run": str(dry_run).lower(),
                    "needs.validate-versions.result": "success",
                    "needs.upload-go-release.result": "success",
                    "needs.publish-swift-checksums.result": "skipped",
                }
                assert release_condition(self.jobs["push-go-subdir-tag"]["if"], values) is expected

    def check_republish_mode(self, channel: str) -> None:
        retag = next(step for step in self.jobs["prepare"]["steps"] if step.get("name") == "Retag for republish")
        for republish, dry_run, expected in (
            (True, True, False),
            (True, "true", False),
            (True, False, True),
            (True, "false", True),
            (True, None, True),
            (False, True, False),
            (False, False, False),
        ):
            with self.subTest(channel=channel, republish=republish, dry_run=dry_run):
                values = {
                    "inputs.republish": False,
                    "inputs.dry_run": False,
                    "github.event.client_payload.republish": False,
                    "github.event.client_payload.dry_run": False,
                }
                values[f"{channel}.republish"] = republish
                values[f"{channel}.dry_run"] = dry_run
                assert release_condition(retag["if"], values) is expected

    def test_manual_dry_run_cannot_retag_for_republish(self) -> None:
        self.check_republish_mode("inputs")

    def test_repository_dispatch_dry_run_cannot_retag_for_republish(self) -> None:
        self.check_republish_mode("github.event.client_payload")

    def test_real_tag_guard_still_rejects_non_tags_including_dry_runs(self) -> None:
        guard = next(
            step for step in self.jobs["prepare"]["steps"] if step.get("name") == "Assert the run can publish at all"
        )
        for dry_run in (False, True):
            for is_tag in (False, True):
                with self.subTest(dry_run=dry_run, is_tag=is_tag):
                    result = subprocess.run(
                        ["bash", "-c", guard["run"]],
                        cwd=ROOT,
                        env={
                            **os.environ,
                            "IS_TAG": str(is_tag).lower(),
                            "DRY_RUN": str(dry_run).lower(),
                            "RESOLVED_REF": "refs/tags/v1.11.2" if is_tag else "refs/heads/main",
                            "TARGETS": "all",
                        },
                        capture_output=True,
                        text=True,
                        check=False,
                    )
                    assert result.returncode == (0 if is_tag else 1), result.stderr
                    if not is_tag:
                        assert "is not a tag" in result.stderr

    def test_initial_checkout_preserves_priorities_and_pins_dry_run_source(self) -> None:
        checkout = [step for step in self.jobs["prepare"]["steps"] if step.get("name") == "Checkout"]
        assert len(checkout) == 1
        cases = list(
            itertools.product(
                (False, True),
                (True, "true", False, "false", None),
                (True, "true", False, "false", None),
                (None, "", "release-candidate", "a" * 40),
                (None, "", "v1.11.2"),
                ("refs/heads/main", "refs/tags/v1.11.2"),
            )
        )
        assert len(cases) == 1200
        for republish, dry_run, payload_dry_run, requested_ref, tag, event_ref in cases:
            with self.subTest(
                republish=republish,
                dry_run=dry_run,
                payload_dry_run=payload_dry_run,
                ref=requested_ref,
                tag=tag,
                event=event_ref,
            ):
                values = {
                    "inputs.republish": republish,
                    "inputs.ref": requested_ref,
                    "inputs.tag": tag,
                    "inputs.dry_run": dry_run,
                    "github.event.client_payload.dry_run": payload_dry_run,
                    "github.event.repository.default_branch": "main",
                    "github.sha": "b" * 40,
                    "github.ref": event_ref,
                }
                if republish:
                    expected = requested_ref or "main"
                elif requested_ref or tag:
                    expected = requested_ref or tag
                elif dry_run is True or dry_run == "true" or payload_dry_run is True or payload_dry_run == "true":
                    expected = "b" * 40
                else:
                    expected = event_ref
                assert resolve_checkout_ref(checkout[0]["with"]["ref"], values) == expected

    def test_initial_checkout_negative_control_detects_moving_branch(self) -> None:
        retired = (
            "${{ (inputs.republish == true && (inputs.ref || github.event.repository.default_branch)) "
            "|| inputs.ref || inputs.tag || github.ref }}"
        )
        values = {
            "inputs.republish": False,
            "inputs.ref": "",
            "inputs.tag": "",
            "inputs.dry_run": True,
            "github.event.repository.default_branch": "main",
            "github.sha": "b" * 40,
            "github.ref": "refs/heads/main",
        }
        assert resolve_checkout_ref(retired, values) == "refs/heads/main"
        rejected = False
        try:
            assert resolve_checkout_ref(retired, values) == values["github.sha"]
        except AssertionError:
            rejected = True
        assert rejected


if __name__ == "__main__":
    unittest.main()
