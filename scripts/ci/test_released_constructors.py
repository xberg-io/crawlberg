"""Ensure the released-constructor check fails every change that breaks a released call."""

from __future__ import annotations

import contextlib
import importlib.util
import io
import subprocess
import sys
import unittest
from pathlib import Path
from typing import TYPE_CHECKING
from unittest import mock

if TYPE_CHECKING:
    from collections.abc import Iterator

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


@contextlib.contextmanager
def expected_error[Error: Exception](kind: type[Error]) -> Iterator[list[Error]]:
    """Capture an expected parser error with no third-party test dependency."""
    caught: list[Error] = []
    try:
        yield caught
    except kind as error:
        caught.append(error)
    else:
        raise AssertionError(f"expected {kind.__name__}")


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
        # ~keep The key is the whole declaration: the signature with the defaults, the form, the
        # modifiers and the chain. Two initializers of one source never share it.
        memberwise_key = f"Link({SWIFT_RELEASED}) init public public struct Link: Codable"
        assert list(read) == [
            memberwise_key,
            "Link(from decoder: any Decoder) init throws public public struct Link: Codable",
        ]
        memberwise, decoder = read.values()
        assert [(p.label, p.type, p.default) for p in memberwise.parameters] == [
            ("url", "String", None),
            ("rel", "String?", "nil"),
            ("marks", "[String]", '[",", "("]'),
            ("nofollow", "Bool", None),
        ]
        assert memberwise.form == "init"
        assert decoder.form == "init throws"
        assert (memberwise.chain, memberwise.modifiers) == (("public struct Link: Codable",), "public")
        assert (decoder.parameters[0].label, decoder.parameters[0].name) == ("from", "decoder")

    def test_a_multi_line_initializer_is_read(self) -> None:
        assert list(CHECK.parse_swift(SWIFT_MULTI_LINE)) == [
            "Link(url: String) init public public struct Link: Codable"
        ]

    def test_a_positional_generic_initializer_of_a_bridge_class_is_read(self) -> None:
        read = CHECK.parse_swift(BRIDGE_TEXT)
        generic = "Box(_ url: G, _ depth: UInt) init<G: IntoRustString> public convenience extension Box"
        assert list(read) == [
            "Box(ptr: UnsafeMutableRawPointer) init public override public class Box: BoxRef",
            generic,
        ]
        assert [p.name for p in read[generic].parameters] == ["url", "depth"]

    def test_a_private_initializer_an_unavailable_one_and_a_protocol_requirement_are_not_constructors(self) -> None:
        text = (
            "public struct Link {\n    init(url: String) {}\n    @available(*, unavailable) public init(x: Int) {}\n}\n"
        )
        text += "public protocol P {\n    init(y: Int)\n}\n"
        assert CHECK.parse_swift(text) == {}

    def test_a_protocol_requirement_that_writes_public_is_not_a_constructor(self) -> None:
        # ~keep Swift rejects a modifier on a requirement, but the grammar reads it. The level of
        # the requirement is then `public`, and only the protocol around it hides it.
        text = "public protocol Q {\n    public init(y: Int)\n}\n"
        text += "public struct K {\n    public init(k: Int) {}\n}\n"
        assert list(CHECK.parse_swift(text)) == ["K(k: Int) init public public struct K"]

    def test_the_bridge_file_of_the_head_names_crawl_page_result(self) -> None:
        # ~keep swift-bridge writes `$` in identifiers; the grammar reads the file all the same.
        bridge = (ROOT / CHECK.SWIFT_BRIDGE).read_text(encoding="utf-8")
        read = CHECK.parse_swift(bridge, CHECK.SWIFT_BRIDGE)
        assert any(key.startswith("CrawlPageResult(_ url: GenericIntoRustString, ") for key in read), list(read)[:3]


class SwiftComparisonTests(unittest.TestCase):
    def assert_fails(self, found: list[str], *parts: str) -> None:
        assert any(all(part in error for part in parts) for error in found), found

    def test_a_new_defaulted_argument_without_a_compatibility_overload_fails(self) -> None:
        assert "function reference" in swift_errors(swift_text(SWIFT_RELEASED + ', originalUrl: String = ""'))[0]

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


def after_parameters(tail: str, *, declaration: str = "public init") -> str:
    """Return the released type with `tail` between `)` and the body of the memberwise initializer."""
    text = swift_text(SWIFT_RELEASED, declaration=declaration)
    return text.replace(f"{SWIFT_RELEASED}) {{", f"{SWIFT_RELEASED}) {tail} {{")


