"""Ensure the released-constructor check fails every change that breaks a released call."""

from __future__ import annotations

import contextlib
import importlib.util
import io
import subprocess
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


SWIFT_RELEASED = 'url: String, rel: String? = nil, marks: [String] = [",", "("], nofollow: Bool'
DECODER = "public init(from decoder: any Decoder) throws {\n    }"


def swift_text(parameters: str, *, declaration: str = "public init", owner: str = "public struct Link: Codable") -> str:
    """Return a type with one initializer of `parameters` and the `Decodable` initializer, as alef writes it."""
    return f"{owner} {{\n    {declaration}({parameters}) {{\n    }}\n    {DECODER}\n}}\n"


def facade(parameters: str, **shape: str) -> dict[str, CHECK.SwiftInitializer]:
    return CHECK.parse_swift(swift_text(parameters, **shape))


def swift_errors(head: str, released: str = swift_text(SWIFT_RELEASED)) -> list[str]:
    return CHECK.compare_swift(CHECK.parse_swift(released), CHECK.parse_swift(head), TAG)


class SwiftReaderTests(unittest.TestCase):
    def test_labels_types_and_defaults_are_read(self) -> None:
        read = facade(SWIFT_RELEASED)
        memberwise_key = "Link(url: String, rel: String?, marks: [String], nofollow: Bool)"
        assert list(read) == [memberwise_key, "Link(from decoder: any Decoder)"]
        memberwise, decoder = read.values()
        assert [(p.label, p.type, p.default) for p in memberwise.parameters] == [
            ("url", "String", None),
            ("rel", "String?", "nil"),
            ("marks", "[String]", '[",", "("]'),
            ("nofollow", "Bool", None),
        ]
        assert memberwise.effects == ()
        assert decoder.effects == ("throws",)
        assert (decoder.parameters[0].label, decoder.parameters[0].name) == ("from", "decoder")

    def test_a_multi_line_initializer_is_read(self) -> None:
        assert list(CHECK.parse_swift(SWIFT_MULTI_LINE)) == ["Link(url: String)"]

    def test_a_positional_generic_initializer_of_a_bridge_class_is_read(self) -> None:
        read = CHECK.parse_swift(BRIDGE_TEXT)
        assert list(read) == ["Box(ptr: UnsafeMutableRawPointer)", "Box(_ url: G, _ depth: UInt)"]
        assert [p.name for p in read["Box(_ url: G, _ depth: UInt)"].parameters] == ["url", "depth"]

    def test_a_private_initializer_an_unavailable_one_and_a_protocol_requirement_are_not_constructors(self) -> None:
        text = "public struct Link {\n    init(url: String) {}\n    @available(*, unavailable) public init(x: Int) {}\n}\n"
        text += "public protocol P {\n    init(y: Int)\n}\n"
        assert CHECK.parse_swift(text) == {}

    def test_the_bridge_file_of_the_head_names_crawl_page_result(self) -> None:
        # ~keep swift-bridge writes `$` in identifiers; the grammar reads the file all the same.
        bridge = (ROOT / CHECK.SWIFT_BRIDGE).read_text(encoding="utf-8")
        read = CHECK.parse_swift(bridge, CHECK.SWIFT_BRIDGE)
        assert any(key.startswith("CrawlPageResult(_ url: GenericIntoRustString, ") for key in read), list(read)[:3]


