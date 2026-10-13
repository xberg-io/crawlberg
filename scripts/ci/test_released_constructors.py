"""Ensure the released-constructor check fails every change that breaks a released call."""

from __future__ import annotations

import importlib.util
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "check_released_constructors", ROOT / "scripts/ci/check_released_constructors.py"
)
assert SPEC is not None and SPEC.loader is not None
CHECK = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = CHECK
SPEC.loader.exec_module(CHECK)

TAG = "v1.0.0"
# ~keep The shape alef writes: the fields in struct order, then a constructor with the optional
# field last. `depth` is field 2 of the struct and positional argument 3.
FIELDS = ("wrap: bool", "depth: int | None", "width: int")
ARGUMENTS = "wrap: bool = ..., width: int = ..., depth: int | None = None"


def stub(fields: tuple[str, ...] = FIELDS, arguments: str = ARGUMENTS) -> str:
    lines = ["class Options:", *(f"    {declaration}" for declaration in fields)]
    lines.append(f"    def __init__(self, {arguments}) -> None: ...")
    return "\n".join(lines) + "\n"


def errors(head: str, released: str | None = None) -> list[str]:
    return CHECK.compare(CHECK.parse_stub(released or stub()), CHECK.parse_stub(head), TAG)


class KeptConstructorTests(unittest.TestCase):
    def test_an_unchanged_stub_passes(self) -> None:
        # ~keep This is also the stub of a new field with `alef(skip)`: alef leaves it out.
        assert errors(stub()) == []

    def test_an_appended_field_with_the_recorded_constructor_passes(self) -> None:
        # ~keep With `legacy_constructor_fields` alef writes the new field keyword-only with a default.
        head = stub((*FIELDS, "hidden: bool"), f"{ARGUMENTS}, *, hidden: bool = ...")
        assert errors(head) == []

    def test_a_second_optional_keyword_argument_passes(self) -> None:
        released = stub((*FIELDS, "hidden: bool"), f"{ARGUMENTS}, *, hidden: bool = ...")
        head = stub(
            (*FIELDS, "hidden: bool", "compact: bool"), f"{ARGUMENTS}, *, hidden: bool = ..., compact: bool = ..."
        )
        assert errors(head, released) == []

    def test_a_new_type_passes(self) -> None:
        assert errors(stub() + stub().replace("Options", "Extras")) == []


class BrokenConstructorTests(unittest.TestCase):
    def assert_fails(self, found: list[str], *parts: str) -> None:
        assert any(all(part in error for part in parts) for error in found), found

    def test_a_required_keyword_argument_appended_fails(self) -> None:
        head = stub((*FIELDS, "hidden: bool"), f"{ARGUMENTS}, *, hidden: bool")
        self.assert_fails(errors(head), "Options.hidden", "required")

    def test_a_keyword_default_removed_fails(self) -> None:
        released = stub((*FIELDS, "hidden: bool"), f"{ARGUMENTS}, *, hidden: bool = ...")
        head = stub((*FIELDS, "hidden: bool"), f"{ARGUMENTS}, *, hidden: bool")
        self.assert_fails(errors(head, released), "Options.hidden", "required now")

    def test_a_positional_default_removed_fails(self) -> None:
        head = stub(arguments="wrap: bool, width: int = ..., depth: int | None = None")
        self.assert_fails(errors(head), "Options.wrap", "required now")

    def test_a_field_inserted_in_the_middle_fails(self) -> None:
        head = stub(
            ("wrap: bool", "hidden: bool", "depth: int | None", "width: int"),
            "wrap: bool = ..., hidden: bool = ..., width: int = ..., depth: int | None = None",
        )
        self.assert_fails(errors(head), "Options.hidden", "field 2 of the struct")

    def test_a_field_inserted_in_the_middle_with_the_recorded_constructor_fails(self) -> None:
        # ~keep The recorded constructor keeps the native one. The bindings in struct order shift.
        head = stub(
            ("wrap: bool", "hidden: bool", "depth: int | None", "width: int"), f"{ARGUMENTS}, *, hidden: bool = ..."
        )
        self.assert_fails(errors(head), "Options.hidden", "field 2 of the struct")

    def test_an_appended_field_with_no_recorded_constructor_fails(self) -> None:
        head = stub(
            (*FIELDS, "hidden: bool"),
            "wrap: bool = ..., width: int = ..., hidden: bool = ..., depth: int | None = None",
        )
        found = errors(head)
        self.assert_fails(found, "Options.hidden", "positional argument 3", "no recorded constructor")
        self.assert_fails(found, "Options.depth", "positional argument 3", "is now positional argument 4")

    def test_an_appended_positional_argument_at_the_end_fails(self) -> None:
        head = stub((*FIELDS, "hidden: bool"), f"{ARGUMENTS}, hidden: bool = ...")
        self.assert_fails(errors(head), "Options.hidden", "positional argument 4", "no recorded constructor")

    def test_a_removed_field_fails(self) -> None:
        head = stub(("wrap: bool", "depth: int | None"), "wrap: bool = ..., depth: int | None = None")
        self.assert_fails(errors(head), "Options.width", "not in the bindings")

    def test_a_retyped_field_fails(self) -> None:
        head = stub(
            ("wrap: bool", "depth: int | None", "width: int | None"),
            "wrap: bool = ..., width: int | None = ..., depth: int | None = None",
        )
        self.assert_fails(errors(head), "Options.width", "`int` to `int | None`")

    def test_a_retyped_argument_fails(self) -> None:
        head = stub(arguments="wrap: bool = ..., width: str = ..., depth: int | None = None")
        self.assert_fails(errors(head), "Options.width", "argument type", "`int` to `str`")

    def test_a_renamed_argument_fails(self) -> None:
        head = stub(arguments="wrap: bool = ..., columns: int = ..., depth: int | None = None")
        self.assert_fails(errors(head), "Options.width", "removed or renamed")

    def test_a_positional_argument_made_keyword_only_fails(self) -> None:
        head = stub(arguments="wrap: bool = ..., width: int = ..., *, depth: int | None = None")
        self.assert_fails(errors(head), "Options.depth", "positional argument 3", "keyword-only")

    def test_a_keyword_argument_made_positional_only_fails(self) -> None:
        head = stub(arguments="wrap: bool = ..., /, width: int = ..., depth: int | None = None")
        self.assert_fails(errors(head), "Options.wrap", "by keyword")

    def test_a_type_with_no_constructor_at_the_head_fails(self) -> None:
        self.assert_fails(errors("class Options:\n    wrap: bool\n"), "Options", "no constructor")