def bridge_errors(old: str, new: str) -> list[str]:
    """Compare the bridge file of the head with a copy of it where `old`, once in the file, is `new`."""
    bridge = (ROOT / CHECK.SWIFT_BRIDGE).read_text(encoding="utf-8")
    assert bridge.count(old) == 1, old
    return CHECK.compare_swift(CHECK.parse_swift(bridge), CHECK.parse_swift(bridge.replace(old, new)), TAG)


class WholeDeclarationTests(unittest.TestCase):
    """Every row of the third review that exited 0, and the two controls of the maintainer, fail."""

    def assert_fails(self, found: list[str], *parts: str) -> None:
        assert any(all(part in error for part in parts) for error in found), found

    def test_n09_typed_throws_fails(self) -> None:
        found = swift_errors(after_parameters("throws(CrawlError)"))
        assert found == ["Link: the Swift initializer of v1.0.0 is `init`, the head is `init throws(CrawlError)`"]

    def test_n12_an_owner_made_internal_fails(self) -> None:
        # ~keep The owner is the ceiling of the initializer's visibility: the head has no public one.
        found = swift_errors(swift_text(SWIFT_RELEASED, owner="struct Link: Codable"))
        assert found == ["Link: the Swift type of v1.0.0 has no public initializer at the head"]

    def test_n13_a_private_owner_fails(self) -> None:
        found = swift_errors(swift_text(SWIFT_RELEASED, owner="private struct Link: Codable"))
        assert found == ["Link: the Swift type of v1.0.0 has no public initializer at the head"]

    def test_n14_an_initializer_moved_to_a_fileprivate_extension_fails(self) -> None:
        head = SWIFT_NO_INITIALIZER + swift_text(SWIFT_RELEASED, owner="fileprivate extension Link")
        assert swift_errors(head) == ["Link: the Swift type of v1.0.0 has no public initializer at the head"]

    def test_n15_a_gained_generic_clause_fails(self) -> None:
        found = swift_errors(swift_text(SWIFT_RELEASED, declaration="public init<T: Sendable>"))
        assert found == ["Link: the Swift initializer of v1.0.0 is `init`, the head is `init<T: Sendable>`"]

    def test_n16_a_changed_generic_constraint_of_the_bridge_fails(self) -> None:
        found = bridge_errors(BRIDGE_LINK_INFO, BRIDGE_LINK_INFO.replace(": IntoRustString>", ": FixedWidthInteger>"))
        self.assert_fails(
            found, "LinkInfo: the Swift initializer of v1.0.0 is `init<GenericIntoRustString: IntoRustString>`"
        )
        self.assert_fails(found, "the head is `init<GenericIntoRustString: FixedWidthInteger>`")

    def test_n17_a_gained_where_clause_fails(self) -> None:
        head = BRIDGE_TEXT.replace("_ depth: UInt) {", "_ depth: UInt) where G: FixedWidthInteger {")
        found = swift_errors(head, BRIDGE_TEXT)
        self.assert_fails(found, "Box: the Swift initializer of v1.0.0 is `init<G: IntoRustString>`")
        self.assert_fails(found, "the head is `init<G: IntoRustString> where G: FixedWidthInteger`")

    def test_n20_an_owner_moved_into_a_namespace_fails(self) -> None:
        head = "public enum Models {\n" + swift_text(SWIFT_RELEASED) + "}\n"
        assert all(key.startswith("Models.Link(") for key in CHECK.parse_swift(head))
        assert swift_errors(head) == ["Link: the Swift type of v1.0.0 has no public initializer at the head"]

    def test_n22_a_conditional_compilation_block_around_the_initializer_fails(self) -> None:
        head = swift_text(SWIFT_RELEASED).replace("    public init(", "#if os(Linux)\n    public init(", 1)
        head = head.replace(f"    }}\n    {DECODER}", f"    }}\n#endif\n    {DECODER}")
        found = swift_errors(head)
        assert found == ["Link: around the Swift initializer v1.0.0 has `no declaration`, the head has `#if os(Linux)`"]

    def test_n22_a_conditional_compilation_block_around_the_owner_fails(self) -> None:
        found = swift_errors("#if os(Linux)\n" + swift_text(SWIFT_RELEASED) + "#endif\n")
        self.assert_fails(
            found,
            "Link: around the Swift initializer v1.0.0 has `public struct Link: Codable`, the head has `#if os(Linux)`",
        )

    def test_n28_two_positional_arguments_of_one_type_swapped_fail(self) -> None:
        found = bridge_errors(
            BRIDGE_BATCH,
            BRIDGE_BATCH.replace(
                "_ total_count: UInt, _ completed_count: UInt", "_ completed_count: UInt, _ total_count: UInt"
            ),
        )
        self.assert_fails(
            found,
            "BatchCrawlResults.total_count: the Swift argument name changed from `total_count` to `completed_count`",
        )
        self.assert_fails(
            found,
            "BatchCrawlResults.completed_count: the Swift argument name changed from `completed_count` to `total_count`",
        )

    def test_n29_a_gained_main_actor_attribute_fails(self) -> None:
        found = swift_errors(swift_text(SWIFT_RELEASED, declaration="@MainActor public init"))
        assert found == [
            "Link: the Swift initializer of v1.0.0 has the modifiers `public`, the head has `@MainActor public`"
        ]

    def test_n30_a_gained_availability_attribute_fails(self) -> None:
        found = swift_errors(swift_text(SWIFT_RELEASED, declaration="@available(macOS 99, *) public init"))
        self.assert_fails(
            found,
            "Link: the Swift initializer of v1.0.0 has the modifiers `public`",
            "`@available(macOS 99, *) public`",
        )

    def test_n31_unavailable_inside_a_message_is_not_unavailable(self) -> None:
        declaration = '@available(*, deprecated, message: "unavailable in 2.0") public init'
        released = swift_text(SWIFT_RELEASED, declaration=declaration)
        assert len(CHECK.parse_swift(released)) == 2
        found = swift_errors(swift_text(SWIFT_RELEASED + ", originalUrl: String", declaration=declaration), released)
        assert found == ["Link.originalUrl: the new Swift initializer argument is required: no call of v1.0.0 compiles"]

    def test_n33_a_positional_argument_removed_and_one_appended_fails(self) -> None:
        head = BRIDGE_BATCH.replace(
            "_ completed_count: UInt, _ failed_count: UInt)", "_ failed_count: UInt, _ extra: UInt = 0)"
        )
        found = bridge_errors(BRIDGE_BATCH, head)
        self.assert_fails(
            found,
            "BatchCrawlResults.completed_count: the Swift argument name changed from `completed_count` to `failed_count`",
        )

    def test_the_maintainer_control_of_a_narrowed_generic_constraint_fails(self) -> None:
        found = bridge_errors(
            BRIDGE_LINK_INFO, BRIDGE_LINK_INFO.replace(": IntoRustString>", ": IntoRustString & Equatable>")
        )
        self.assert_fails(
            found, "LinkInfo: the Swift initializer of v1.0.0 is `init<GenericIntoRustString: IntoRustString>`"
        )
        self.assert_fails(found, "the head is `init<GenericIntoRustString: IntoRustString & Equatable>`")

    def test_the_maintainer_control_of_an_implicitly_unwrapped_initializer_fails(self) -> None:
        found = bridge_errors(BRIDGE_LINK_INFO, BRIDGE_LINK_INFO.replace("convenience init<", "convenience init!<"))
        self.assert_fails(
            found, "LinkInfo: the Swift initializer of v1.0.0 is `init<GenericIntoRustString: IntoRustString>`"
        )
        self.assert_fails(found, "the head is `init!<GenericIntoRustString: IntoRustString>`")

    def test_an_unknown_node_kind_under_the_initializer_fails_with_the_kind(self) -> None:
        with (
            mock.patch.object(CHECK, "INITIALIZER_HEAD", CHECK.INITIALIZER_HEAD - {"type_parameters"}),
            expected_error(SyntaxError) as raised,
        ):
            CHECK.parse_swift(BRIDGE_TEXT)
        assert raised[0].msg == "a `type_parameters` node of the initializer that the reader does not classify"
        assert raised[0].lineno == 7

    def test_an_unknown_node_kind_around_the_initializer_fails_with_the_kind(self) -> None:
        # ~keep A type inside a function is internal, so the reader never reaches its parent: the
        # arm is reached with a body kind that the reader does not know.
        with (
            mock.patch.object(CHECK, "BODIES", CHECK.BODIES - {"enum_class_body"}),
            expected_error(SyntaxError) as raised,
        ):
            CHECK.parse_swift("public enum Link {\n    public init(url: String) {}\n}\n")
        assert raised[0].msg == "a `enum_class_body` node around the initializer that the reader does not classify"
        assert raised[0].lineno == 1

    def test_a_type_inside_a_function_is_not_public(self) -> None:
        local = "func make() {\n    struct Link {\n        public init(url: String) {}\n    }\n}\n"
        assert CHECK.parse_swift(local) == {}

    def test_an_added_argument_with_a_default_before_a_positional_argument_fails(self) -> None:
        head = BRIDGE_TEXT.replace("(_ url: G, _ depth: UInt)", "(_ url: G, limit: UInt = 0, _ depth: UInt)")
        found = swift_errors(head, BRIDGE_TEXT)
        assert found == [
            "Box.limit: the new Swift initializer argument comes before the positional argument `depth` of v1.0.0: a positional call of v1.0.0 binds another value"
        ]

    def test_an_added_defaulted_labelled_argument_changes_the_function_reference(self) -> None:
        assert "function reference" in swift_errors(swift_text("extra: Int = 0, " + SWIFT_RELEASED))[0]

    def test_a_renamed_internal_name_fails(self) -> None:
        found = swift_errors(swift_text(SWIFT_RELEASED.replace("url: String", "url u: String")))
        assert found == ["Link.url: the Swift argument name changed from `url` to `u`"]

    def test_a_gained_default_passes(self) -> None:
        assert swift_errors(swift_text(SWIFT_RELEASED.replace("nofollow: Bool", "nofollow: Bool = false"))) == []

    def test_comments_and_whitespace_are_not_compared(self) -> None:
        parameters = SWIFT_RELEASED.replace("url: String,", "url:   String, /* c */")
        head = f"public struct Link /* c */ : Codable {{\n    public /* c */ init({parameters} // c\n    ) {{\n    }}\n    {DECODER}\n}}\n"
        assert swift_errors(head) == []

    def test_the_sources_name_the_stub_and_the_three_swift_files(self) -> None:
        paths = [source.path for source in CHECK.SOURCES]
        assert paths == [CHECK.STUB, CHECK.SWIFT_FACADE, CHECK.SWIFT_BRIDGE, CHECK.SWIFT_BRIDGE_CORE]


