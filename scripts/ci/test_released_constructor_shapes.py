"""Regression coverage for released Swift result shapes and function references."""

import contextlib
import unittest
from collections.abc import Iterator

import check_released_constructors as check

TAG = "v1.0.0"


@contextlib.contextmanager
def expected_syntax_error() -> Iterator[None]:
    try:
        yield
    except SyntaxError:
        return
    raise AssertionError("expected SyntaxError")


class SwiftFactoryShapeTests(unittest.TestCase):
    def test_wrapped_and_aliased_results_are_compared(self) -> None:
        for result in ("Box?", "Optional<Box>", "[Box]", "Result"):
            source = (
                "public typealias Result = Box\npublic class Box { public init() {}; "
                f"public static func make(_ value: Int) -> {result} {{ fatalError() }} }}"
            )
            with self.subTest(result=result):
                released = check.parse_swift_factories(source)
                assert len(released) == 1, released
                changed = source.replace("_ value: Int)", "_ value: Int, _ required: Int)")
                assert check.compare_swift(released, check.parse_swift_factories(changed), TAG)

    def test_static_property_removal_fails(self) -> None:
        source = "public class Box { public init() {}; public static var shared: Box { fatalError() } }"
        read = check.parse_swift_factories(source)
        assert len(read) == 1, read
        assert check.compare_swift(read, {}, TAG)

    def test_self_in_a_vector_is_not_the_owner_result(self) -> None:
        source = "public extension UInt { static func vecOfSelfLen(vecPtr: Int) -> RustVec<Self> { fatalError() } }"
        assert check.parse_swift_factories(source) == {}
        runtime = "extension UInt: Vectorizable { public static func vecOfSelfLen(vecPtr: Int) -> UInt {} }"
        assert check.parse_swift_factories(runtime, check.SWIFT_BRIDGE_CORE) == {}
        assert len(check.parse_swift_factories(runtime)) == 1

    def test_public_extension_cannot_export_an_internal_owner(self) -> None:
        source = "internal class Box {}; public extension Box { init() {}; static func make() -> Self {} }"
        assert check.parse_swift(source) == {}
        assert check.parse_swift_factories(source) == {}

    def test_function_type_alias_is_resolved_for_trailing_closures(self) -> None:
        source = "public typealias Handler = () -> Void\npublic class Box { public init(done: Handler) {} }"
        read = check.parse_swift(source)
        assert next(iter(read.values())).parameters[0].closure is True

    def test_generic_parameter_shadows_a_function_type_alias(self) -> None:
        source = "typealias Handler = () -> Void\npublic class Box { public init<Handler>(done: Handler) {} }"
        assert next(iter(check.parse_swift(source).values())).parameters[0].closure is False

    def test_factory_name_result_and_key_filters_have_negative_controls(self) -> None:
        source = (
            "public class Box { public static func make(_ value: Int) -> Self {}; "
            "public static func other(_ value: Int) -> Self {} }"
        )
        read = check.parse_swift_factories(source)
        assert len(read) == 2, read
        removed = source.replace("public static func make(_ value: Int) -> Self {}; ", "")
        assert check.compare_swift(read, check.parse_swift_factories(removed), TAG)
        ordinary = "public class Box { public static func count() -> Int {} }"
        assert check.parse_swift_factories(ordinary) == {}


class SwiftReferenceCompatibilityTests(unittest.TestCase):
    def test_an_exact_compatibility_overload_preserves_released_references(self) -> None:
        released = "public class Box { public init(value: Int) {}; public static func make(value: Int) -> Self {} }"
        added = released.replace("public init(value: Int)", "public init(value: Int, extra: Int = 0)").replace(
            "make(value: Int)", "make(value: Int, extra: Int = 0)"
        )
        head = (released[:-1] + "\n" + added.split("{", 1)[1]).replace(";", "\n")
        assert check.compare_swift(check.parse_swift(released), check.parse_swift(head), TAG) == []
        assert check.compare_swift(check.parse_swift_factories(released), check.parse_swift_factories(head), TAG) == []

    def test_a_new_required_positional_parameter_is_named_instead_of_a_false_reorder(self) -> None:
        released = "public class Box { public static func make(_ value: Int) -> Self {} }"
        head = released.replace("_ value: Int)", "_ value: Int, _ required: Int)")
        errors = check.compare_swift(check.parse_swift_factories(released), check.parse_swift_factories(head), TAG)
        assert len(errors) == 1, errors
        assert "required: the new Swift" in errors[0], errors


class SwiftAliasScopeTests(unittest.TestCase):
    def test_nested_aliases_do_not_change_a_sibling_parameter(self) -> None:
        source = (
            "typealias Handler = () -> Void\n"
            "public struct One { typealias Handler = Int; public init(done: Handler) {} }\n"
            "public struct Two { public init(done: Handler) {} }"
        )
        read = list(check.parse_swift(source).values())
        assert [item.parameters[0].closure for item in read] == [False, True]

    def test_qualified_extension_cannot_export_nested_internal_type(self) -> None:
        source = "public struct Outer { internal class Box {} }; public extension Outer.Box { init() {} }"
        assert check.parse_swift(source) == {}

    def test_optional_unrelated_property_is_not_a_factory(self) -> None:
        source = "public class Box { public static var count: Optional<Int> { nil } }"
        assert check.parse_swift_factories(source) == {}

    def test_recursive_alias_fails_closed(self) -> None:
        source = "typealias Alias = Optional<Alias>\npublic struct Box { public init(value: Alias) {} }"
        with expected_syntax_error():
            check.parse_swift(source)


class SwiftPropertyEffectTests(unittest.TestCase):
    @staticmethod
    def source(effect: str) -> str:
        return f"public class Box {{ public static var shared: Box {{ get {effect} {{ fatalError() }} }} }}"

    def test_adding_async_to_a_getter_requires_a_new_call(self) -> None:
        released = check.parse_swift_factories(self.source(""))
        assert check.compare_swift(released, check.parse_swift_factories(self.source("async")), TAG)

    def test_adding_throws_to_a_getter_requires_a_new_call(self) -> None:
        released = check.parse_swift_factories(self.source(""))
        assert check.compare_swift(released, check.parse_swift_factories(self.source("throws")), TAG)

    def test_adding_both_getter_effects_requires_a_new_call(self) -> None:
        released = check.parse_swift_factories(self.source(""))
        assert check.compare_swift(released, check.parse_swift_factories(self.source("async throws")), TAG)

    def test_shorthand_and_explicit_ordinary_getters_are_compatible(self) -> None:
        implicit = "public class Box { public static var shared: Box { fatalError() } }"
        assert (
            check.compare_swift(
                check.parse_swift_factories(implicit), check.parse_swift_factories(self.source("")), TAG
            )
            == []
        )
