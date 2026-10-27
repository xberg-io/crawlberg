"""Guard manual plugin publication against unintended registry writes and skipped validation."""

from __future__ import annotations

import ast
import re
import unittest
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[2]
PLUGIN_JOBS = ("dispatch-plugin-npm", "dispatch-plugin-hermes")


def evaluate_condition(expression: str, *, event: str, plugins_only: bool, ref: str) -> bool:
    values = {
        "github.event_name": event,
        "inputs.plugins-only": plugins_only,
        "github.ref": ref,
        "github.actor": "release-owner",
    }
    source = expression.strip().removeprefix("${{").removesuffix("}}")
    for name, value in values.items():
        source = source.replace(name, repr(value))
    source = source.replace("&&", " and ").replace("||", " or ")
    source = re.sub(r"!(?!=)", "not ", source)
    tree = ast.parse(source.strip(), mode="eval")

    def evaluate(node: ast.AST) -> bool | str:
        match node:
            case ast.Constant(value=value):
                return value
            case ast.BoolOp(op=ast.And(), values=operands):
                return all(evaluate(operand) for operand in operands)
            case ast.BoolOp(op=ast.Or(), values=operands):
                return any(evaluate(operand) for operand in operands)
            case ast.UnaryOp(op=ast.Not(), operand=operand):
                return not evaluate(operand)
            case ast.Compare(left=left, ops=[ast.Eq()], comparators=[right]):
                return evaluate(left) == evaluate(right)
            case ast.Compare(left=left, ops=[ast.NotEq()], comparators=[right]):
                return evaluate(left) != evaluate(right)
            case _:
                raise AssertionError(f"Unsupported workflow condition: {ast.dump(node)}")

    return bool(evaluate(tree.body))


class PluginDispatchTests(unittest.TestCase):
    def setUp(self) -> None:
        self.jobs = yaml.safe_load((ROOT / ".github/workflows/publish.yaml").read_text())["jobs"]

    def test_dry_run_reaches_both_registry_publishers(self) -> None:
        for name in PLUGIN_JOBS:
            with self.subTest(job=name):
                steps = self.jobs[name]["steps"]
                publisher = next(step for step in steps if "/publish-" in step.get("uses", ""))
                assert publisher["with"].get("dry-run") == "${{ inputs.dry_run }}"

    def test_non_main_plugin_dispatch_fails_instead_of_skipping_every_job(self) -> None:
        guard = self.jobs.get("validate-plugin-dispatch")
        assert guard is not None, "A plugin-only dispatch on another branch needs a failing validation job"
        for event, plugins_only, ref, expected in (
            ("workflow_dispatch", True, "refs/heads/feature", True),
            ("workflow_dispatch", True, "refs/tags/v1.11.0", True),
            ("workflow_dispatch", True, "refs/heads/main", False),
            ("workflow_dispatch", False, "refs/heads/feature", False),
            ("release", False, "refs/tags/v1.11.0", False),
        ):
            with self.subTest(event=event, plugins_only=plugins_only, ref=ref):
                assert evaluate_condition(guard["if"], event=event, plugins_only=plugins_only, ref=ref) is expected
        assert len(guard["steps"]) == 1
        failure = guard["steps"][0]
        assert failure["uses"].startswith("actions/github-script@")
        assert "core.setFailed(" in failure["with"]["script"]

    def test_only_main_plugin_dispatch_admits_registry_jobs(self) -> None:
        for name in PLUGIN_JOBS:
            for event, plugins_only, ref, expected in (
                ("workflow_dispatch", True, "refs/heads/main", True),
                ("workflow_dispatch", True, "refs/heads/feature", False),
                ("workflow_dispatch", False, "refs/heads/main", False),
                ("release", False, "refs/tags/v1.11.0", False),
            ):
                with self.subTest(job=name, event=event, plugins_only=plugins_only, ref=ref):
                    actual = evaluate_condition(self.jobs[name]["if"], event=event, plugins_only=plugins_only, ref=ref)
                    assert actual is expected

    def test_plugin_verification_uses_the_v1_actions(self) -> None:
        expected = "v1"
        for name in PLUGIN_JOBS:
            actions = [
                step["uses"]
                for step in self.jobs[name]["steps"]
                if step.get("uses", "").startswith(
                    ("xberg-io/actions/setup-ai-rulez@", "xberg-io/actions/verify-ai-rulez-plugin@")
                )
            ]
            with self.subTest(job=name):
                assert len(actions) == 2
                assert all(action.endswith(f"@{expected}") for action in actions)


if __name__ == "__main__":
    unittest.main()