M1_RELEASED = "public struct Box {\n    public init(a: Int) {}\n}\npublic extension Box {\n    init(b: String) {}\n}\n"
M2_PAIR = "public class Box {\n    public init<G>(_ value: G) {}\n    public init<G: Equatable>(_ value: G) {}\n}\n"
M2_TWINS = (
    "public class Box<T> {\n    public init(_ value: Int) {}\n    public init(_ value: Int) async {}\n}\n"
    "extension Box where T: Equatable {\n    public init(_ v: T) {}\n}\n"
    "extension Box where T: Hashable {\n    public init(_ v: T) {}\n}\n"
)


class SixRowTests(unittest.TestCase):
    """The two controls of the seventh maintainer review (M1, M2) and the rows A22 to A25 of the fourth review."""

    def test_m1_an_initializer_of_a_public_extension_is_public_and_its_removal_fails(self) -> None:
        read = CHECK.parse_swift(M1_RELEASED)
        assert [(i.owner, i.modifiers, i.chain) for i in read.values()] == [
            ("Box", "public", ("public struct Box",)),
            ("Box", "", ("public extension Box",)),
        ]
        head = "public struct Box {\n    public init(a: Int) {}\n}\n"
        found = swift_errors(head, M1_RELEASED)
        assert (
            found[0]
            == "Box: around the Swift initializer v1.0.0 has `public extension Box`, the head has `public struct Box`"
        )
        assert "Box.b: the Swift initializer argument of v1.0.0 is removed or renamed" in found
        status, output = gate((CHECK.SWIFT_FACADE, M1_RELEASED, head))
        assert status == 1, output
        assert (
            f"{CHECK.SWIFT_FACADE}: v1.0.0 has 2 constructors, the head (head/{CHECK.SWIFT_FACADE}) has 1, 1 kept"
            in output
        )

    def test_m1_the_effective_visibility_is_the_initializer_then_the_extension_under_every_enclosing_declaration(
        self,
    ) -> None:
        hidden = (
            "private extension Box {\n    public init(b: String) {}\n}\n",
            "public extension Box {\n    internal init(b: String) {}\n}\n",
            "extension Box {\n    init(b: String) {}\n}\n",
            "struct Box {\n    public init(b: String) {}\n}\n",
            "public struct Outer {\n    struct Box {\n        public init(b: String) {}\n    }\n}\n",
            "public extension Outer {\n    struct Box {\n        init(b: String) {}\n    }\n}\n",
            "public protocol P {\n    init(b: String)\n}\n",
        )
        for text in hidden:
            assert CHECK.parse_swift(text) == {}, text
        public = (
            "extension Box {\n    public init(b: String) {}\n}\n",
            "public extension Box {\n    init(b: String) {}\n}\n",
            "open class Box {\n    public init(b: String) {}\n}\n",
            "public struct Outer {\n    public struct Box {\n        public init(b: String) {}\n    }\n}\n",
            "public extension Outer {\n    struct Box {\n        public init(b: String) {}\n    }\n}\n",
            "@objc extension Box {\n    public init(b: String) {}\n}\n",
        )
        for text in public:
            assert len(CHECK.parse_swift(text)) == 1, text

    def test_m2_two_overloads_with_equal_parameters_are_both_read_and_the_removal_of_either_fails(self) -> None:
        assert [i.form for i in CHECK.parse_swift(M2_PAIR).values()] == ["init<G>", "init<G: Equatable>"]
        head = M2_PAIR.replace("    public init<G>(_ value: G) {}\n", "")
        assert swift_errors(head, M2_PAIR) == [
            "Box: the Swift initializer of v1.0.0 is `init<G>`, the head is `init<G: Equatable>`"
        ]
        head = M2_PAIR.replace("    public init<G: Equatable>(_ value: G) {}\n", "")
        assert swift_errors(head, M2_PAIR) == [
            "Box: the Swift initializer of v1.0.0 is `init<G: Equatable>`, the head is `init<G>`"
        ]

    def test_m2_an_async_twin_and_two_constrained_extensions_are_kept_apart(self) -> None:
        assert len(CHECK.parse_swift(M2_TWINS)) == 4
        head = M2_TWINS.replace("    public init(_ value: Int) async {}\n", "")
        assert swift_errors(head, M2_TWINS) == [
            "Box: the Swift initializer of v1.0.0 is `init async`, the head is `init`"
        ]
        head = M2_TWINS.replace("extension Box where T: Hashable {\n    public init(_ v: T) {}\n}\n", "")
        expected = "Box: around the Swift initializer v1.0.0 has `extension Box where T: Hashable`, the head has `extension Box where T: Equatable`"
        assert swift_errors(head, M2_TWINS) == [expected]

    def test_m2_one_declaration_twice_fails_the_parse(self) -> None:
        with expected_error(SyntaxError) as raised:
            CHECK.parse_swift("public struct Box {\n    public init(a: Int) {}\n    public init(a: Int) {}\n}\n")
        assert raised[0].msg == "a second declaration of `Box(a: Int) init public public struct Box`"
        assert raised[0].lineno == 3

    def test_m2_a_removed_initializer_reference_requires_the_exact_arity(self) -> None:
        two = "public struct Box {\n    public init(a: Int) {}\n    public init(a: Int, b: Int = 0) {}\n}\n"
        assert "function reference" in swift_errors(two.replace("    public init(a: Int) {}\n", ""), two)[0]
        head = two.replace("    public init(a: Int, b: Int = 0) {}\n", "")
        assert swift_errors(head, two) == ["Box.b: the Swift initializer argument of v1.0.0 is removed or renamed"]

    def test_a22_an_added_defaulted_closure_before_a_released_defaulted_closure_fails(self) -> None:
        released = swift_text(SWIFT_RELEASED + ", onDone: (() -> Void)? = nil")
        head = swift_text(SWIFT_RELEASED + ", onStart: (() -> Void)? = nil, onDone: (() -> Void)? = nil")
        assert swift_errors(head, released) == [
            (
                "Link.onStart: the new Swift initializer argument comes before the argument `onDone` of v1.0.0, "
                "which has a function type: a trailing closure of v1.0.0 binds another parameter"
            )
        ]

    def test_a22_an_added_defaulted_closure_at_the_end_fails(self) -> None:
        found = swift_errors(swift_text(SWIFT_RELEASED + ", onDone: (() -> Void)? = nil"))
        assert found == [
            "Link.onDone: the new Swift initializer argument has a function type: a trailing closure of v1.0.0 binds it"
        ]

    def test_a22_an_added_defaulted_argument_before_a_released_closure_fails_and_after_it_passes(self) -> None:
        released = swift_text(SWIFT_RELEASED + ", accept: (String) -> Bool")
        head = swift_text(SWIFT_RELEASED + ", extra: Int = 0, accept: (String) -> Bool")
        assert len(swift_errors(head, released)) == 1
        assert (
            "Link.extra: the new Swift initializer argument comes before the argument `accept` of v1.0.0"
            in swift_errors(head, released)[0]
        )
        head = swift_text(SWIFT_RELEASED + ", accept: (String) -> Bool, extra: Int = 0")
        assert "function reference" in swift_errors(head, released)[0]

    def test_a24_a_dollar_is_compared_as_the_file_writes_it(self) -> None:
        released = swift_text(SWIFT_RELEASED.replace("rel: String? = nil", 'rel: String? = "$"'))
        head = swift_text(SWIFT_RELEASED.replace("rel: String? = nil", 'rel: String? = "_"'))
        assert swift_errors(head, released) == ['Link.rel: the Swift argument default changed from `"$"` to `"_"`']
        read = CHECK.parse_swift("public class CbWrapper$crawl {\n    public init(cb: Wrapper$Result) {}\n}\n")
        assert [(i.owner, i.parameters[0].type, i.chain) for i in read.values()] == [
            ("CbWrapper$crawl", "Wrapper$Result", ("public class CbWrapper$crawl",))
        ]

    def test_a25_a_platform_unavailable_initializer_stays_a_released_call(self) -> None:
        declaration = "@available(macOS, unavailable) public init"
        released = swift_text(SWIFT_RELEASED, declaration=declaration)
        assert [i.modifiers for i in CHECK.parse_swift(released).values()] == [
            "@available(macOS, unavailable) public",
            "public",
        ]
        head = swift_text(SWIFT_RELEASED + ", originalUrl: String", declaration=declaration)
        assert swift_errors(head, released) == [
            "Link.originalUrl: the new Swift initializer argument is required: no call of v1.0.0 compiles"
        ]
        head = swift_text(SWIFT_RELEASED, declaration="@available(iOS, unavailable) public init")
        self.assert_fails(
            swift_errors(head, released),
            "has the modifiers `@available(macOS, unavailable) public`",
            "`@available(iOS, unavailable) public`",
        )

    def test_a25_an_initializer_unavailable_everywhere_is_not_a_constructor(self) -> None:
        read = CHECK.parse_swift(
            swift_text(SWIFT_RELEASED, declaration='@available(*, unavailable, renamed: "x") public init')
        )
        assert [i.form for i in read.values()] == ["init throws"]

    def assert_fails(self, found: list[str], *parts: str) -> None:
        assert any(all(part in error for part in parts) for error in found), found