def facade(parameters: str) -> dict[str, dict[str, bool]]:
    lines = ["public struct Link: Codable {", f"    public init({parameters}) {{", "    }"]
    lines += ["    public init(from decoder: any Decoder) throws {", "    }", "}"]
    return CHECK.parse_swift_facade("\n".join(lines))


SWIFT_RELEASED = 'url: String, rel: String? = nil, marks: [String] = [",", "("], nofollow: Bool'


class SwiftFacadeTests(unittest.TestCase):
    def test_labels_and_defaults_are_read(self) -> None:
        assert facade(SWIFT_RELEASED) == {"Link": {"url": False, "rel": True, "marks": True, "nofollow": False}}

    def test_a_new_argument_with_a_default_passes(self) -> None:
        head = facade(SWIFT_RELEASED + ', originalUrl: String = ""')
        assert CHECK.compare_swift(facade(SWIFT_RELEASED), head, TAG) == []

    def test_a_new_required_argument_fails(self) -> None:
        head = facade(SWIFT_RELEASED + ", originalUrl: String")
        found = CHECK.compare_swift(facade(SWIFT_RELEASED), head, TAG)
        assert found == ["Link.originalUrl: the new Swift initializer argument is required: no call of v1.0.0 compiles"]

    def test_a_lost_default_a_removed_label_and_a_new_order_fail(self) -> None:
        head = facade("nofollow: Bool, url: String, rel: String?")
        found = "\n".join(CHECK.compare_swift(facade(SWIFT_RELEASED), head, TAG))
        assert "Link: the Swift initializer labels of v1.0.0 are in another order" in found
        assert "Link.rel: the Swift argument has a default in v1.0.0 and is required now" in found
        assert "Link.marks: the Swift initializer argument of v1.0.0 is removed or renamed" in found

    def test_a_struct_with_no_initializer_at_the_head_fails(self) -> None:
        found = CHECK.compare_swift(facade(SWIFT_RELEASED), {}, TAG)
        assert found == ["Link: the Swift struct of v1.0.0 has no public initializer at the head"]


class ReleaseTagTests(unittest.TestCase):
    def test_the_highest_version_is_chosen_in_any_list_order(self) -> None:
        # ~keep The list has no ancestry: `v1.10.3` on a release branch is chosen like any other.
        assert CHECK.newest_release_tag(["v1.10.3", "v1.9.0", "v1.10.0", "v1.2.11"]) == "v1.10.3"
        assert CHECK.newest_release_tag(["v1.10.0", "v1.10.3", "v1.9.0"]) == "v1.10.3"

    def test_versions_compare_by_number(self) -> None:
        assert CHECK.newest_release_tag(["v1.9.0", "v1.10.0", "v1.2.30"]) == "v1.10.0"

    def test_a_pre_release_is_ignored(self) -> None:
        assert CHECK.newest_release_tag(["v1.10.3", "v1.11.0-rc.1", "v2.0.0-rc.4"]) == "v1.10.3"

    def test_other_tag_shapes_are_ignored(self) -> None:
        assert CHECK.newest_release_tag(["v1.10.3", "packages/go/v1.11.0", "v2", "v1.11"]) == "v1.10.3"

    def test_no_release_tag_gives_none(self) -> None:
        assert CHECK.newest_release_tag(["v1.0.0-rc.1", "packages/go/v1.0.0"]) is None

    def test_a_tag_without_the_stub_gives_none(self) -> None:
        # ~keep The empty tree has no file, like a release older than the Python binding.
        assert CHECK.released_stub("4b825dc642cb6eb9a060e54bf8d69288fbee4904") is None


if __name__ == "__main__":
    unittest.main()