class SwiftComparisonTests(unittest.TestCase):
    def assert_fails(self, found: list[str], *parts: str) -> None:
        assert any(all(part in error for part in parts) for error in found), found

    def test_a_new_argument_with_a_default_passes(self) -> None:
        assert swift_errors(swift_text(SWIFT_RELEASED + ', originalUrl: String = ""')) == []

    def test_a_new_required_argument_fails(self) -> None:
        found = swift_errors(swift_text(SWIFT_RELEASED + ", originalUrl: String"))
        assert found == ["Link.originalUrl: the new Swift initializer argument is required: no call of v1.0.0 compiles"]

    def test_a_lost_default_a_removed_label_and_a_new_order_fail(self) -> None:
        found = "\n".join(swift_errors(swift_text("nofollow: Bool, url: String, rel: String?")))
        assert "Link: the Swift initializer labels of v1.0.0 are in another order" in found
        assert "Link.rel: the Swift argument has a default in v1.0.0 and is required now" in found
        assert "Link.marks: the Swift initializer argument of v1.0.0 is removed or renamed" in found

    def test_a_struct_with_no_initializer_at_the_head_fails(self) -> None:
        found = CHECK.compare_swift(facade(SWIFT_RELEASED), {}, TAG)
        assert found == ["Link: the Swift type of v1.0.0 has no public initializer at the head"]

    def test_a_changed_argument_type_fails(self) -> None:
        found = swift_errors(swift_text(SWIFT_RELEASED.replace("nofollow: Bool", "nofollow: String")))
        self.assert_fails(found, "Link.nofollow", "type changed from `Bool` to `String`")

    def test_a_changed_default_value_fails(self) -> None:
        found = swift_errors(swift_text(SWIFT_RELEASED.replace("rel: String? = nil", 'rel: String? = "nofollow"')))
        self.assert_fails(found, "Link.rel", 'default changed from `nil` to `"nofollow"`')

    def test_an_initializer_given_to_another_type_fails(self) -> None:
        head = SWIFT_NO_INITIALIZER + swift_text(SWIFT_RELEASED, owner="extension LinkType")
        self.assert_fails(swift_errors(head), "Link: the Swift type of v1.0.0 has no public initializer")

    def test_a_convenience_initializer_with_a_new_required_argument_fails(self) -> None:
        shape = {"declaration": "public convenience init", "owner": "public class Link"}
        released = swift_text(SWIFT_RELEASED, **shape)
        found = swift_errors(swift_text(SWIFT_RELEASED + ", originalUrl: String", **shape), released)
        self.assert_fails(found, "Link.originalUrl", "required")

    def test_an_inlinable_initializer_with_a_new_required_argument_fails(self) -> None:
        shape = {"declaration": "@inlinable public init"}
        released = swift_text(SWIFT_RELEASED, **shape)
        found = swift_errors(swift_text(SWIFT_RELEASED + ", originalUrl: String", **shape), released)
        self.assert_fails(found, "Link.originalUrl", "required")

    def test_an_initializer_in_an_extension_at_the_end_with_a_new_required_argument_fails(self) -> None:
        released = SWIFT_NO_INITIALIZER + swift_text(SWIFT_RELEASED, owner="extension Link")
        head = SWIFT_NO_INITIALIZER + swift_text(SWIFT_RELEASED + ", originalUrl: String", owner="extension Link")
        self.assert_fails(swift_errors(head, released), "Link.originalUrl", "required")

    def test_an_initializer_made_unavailable_fails(self) -> None:
        found = swift_errors(swift_text(SWIFT_RELEASED, declaration="@available(*, unavailable) public init"))
        self.assert_fails(found, "Link.url", "removed or renamed")

    def test_a_closure_argument_hides_no_later_required_argument(self) -> None:
        released = swift_text(SWIFT_RELEASED + ", accept: (String) -> Bool")
        head = swift_text(SWIFT_RELEASED + ", accept: (String) -> Bool, originalUrl: String")
        self.assert_fails(swift_errors(head, released), "Link.originalUrl", "required")

    def test_a_new_positional_argument_of_a_bridge_class_fails(self) -> None:
        head = BRIDGE_TEXT.replace("_ depth: UInt)", "_ depth: UInt, _ original_url: G)")
        self.assert_fails(swift_errors(head, BRIDGE_TEXT), "Box.original_url", "required")

    def test_a_gained_throws_fails(self) -> None:
        head = swift_text(SWIFT_RELEASED).replace(f"{SWIFT_RELEASED}) {{", f"{SWIFT_RELEASED}) throws {{")
        assert swift_errors(head) == ["Link: the Swift initializer of v1.0.0 is `init`, the head is `init throws`"]


SWIFT_TEXT = "public struct Link: Codable {\n    public init(url: String) {\n    }\n}\n"
SWIFT_MULTI_LINE = "public struct Link: Codable {\n    public init(\n        url: String) {\n    }\n}\n"
# ~keep Text that is not Swift at the top of the facade, as a merge or a hand edit leaves it.
SWIFT_NOT_SWIFT = "This file is generated; do not edit.\n" + SWIFT_TEXT
SWIFT_NO_INITIALIZER = "public struct Link: Codable {\n    public let url: String\n}\n"
BRIDGE_TEXT = (
    "public class Box: BoxRef {\n    public override init(ptr: UnsafeMutableRawPointer) {\n"
    "        super.init(ptr: ptr)\n    }\n}\n"
    "extension Box {\n    public convenience init<G: IntoRustString>(_ url: G, _ depth: UInt) {\n"
    "        self.init(ptr: __swift_bridge__$Box$new(url, depth))\n    }\n}\n"
)
STUB_NO_CONSTRUCTOR = "class Options:\n    wrap: bool\n"


def gate(*inputs: tuple[str, str | None, str | None]) -> tuple[int, str]:
    """Run the gate on `(source, released text, head text)` rows. Return its exit status and its output."""
    sources = {source.path: source for source in CHECK.SOURCES}
    rows = [CHECK.Input(sources[path], released, f"head/{path}", head) for path, released, head in inputs]
    output = io.StringIO()
    with contextlib.redirect_stdout(output):
        status = CHECK.run(TAG, rows)
    return status, output.getvalue()