class FailClosedBranchTests(unittest.TestCase):
    """Each arm that fails the parse is reached by an input, or by a kind set made smaller."""

    def assert_parse_fails(self, text: str, message: str, line: int) -> None:
        with expected_error(SyntaxError) as raised:
            CHECK.parse_swift(text)
        assert (raised[0].msg, raised[0].lineno) == (message, line)

    def test_a_child_of_an_enclosing_declaration_of_another_kind_fails_with_the_kind(self) -> None:
        text = "public indirect enum E {\n    public init(x: Int) {}\n}\n"
        self.assert_parse_fails(text, "a `indirect` node around the initializer that the reader does not classify", 1)

    def test_a_directive_that_opens_no_block_fails_with_its_token(self) -> None:
        text = "public struct L {\n#else\n    public init(x: Int) {}\n}\n"
        self.assert_parse_fails(text, "a `#else` directive before the initializer that the reader does not classify", 2)

    def test_a_modifiers_child_of_another_kind_fails_with_the_kind(self) -> None:
        with mock.patch.object(CHECK, "MODIFIERS", CHECK.MODIFIERS - {"member_modifier"}):
            self.assert_parse_fails(
                BRIDGE_TEXT, "a `member_modifier` node of the modifiers that the reader does not classify", 2
            )

    def test_a_parameter_child_of_another_kind_fails_with_the_kind(self) -> None:
        text = swift_text(SWIFT_RELEASED.replace("nofollow: Bool", "nofollow: @escaping @Sendable () -> Bool"))
        self.assert_parse_fails(text, "a `type_modifiers` node of a parameter that the reader does not classify", 2)

    def test_a_parameter_without_a_type_is_a_parse_error_before_the_read(self) -> None:
        # ~keep The grammar gives every `parameter` a name and a type; without one the file fails
        # before any initializer is read, so the reader has no arm for it.
        for parameters in ("x:", "x", "x: , y: Int"):
            text = f"public struct L {{\n    public init({parameters}) {{}}\n}}\n"
            self.assert_parse_fails(text, "text that the Swift grammar does not read", 2)

    def test_an_initializer_outside_every_type_fails(self) -> None:
        self.assert_parse_fails("public init(x: Int) {}\n", "an initializer that no type declares", 1)

    def test_two_initializers_under_different_directives_are_both_read(self) -> None:
        # ~keep The chain is part of the key: the `#if` and the `#else` arm hold one declaration each.
        text = "public struct L {\n#if os(Linux)\n    public init(x: Int) {}\n#else\n    public init(x: Int) {}\n#endif\n}\n"
        assert [i.chain for i in CHECK.parse_swift(text).values()] == [
            ("public struct L", "#if os(Linux)"),
            ("public struct L", "#if os(Linux) #else"),
        ]

    def test_a_comment_inside_a_default_and_a_type_is_not_compared(self) -> None:
        released = swift_text(SWIFT_RELEASED + ', x: [String] = [",", "("], y: Dictionary<String, Int> = [:]')
        head = swift_text(
            SWIFT_RELEASED + ', x: [String] = [",", /* c */ "("], y: Dictionary<String, /* c */ Int> = [:]'
        )
        assert swift_errors(head, released) == []


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
# ~keep Two lines of the generated bridge file of the head, byte-equal to v1.10.3.
BRIDGE_LINK_INFO = (
    "public convenience init<GenericIntoRustString: IntoRustString>(_ url: GenericIntoRustString, "
    "_ text: GenericIntoRustString, _ link_type: LinkType, _ rel: Optional<GenericIntoRustString>, _ nofollow: Bool) {"
)
BRIDGE_BATCH = (
    "public convenience init(_ results: RustVec<BatchCrawlResult>, _ total_count: UInt, "
    "_ completed_count: UInt, _ failed_count: UInt) {"
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


class SwiftFactoryTests(unittest.TestCase):
    @staticmethod
    def source(signature: str, owner: str = "public class Box") -> str:
        return f"{owner} {{ public init() {{}}; {signature} {{ fatalError() }} }}"

    def test_the_exception_assertion_rejects_a_nonfailing_parse(self) -> None:
        with expected_error(AssertionError) as raised, expected_error(SyntaxError):
            CHECK.parse_swift(self.source("public static func make() -> Self"))
        assert str(raised[0]) == "expected SyntaxError"

    def test_a_required_factory_argument_fails_the_gate(self) -> None:
        released = self.source("public static func make(_ value: Int) -> Self")
        head = self.source("public static func make(_ value: Int, _ required: Int) -> Self")
        status, output = gate((CHECK.SWIFT_FACADE, released, head))
        assert status == 1, output
        assert "factory" in output and "required" in output, output

    def test_factory_declarations_and_overloads_are_kept(self) -> None:
        for modifier in ("static", "class"):
            for result in ("Self", "Box"):
                with self.subTest(modifier=modifier, result=result):
                    source = self.source(f"public {modifier} func make(_ value: Int) -> {result}")
                    factories = CHECK.parse_swift_factories(source)
                    assert len(factories) == 1, factories
                    assert CHECK.compare_swift(factories, factories, TAG) == []
        source = self.source(
            "public static func make(_ value: Int) -> Self {}; public static func make(_ value: String) -> Self"
        )
        assert len(CHECK.parse_swift_factories(source)) == 2

    def test_factory_breaking_changes_fail(self) -> None:
        released = self.source("public static func make<T>(_ value: T, extra: Int = 1) -> Self")
        changes = (
            "public static func make<T: Equatable>(_ value: T, extra: Int = 1) -> Self",
            "public static func make<T>(_ value: T, extra: Int = 1) -> Self where T: Equatable",
            "public static func make<T>(_ value: T, extra: Int = 1) async -> Self",
            "public static func make<T>(_ value: T, extra: Int = 1) throws -> Self",
            "public class func make<T>(_ value: T, extra: Int = 1) -> Self",
            "public static func make<T>(_ value: T, extra: Int = 2) -> Self",
            "public static func make<T>(_ value: T, extra: Int) -> Self",
            "public static func make<T>(_ value: String, extra: Int = 1) -> Self",
            "public static func renamed<T>(_ value: T, extra: Int = 1) -> Self",
            "private static func make<T>(_ value: T, extra: Int = 1) -> Self",
            "public static func make<T>(_ value: T, extra: Int = 1) -> String",
        )
        for changed in changes:
            with self.subTest(changed=changed):
                found = CHECK.compare_swift(
                    CHECK.parse_swift_factories(released), CHECK.parse_swift_factories(self.source(changed)), TAG
                )
                assert found, changed

    def test_factory_owner_visibility_and_extension_are_checked(self) -> None:
        source = "public class Box { public init() {} }; public extension Box { static func make() -> Self { fatalError() } }"
        assert len(CHECK.parse_swift_factories(source)) == 1
        assert (
            CHECK.parse_swift_factories(
                source.replace("public class Box", "internal class Box").replace(
                    "public extension", "private extension"
                )
            )
            == {}
        )
        assert CHECK.parse_swift_factories(self.source("public static func make() -> Self", "internal class Box")) == {}
        assert CHECK.parse_swift_factories(self.source("public func make() -> Self")) == {}
        assert (
            CHECK.parse_swift_factories(self.source("@available(*, unavailable) public static func make() -> Self"))
            == {}
        )

    def test_factory_whole_owner_declarations_are_checked(self) -> None:
        released = (
            "public struct Outer { public struct Box { public static func make() -> Outer.Box { fatalError() } } }"
        )
        factories = CHECK.parse_swift_factories(released)
        assert len(factories) == 1
        assert next(iter(factories.values())).owner == "Outer.Box"
        for head in (
            released.replace("public struct Outer", "public struct Outer: Equatable"),
            released.replace("static func", "@MainActor static func"),
            "#if FLAG\n" + released + "\n#endif",
            released.replace("struct Box", "struct Box<T>"),
        ):
            with self.subTest(head=head):
                assert CHECK.compare_swift(factories, CHECK.parse_swift_factories(head), TAG)

    def test_factory_generic_owner_and_duplicate_declarations_are_read(self) -> None:
        source = self.source("public static func make<T>(_ value: T) -> Box<T>", "public class Box<T>")
        assert len(CHECK.parse_swift_factories(source)) == 1
        duplicate = self.source("public static func make() -> Self {}; public static func make() -> Self")
        with expected_error(SyntaxError) as raised:
            CHECK.parse_swift_factories(duplicate)
        assert "second declaration" in str(raised[0]), raised[0]

    def test_an_added_default_factory_parameter_changes_the_function_reference(self) -> None:
        released = self.source("public static func make(_ value: Int) -> Self")
        head = self.source("public static func make(_ value: Int, extra: Int = 1) -> Self")
        assert (
            "function reference"
            in CHECK.compare_swift(CHECK.parse_swift_factories(released), CHECK.parse_swift_factories(head), TAG)[0]
        )

    def test_a_removed_factory_overload_fails(self) -> None:
        released = self.source(
            "public static func make(_ value: Int) -> Self {}; public static func make(_ value: String) -> Self"
        )
        head = self.source("public static func make(_ value: Int) -> Self")
        assert CHECK.compare_swift(CHECK.parse_swift_factories(released), CHECK.parse_swift_factories(head), TAG)

    def test_zero_released_factories_pass_without_relaxing_constructor_counts(self) -> None:
        released = self.source("public func ordinary() -> Int")
        head = self.source("public static func make() -> Self")
        status, output = gate((CHECK.SWIFT_FACADE, released, head))
        assert status == 0, output
        assert "0 factories" in output and "has 1, 0 kept" in output, output
        status, output = gate((CHECK.SWIFT_FACADE, "public class Box {}", head))
        assert status == 1, output
        assert "0 constructors" in output, output


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
        with expected_error(subprocess.CalledProcessError) as raised:
            CHECK.released_stub("no-such-tag-of-this-repository")
        assert raised[0].returncode != 0


def load_tests(loader: unittest.TestLoader, suite: unittest.TestSuite, _pattern: str | None) -> unittest.TestSuite:
    # ~keep CI executes this file directly, so the split shape cases must join its canonical suite.
    from test_released_constructor_shapes import (
        SwiftAliasScopeTests,
        SwiftFactoryShapeTests,
        SwiftPropertyEffectTests,
        SwiftReferenceCompatibilityTests,
    )

    for cases in (
        SwiftAliasScopeTests,
        SwiftFactoryShapeTests,
        SwiftPropertyEffectTests,
        SwiftReferenceCompatibilityTests,
    ):
        suite.addTests(loader.loadTestsFromTestCase(cases))
    return suite


if __name__ == "__main__":
    unittest.main()