class FailClosedTests(unittest.TestCase):
    def test_both_sources_print_their_counts_and_pass(self) -> None:
        status, output = gate((CHECK.STUB, stub(), stub()), (CHECK.SWIFT_FACADE, SWIFT_TEXT, SWIFT_TEXT))
        assert status == 0, output
        assert f"{CHECK.STUB}: v1.0.0 has 1 constructors, the head (head/{CHECK.STUB}) has 1, 1 kept" in output
        swift = CHECK.SWIFT_FACADE
        assert f"{swift}: v1.0.0 has 1 constructors, the head (head/{swift}) has 1, 1 kept" in output
        assert "ok: the 2 constructors of v1.0.0 are kept" in output

    def test_a_released_swift_facade_with_no_recognized_initializer_fails(self) -> None:
        # ~keep The head has a broken call. With no released initializer, no comparison names it.
        head = SWIFT_TEXT.replace("url: String", "url: String, originalUrl: String")
        status, output = gate((CHECK.STUB, stub(), stub()), (CHECK.SWIFT_FACADE, SWIFT_NO_INITIALIZER, head))
        assert status == 1, output
        assert f"::error::{CHECK.SWIFT_FACADE}: v1.0.0 has 0 constructors, the head" in output
        assert "has 1. The check does not pass" in output

    def test_a_head_swift_facade_with_no_recognized_initializer_fails(self) -> None:
        status, output = gate((CHECK.SWIFT_FACADE, SWIFT_TEXT, SWIFT_NO_INITIALIZER))
        assert status == 1, output
        assert "has 1 constructors, the head" in output
        assert "has 0, the first missing type is `Link`" in output

    def test_a_released_stub_with_no_recognized_constructor_fails(self) -> None:
        status, output = gate((CHECK.STUB, STUB_NO_CONSTRUCTOR, stub()), (CHECK.SWIFT_FACADE, SWIFT_TEXT, SWIFT_TEXT))
        assert status == 1, output
        assert f"::error::{CHECK.STUB}: v1.0.0 has 0 constructors, the head" in output

    def test_a_head_that_lost_a_released_type_fails(self) -> None:
        released = stub() + stub().replace("Options", "Extras")
        status, output = gate((CHECK.STUB, released, stub()))
        assert status == 1, output
        assert "v1.0.0 has 2 constructors, the head" in output
        assert "has 1, the first missing type is `Extras`" in output
        assert "has 1, 1 kept" in output

    def test_a_source_absent_at_the_release_is_named_and_passes(self) -> None:
        status, output = gate((CHECK.STUB, stub(), stub()), (CHECK.SWIFT_FACADE, None, SWIFT_TEXT))
        assert status == 0, output
        assert f"not compared: {CHECK.SWIFT_FACADE} absent at v1.0.0" in output.splitlines()
        assert "ok: the 1 constructors of v1.0.0 are kept" in output

    def test_no_source_at_the_release_fails(self) -> None:
        status, output = gate((CHECK.STUB, None, stub()), (CHECK.SWIFT_FACADE, None, SWIFT_TEXT))
        assert status == 1, output
        assert "nothing was compared" in output

    def test_the_stub_absent_at_the_release_fails_with_the_swift_facade_compared(self) -> None:
        status, output = gate((CHECK.STUB, None, stub()), (CHECK.SWIFT_FACADE, SWIFT_TEXT, SWIFT_TEXT))
        assert status == 1, output
        assert f"::error::the stub of v1.0.0 is missing: {CHECK.STUB}" in output.splitlines()
        assert "nothing was compared" not in output

    def test_the_stub_absent_at_the_head_fails(self) -> None:
        status, output = gate((CHECK.STUB, stub(), None), (CHECK.SWIFT_FACADE, SWIFT_TEXT, SWIFT_TEXT))
        assert status == 1, output
        assert f"::error::{CHECK.STUB} exists at v1.0.0 and head/{CHECK.STUB} does not exist" in output

    def test_no_source_given_fails(self) -> None:
        status, output = gate()
        assert status == 1, output
        assert "nothing was compared" in output

    def test_a_source_absent_at_the_head_fails(self) -> None:
        status, output = gate((CHECK.STUB, stub(), stub()), (CHECK.SWIFT_FACADE, SWIFT_TEXT, None))
        assert status == 1, output
        assert f"::error::{CHECK.SWIFT_FACADE} exists at v1.0.0 and head/{CHECK.SWIFT_FACADE} does not exist" in output

    def test_a_swift_source_that_is_not_swift_fails_with_the_file_and_the_line(self) -> None:
        status, output = gate((CHECK.SWIFT_FACADE, SWIFT_NOT_SWIFT, SWIFT_TEXT))
        assert status == 1, output
        assert f"::error::v1.0.0:{CHECK.SWIFT_FACADE}:1: parse error: text that the Swift grammar" in output

    def test_a_stub_that_is_not_python_fails_with_the_file_and_the_line(self) -> None:
        status, output = gate((CHECK.STUB, stub(), stub() + "class Broken(:\n"))
        assert status == 1, output
        assert f"::error::head/{CHECK.STUB}:6: parse error" in output


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

    def test_a_tag_that_git_does_not_know_is_an_error_not_an_absent_file(self) -> None:
        with self.assertRaises(subprocess.CalledProcessError):
            CHECK.released_stub("no-such-tag-of-this-repository")


if __name__ == "__main__":
    unittest.main()
