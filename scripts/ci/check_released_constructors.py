#!/usr/bin/env python3
"""Check released Python constructors and Swift initializer/factory declarations.

The Python stub records field order and native signatures. Every released type and argument
must remain compatible. Swift declarations are read with the pinned tree-sitter grammar,
including enclosing declarations, modifiers, generics, conditions, defaults and overloads.
Initializer and function references require exact arity: adding a defaulted argument needs
an exact released compatibility overload. Public static/class owner-returning factories,
optional/array wrappers and static owner-valued properties are compared by owner and name.
Same-file aliases and owner visibility are resolved; imported declarations are not available
to this reader. Known Vectorizable pointer operations in SwiftBridgeCore are not DTO factories.

A present source must contain constructors at both revisions. Missing released types,
unread grammar nodes, duplicate declarations and unresolved recursive aliases fail closed.
The Python stub is mandatory at the newest release tag. Swift sources predating the package
are reported as absent instead of compared. Every source prints released, current and kept
counts; comparing zero sources fails. Run from the repository root with fetched release tags.
"""

from __future__ import annotations

import argparse
import ast
import re
import subprocess
import sys
from collections import Counter
from dataclasses import dataclass, field, replace
from itertools import zip_longest
from pathlib import Path
from typing import TYPE_CHECKING, Any, NamedTuple, TypeVar

if TYPE_CHECKING:
    from collections.abc import Callable, Iterator

REQUIREMENTS = "scripts/ci/requirements-released-constructors.txt"
try:
    import tree_sitter_swift
    from tree_sitter import Language, Node, Parser
except ImportError as missing_module:
    sys.exit(f"::error::{missing_module}. The Swift reader needs the pinned wheels: pip install -r {REQUIREMENTS}")

STUB = "packages/python/crawlberg/_crawlberg.pyi"
SWIFT_FACADE = "packages/swift/Sources/Crawlberg/Crawlberg.swift"
SWIFT_BRIDGE = "packages/swift/Sources/RustBridge/crawlberg-swift.swift"
SWIFT_BRIDGE_CORE = "packages/swift/Sources/RustBridge/SwiftBridgeCore.swift"
RELEASE_TAG = re.compile(r"^v\d+\.\d+\.\d+$")
SWIFT = Language(tree_sitter_swift.language())


@dataclass(frozen=True)
class Parameter:
    """One parameter of a constructor, as the stub declares it."""

    name: str
    annotation: str
    has_default: bool
    # ~keep The position in a positional call, from 0. `None`: the parameter is keyword-only.
    slot: int | None
    by_keyword: bool


@dataclass
class BoundType:
    # ~keep `fields` is the struct order: the public Python dataclass, Kotlin and Go follow it.
    # `signature` is the native constructor: PHP, WebAssembly, Java, Dart and Zig follow it.
    fields: dict[str, str] = field(default_factory=dict)
    signature: dict[str, Parameter] = field(default_factory=dict)


def parse_signature(init: ast.FunctionDef) -> dict[str, Parameter]:
    """Return every parameter of the constructor but `self`, by name."""
    arguments = init.args
    positional = [(argument, False) for argument in arguments.posonlyargs]
    positional += [(argument, True) for argument in arguments.args]
    first_default = len(positional) - len(arguments.defaults)
    signature: dict[str, Parameter] = {}
    for index, (argument, by_keyword) in enumerate(positional[1:], start=1):
        annotation = ast.unparse(argument.annotation) if argument.annotation else ""
        signature[argument.arg] = Parameter(argument.arg, annotation, index >= first_default, index - 1, by_keyword)
    for argument, default in zip(arguments.kwonlyargs, arguments.kw_defaults, strict=True):
        annotation = ast.unparse(argument.annotation) if argument.annotation else ""
        signature[argument.arg] = Parameter(argument.arg, annotation, default is not None, None, True)
    return signature


def parse_stub(source: str, where: str = STUB) -> dict[str, BoundType]:
    """Read stub constructors, raising SyntaxError with the path and line for invalid Python."""
    types: dict[str, BoundType] = {}
    for node in ast.parse(source, filename=where).body:
        if not isinstance(node, ast.ClassDef):
            continue
        bound = BoundType()
        init = None
        for item in node.body:
            if isinstance(item, ast.AnnAssign) and isinstance(item.target, ast.Name):
                bound.fields[item.target.id] = ast.unparse(item.annotation)
            elif isinstance(item, ast.FunctionDef) and item.name == "__init__":
                init = item
        if init is None:
            continue
        bound.signature = parse_signature(init)
        types[node.name] = bound
    return types


def compare_fields(name: str, released: BoundType, head: BoundType, tag: str) -> list[str]:
    errors = []
    head_names = list(head.fields)
    for field_name, annotation in released.fields.items():
        if field_name not in head.fields:
            errors.append(f"{name}.{field_name}: the field of {tag} is not in the bindings (removed or skipped)")
        elif head.fields[field_name] != annotation:
            errors.append(f"{name}.{field_name}: the type changed from `{annotation}` to `{head.fields[field_name]}`")
    kept = [field_name for field_name in head_names if field_name in released.fields]
    if not errors and kept != list(released.fields):
        errors.append(f"{name}: the fields of {tag} are in another order: {', '.join(kept)}")
    for slot, field_name in enumerate(head_names):
        later = [other for other in head_names[slot + 1 :] if other in released.fields]
        if field_name not in released.fields and later:
            errors.append(
                f"{name}.{field_name}: the new field is field {slot + 1} of the struct, before `{later[0]}` of {tag}"
            )
    return errors


def compare_constructor(name: str, released: BoundType, head: BoundType, tag: str) -> list[str]:
    """Keep released names, types, positional slots, keyword access and defaults.

    New arguments must leave every released call binding the same values.
    """
    errors = []
    for parameter in released.signature.values():
        where = f"{name}.{parameter.name}"
        now = head.signature.get(parameter.name)
        if now is None:
            errors.append(f"{where}: the constructor argument of {tag} is removed or renamed")
            continue
        if now.annotation != parameter.annotation:
            errors.append(f"{where}: the argument type changed from `{parameter.annotation}` to `{now.annotation}`")
        if parameter.slot is not None and now.slot != parameter.slot:
            was = parameter.slot + 1
            moved = "keyword-only" if now.slot is None else f"positional argument {now.slot + 1}"
            errors.append(f"{where}: positional argument {was} of the constructor of {tag} is now {moved}")
        if parameter.by_keyword and not now.by_keyword:
            errors.append(f"{where}: the constructor of {tag} takes it by keyword, the head does not")
        if parameter.has_default and not now.has_default:
            errors.append(f"{where}: the argument has a default in {tag} and is required now")
    for parameter in head.signature.values():
        if parameter.name in released.signature:
            continue
        where = f"{name}.{parameter.name}"
        if not parameter.has_default:
            errors.append(f"{where}: the new constructor argument is required, so no call of {tag} is valid")
        # ~keep Python accepts a new positional argument with a default at the end. The bindings
        # with one positional constructor (PHP, WebAssembly, Java, Dart, Zig) do not. alef writes
        # a new field as keyword-only exactly when the struct records the released constructor.
        if parameter.slot is not None:
            errors.append(
                f"{where}: the new field is positional argument {parameter.slot + 1} of the constructor, "
                f"with no recorded constructor of {tag}"
            )
    return errors


def compare(released: dict[str, BoundType], head: dict[str, BoundType], tag: str) -> list[str]:
    errors = []
    for name, released_type in released.items():
        if name not in head:
            errors.append(f"{name}: the type of {tag} has no constructor in the bindings")
            continue
        found = compare_fields(name, released_type, head[name], tag)
        # ~keep A field in the wrong struct slot already names its type. The constructor check
        # then only repeats it, so it runs for a type whose struct order is good.
        errors.extend(found or compare_constructor(name, released_type, head[name], tag))
    return errors


@dataclass(frozen=True)
class SwiftParameter:
    """One parameter of a Swift initializer, as the grammar reads it."""

    # ~keep The label is the external name: `_` when a call passes the argument by position.
    label: str
    name: str
    # ~keep The text after the colon: `inout`, `@escaping`, the type and `...`, as a call sees it.
    type: str
    default: str | None
    # ~keep True when the type holds a function type anywhere (`() -> Void`, `(() -> Void)?`):
    # a released call may pass the parameter as a trailing closure, with no label.
    closure: bool
    resolved_type: str = ""

    @property
    def declaration(self) -> str:
        head = self.label if self.label == self.name else f"{self.label} {self.name}"
        return f"{head}: {self.type}" if self.default is None else f"{head}: {self.type} = {self.default}"

    @property
    def where(self) -> str:
        """The name a caller writes: the label, or the internal name of a positional parameter."""
        return self.name if self.label == "_" else self.label


@dataclass(frozen=True)
class SwiftInitializer:
    """One public initializer, read whole: everything of its declaration but the body."""

    # ~keep The names of the enclosing declarations, outermost first: `Models.LinkInfo`.
    owner: str
    # ~keep The text of each enclosing declaration without its body, and of each `#if` open
    # around the initializer, outermost first.
    chain: tuple[str, ...]
    # ~keep The attributes and modifiers before `init`.
    modifiers: str
    # ~keep `init` with `?` or `!`, the generic clause, and everything between `)` and the body.
    form: str
    parameters: tuple[SwiftParameter, ...]

    @property
    def key(self) -> str:
        """Key the full signature and enclosing chain, retaining generic/async/extension overloads."""
        signature = f"{self.owner}({', '.join(parameter.declaration for parameter in self.parameters)})"
        return " ".join((signature, self.form, self.modifiers, *self.chain))


@dataclass(frozen=True)
class SwiftFactory(SwiftInitializer):
    """One public type factory, including its function name and whole declaration."""

    name: str

    @property
    def key(self) -> str:
        signature = f"{self.owner}.{self.name}({', '.join(parameter.declaration for parameter in self.parameters)})"
        return " ".join((signature, self.form, self.modifiers, *self.chain))


class UnreadNodeError(Exception):
    """A node of a declaration that the reader does not classify. The parse fails with its kind."""

    def __init__(self, node: Node, what: str) -> None:
        super().__init__(what)
        self.node = node


# ~keep Every child kind that the reader classifies. A child of any other kind fails the parse, so
# a new grammar form is never compared by accident. The parameter list and the body are read by
# their own rules.
COMMENTS = frozenset({"comment", "multiline_comment"})
KINDS = frozenset(SWIFT.node_kind_for_id(kind) or "" for kind in range(SWIFT.node_kind_count))
MODIFIERS = frozenset({"attribute"} | {kind for kind in KINDS if kind.endswith("_modifier")})
INITIALIZER_HEAD = frozenset({"init", "?", "bang", "type_parameters"})
INITIALIZER_TAIL = frozenset({"async", "throws", "throws_clause", "type_constraints"})
PARAMETER_LIST = frozenset({"(", ",", "=", ")"})
OWNER_HEAD = frozenset(
    {"modifiers", "attribute", "struct", "class", "enum", "actor", "extension", "type_identifier", "user_type"}
    | {"type_parameters", ":", "inheritance_specifier", ",", "type_constraints"}
)
BODIES = frozenset({"class_body", "enum_class_body"})


def nodes(node: Node) -> Iterator[Node]:
    yield node
    for child in node.children:
        yield from nodes(child)


def text(source: bytes, node: Node) -> str:
    """Read the original bytes, including bridge dollar identifiers."""
    return source[node.start_byte : node.end_byte].decode()


def leaves(node: Node) -> Iterator[Node]:
    """Yield the leaves of the tree in order. A comment and a string literal are leaves."""
    if node.child_count and node.type not in COMMENTS and not node.type.endswith("string_literal"):
        for child in node.children:
            yield from leaves(child)
    else:
        yield node


def tokens(source: bytes, *items: Node) -> Iterator[str]:
    """Yield the tokens of the nodes in order, with the comments out.

    A leaf of the tree is a token. Text between two leaves that is not a comment is a token too:
    the grammar hides a keyword of some nodes (`throws` of a `throws_clause`).
    """
    for item in items:
        end = item.start_byte
        for node in leaves(item):
            yield from source[end : node.start_byte].decode().split()
            if node.type not in COMMENTS:
                yield text(source, node)
            end = node.end_byte


def spaced(before: str, lexeme: str) -> bool:
    """True when canonical Swift writes a space between the two lexemes."""
    word = before[-1].isalnum() or before[-1] in "_>?!"
    if before[-1] in "([<@." or lexeme[0] in ")]>,:?!." or lexeme == "...":
        return False
    return not (lexeme[0] in "(<" and word)


def normal(source: bytes, *items: Node) -> str:
    """Return the text of the nodes with the comments out and the whitespace made canonical."""
    out = ""
    for lexeme in tokens(source, *items):
        if lexeme:
            out += f" {lexeme}" if out and spaced(out, lexeme) else lexeme
    return out


def classified(node: Node, known: frozenset[str], where: str) -> list[Node]:
    """Return the children of `node` that are not comments. A child of a kind not in `known` raises."""
    children = [child for child in node.children if child.type not in COMMENTS]
    for child in children:
        if child.type not in known:
            raise UnreadNodeError(child, f"a `{child.type}` node {where} that the reader does not classify")
    return children


def open_directives(source: bytes, parent: Node, child: Node) -> list[str]:
    """Return the text of each `#if` that is open at `child` among the children of `parent`."""
    stack: list[str] = []
    for sibling in parent.children:
        if sibling == child:
            break
        if sibling.type != "directive":
            continue
        directive = sibling.children[0].type
        if directive == "#if":
            stack.append(normal(source, sibling))
        elif directive in ("#elseif", "#else") and stack:
            stack[-1] = f"{stack[-1]} {normal(source, sibling)}"
        elif directive == "#endif" and stack:
            stack.pop()
        else:
            raise UnreadNodeError(
                sibling, f"a `{directive}` directive before the initializer that the reader does not classify"
            )
    return stack


def visibility(source: bytes, declaration: Node) -> str | None:
    """Return the visibility modifier that a declaration writes, or `None`."""
    modifiers = next((child for child in declaration.children if child.type == "modifiers"), None)
    written = (
        [text(source, item) for item in modifiers.children if item.type == "visibility_modifier"] if modifiers else []
    )
    return written[0] if written else None


def extension(declaration: Node) -> bool:
    return any(child.type == "extension" for child in declaration.children)


def level(source: bytes, declaration: Node) -> str:
    """Return the access level of a declaration, as Swift resolves it.

    The level is the visibility modifier the declaration writes. Without one, a member of an
    extension has the level of the extension (`public extension Box { init() }` is public), and
    every other declaration is `internal`.
    """
    own = visibility(source, declaration)
    if own:
        return own
    holder = declaration.parent.parent if declaration.parent and declaration.parent.type in BODIES else None
    return level(source, holder) if holder is not None and extension(holder) else "internal"


def public(source: bytes, node: Node) -> bool:
    """True when a caller outside the module can call the initializer `node`: the one decision.

    The initializer's own level (`level`) must be `public` or `open`, and so must the level of
    every enclosing declaration: the owner type, each type around it, and each extension that
    writes a visibility modifier. A `fileprivate extension`, an `internal` type and a type without
    a modifier hide their initializers. A protocol holds a requirement, not a call. An extension
    without a visibility modifier hides nothing: its members carry their own levels.
    """
    declarations: list[Node] = []
    parent = node.parent
    while parent is not None:
        if parent.type in ("protocol_body", "protocol_declaration"):
            return False
        if parent.type == "class_declaration":
            declarations.append(parent)
        parent = parent.parent
    ceilings = [item for item in declarations if not extension(item) or visibility(source, item)]
    return all(level(source, item) in ("public", "open") for item in (node, *ceilings))


def enclosing(source: bytes, node: Node) -> tuple[str, tuple[str, ...]]:
    """Return the owner path and the chain of `node`.

    The chain has, outermost first, each enclosing declaration without its body and each `#if`
    that is open around the initializer. An enclosing node of another kind raises.
    """
    names: list[str] = []
    chain: list[str] = []
    child, parent = node, node.parent
    while parent is not None:
        links = open_directives(source, parent, child)
        if child.type == "class_declaration":
            head = [
                item
                for item in classified(child, OWNER_HEAD | BODIES, "around the initializer")
                if item.type not in BODIES
            ]
            name = child.child_by_field_name("name")
            names.insert(0, text(source, name) if name else "")
            links.append(normal(source, *head))
        chain[:0] = links
        if parent.type not in BODIES and parent.type not in ("class_declaration", "source_file"):
            raise UnreadNodeError(
                parent, f"a `{parent.type}` node around the initializer that the reader does not classify"
            )
        child, parent = parent, parent.parent
    if not names:
        raise UnreadNodeError(node, "an initializer that no type declares")
    return ".".join(names), tuple(chain)


def unavailable(source: bytes, modifiers: Node | None) -> bool:
    """True for `@available(*, unavailable, ...)`, read as nodes: no platform can call it.

    `@available(macOS, unavailable)` keeps the initializer: another platform calls it, and the
    attribute text is compared like any other.
    """
    for attribute in modifiers.children if modifiers else ():
        if attribute.type != "attribute" or normal(source, attribute.children[1]) != "available":
            continue
        arguments = [item for item in attribute.children[2:] if item.type not in ("(", ",", ")")]
        everywhere = bool(arguments) and arguments[0].type == "*"
        if everywhere and any(
            item.type == "simple_identifier" and text(source, item) == "unavailable" for item in arguments
        ):
            return True
    return False


def swift_scope(source: bytes, node: Node | None) -> str:
    names = []
    while node is not None:
        if node.type == "class_declaration":
            name = node.child_by_field_name("name")
            if name is not None:
                names.insert(0, text(source, name))
        node = node.parent
    return ".".join(names)


def owner_result(result: str, owner: str) -> bool:
    while result.endswith(("?", "!")) or result.startswith(("Optional<", "[")):
        if result.endswith(("?", "!")):
            result = result[:-1]
        elif result.startswith("Optional<") and result.endswith(">"):
            result = result[len("Optional<") : -1]
        elif result.startswith("[") and result.endswith("]"):
            result = result[1:-1]
        else:
            break
    return result.partition("<")[0] in {"Self", owner, owner.rsplit(".", 1)[-1]}


@dataclass
class SwiftContext:
    aliases: dict[str, str]
    owners: dict[str, Node]
    source: bytes
    where: str

    def resolve(self, value: str, node: Node) -> str:
        shadowed = set()
        parent: Node | None = node
        while parent is not None:
            for clause in parent.children:
                if clause.type == "type_parameters":
                    shadowed.update(
                        text(self.source, child.children[0]) for child in clause.named_children if child.children
                    )
            parent = parent.parent
        seen = set()
        while value not in seen and len(seen) <= len(self.aliases):
            seen.add(value)
            scope = swift_scope(self.source, node.parent)
            prefixes = [".".join(scope.split(".")[:index]) for index in range(len(scope.split(".")), 0, -1)]
            prefixes.append("")

            def expand(match: re.Match[str]) -> str:
                name = match[0]
                if name in shadowed:
                    return name
                for prefix in prefixes:
                    candidate = f"{prefix}.{name}" if prefix else name
                    if candidate in self.aliases:
                        return self.aliases[candidate]
                return name

            changed = re.sub(r"\b[A-Za-z_][A-Za-z_0-9.]*\b", expand, value)
            if changed == value:
                return value
            value = changed
        raise UnreadNodeError(node, "a cyclic typealias that the reader cannot resolve")


def swift_context(source: bytes, root: Node, where: str) -> SwiftContext:
    aliases = {}
    owners = {}
    for node in nodes(root):
        name = node.child_by_field_name("name")
        if node.type == "typealias_declaration" and name is not None:
            parts = [child for child in node.children if child.start_byte > name.end_byte and child.type != "="]
            scope = swift_scope(source, node.parent)
            key = f"{scope}.{text(source, name)}" if scope else text(source, name)
            aliases[key] = normal(source, *parts)
        elif node.type == "class_declaration" and name is not None and not extension(node):
            owners[swift_scope(source, node)] = node
    return SwiftContext(aliases, owners, source, where)


def owner_is_public(source: bytes, node: Node, context: SwiftContext | None) -> bool:
    if context is None:
        return True
    parent = node.parent
    while parent is not None:
        if parent.type == "class_declaration" and extension(parent):
            name = parent.child_by_field_name("name")
            owner = context.owners.get(text(source, name)) if name else None
            if owner is not None and not public(source, owner):
                return False
        parent = parent.parent
    return True


def swift_parameter(source: bytes, node: Node, context: SwiftContext | None = None) -> SwiftParameter:
    label = name = None
    declared: list[Node] = []
    for index, child in enumerate(node.children):
        field_name = node.field_name_for_child(index)
        if child.type in COMMENTS or child.type == ":":
            continue
        if field_name == "external_name":
            label = text(source, child)
        elif field_name == "name" and child.type == "simple_identifier":
            name = text(source, child)
        elif field_name == "name" or child.type in ("parameter_modifiers", "..."):
            declared.append(child)
        else:
            raise UnreadNodeError(child, f"a `{child.type}` node of a parameter that the reader does not classify")
    # ~keep The grammar gives every `parameter` a name and a type: `init(x)` and `init(x:)` are
    # parse errors, which `parse_swift` raises before it reads any initializer.
    assert name is not None and declared, node
    closure = any(item.type == "function_type" for item in declared for item in nodes(item))
    written = normal(source, *declared)
    resolved = context.resolve(written, node) if context else written
    closure = closure or "->" in resolved
    return SwiftParameter(label or name, name, written, None, closure, resolved)


def swift_initializer(source: bytes, node: Node, context: SwiftContext | None = None) -> SwiftInitializer | None:
    """Read a callable public initializer, failing on unclassified declaration nodes."""
    modifiers = next((child for child in node.children if child.type == "modifiers"), None)
    if not public(source, node) or not owner_is_public(source, node, context) or unavailable(source, modifiers):
        return None
    if modifiers:
        classified(modifiers, MODIFIERS, "of the modifiers")
    owner, chain = enclosing(source, node)
    parameters: list[SwiftParameter] = []
    form: list[Node] = []
    for index, child in enumerate(node.children):
        if child.type == "parameter":
            parameters.append(swift_parameter(source, child, context))
        elif node.field_name_for_child(index) == "default_value" and parameters:
            parameters[-1] = replace(parameters[-1], default=normal(source, child))
        elif child.type in INITIALIZER_HEAD or child.type in INITIALIZER_TAIL:
            form.append(child)
        elif child.type not in PARAMETER_LIST | COMMENTS | {"modifiers", "function_body"}:
            raise UnreadNodeError(child, f"a `{child.type}` node of the initializer that the reader does not classify")
    written = normal(source, modifiers) if modifiers else ""
    return SwiftInitializer(owner, chain, written, normal(source, *form), tuple(parameters))


VECTOR_OPERATIONS = frozenset(
    {
        "vecOfSelfNew",
        "vecOfSelfFree",
        "vecOfSelfPush",
        "vecOfSelfPop",
        "vecOfSelfGet",
        "vecOfSelfGetMut",
        "vecOfSelfAsPtr",
        "vecOfSelfLen",
    }
)


def swift_factory(source: bytes, node: Node, context: SwiftContext | None = None) -> SwiftFactory | None:
    """Read a public static/class function returning Self or its enclosing type."""
    modifiers = next((child for child in node.children if child.type == "modifiers"), None)
    if (
        not modifiers
        or not public(source, node)
        or not owner_is_public(source, node, context)
        or unavailable(source, modifiers)
    ):
        return None
    kinds = {normal(source, child) for child in modifiers.children}
    if not kinds.intersection({"static", "class"}):
        return None
    owner, chain = enclosing(source, node)
    name_node = node.child_by_field_name("name")
    factory_name = text(source, name_node) if name_node else ""
    # ~keep Vectorizable's pointer operations read/free a vector; numeric return types do not construct DTOs.
    if context and context.where.endswith(SWIFT_BRIDGE_CORE) and factory_name in VECTOR_OPERATIONS:
        return None
    returned = next(
        (child for index, child in enumerate(node.children) if index and node.children[index - 1].type == "->"), None
    )
    if returned is None:
        return None
    written_result = normal(source, returned)
    result = context.resolve(written_result, node) if context else written_result
    if not owner_result(result, owner):
        return None
    classified(modifiers, MODIFIERS, "of the factory modifiers")
    parameters: list[SwiftParameter] = []
    form: list[Node] = []
    name = ""
    allowed = INITIALIZER_HEAD | INITIALIZER_TAIL | {"func", "simple_identifier", "->", returned.type}
    for index, child in enumerate(node.children):
        if child.type == "parameter":
            parameters.append(swift_parameter(source, child, context))
        elif node.field_name_for_child(index) == "default_value" and parameters:
            parameters[-1] = replace(parameters[-1], default=normal(source, child))
        elif child.type in allowed:
            if child.type == "simple_identifier" and node.field_name_for_child(index) == "name":
                name = text(source, child)
            else:
                form.append(child)
        elif child.type not in PARAMETER_LIST | COMMENTS | {"modifiers", "function_body"}:
            raise UnreadNodeError(child, f"a `{child.type}` node of the factory that the reader does not classify")
    resolved = context.resolve(written_result, node) if context else written_result
    declaration = normal(source, *form)
    if resolved != written_result:
        declaration += f" resolved result {resolved}"
    return SwiftFactory(owner, chain, normal(source, modifiers), declaration, tuple(parameters), name)


def property_getter(source: bytes, node: Node) -> str:
    computed = node.child_by_field_name("computed_value")
    if computed is None:
        return "get"
    children = classified(
        computed, COMMENTS | {"{", "}", "computed_getter", "computed_setter", "statements"}, "of a computed property"
    )
    getter = next((child for child in children if child.type == "computed_getter"), None)
    if getter is None:
        return "get"
    head = classified(
        getter, COMMENTS | {"getter_specifier", "modifiers", "statements", "{", "}"}, "of a property getter"
    )
    return normal(source, *(child for child in head if child.type not in {"statements", "{", "}"}))


def swift_property(source: bytes, node: Node, context: SwiftContext | None = None) -> SwiftFactory | None:
    modifiers = next((child for child in node.children if child.type == "modifiers"), None)
    annotation = next((child for child in node.children if child.type == "type_annotation"), None)
    if not modifiers or annotation is None or not public(source, node) or not owner_is_public(source, node, context):
        return None
    if unavailable(source, modifiers) or not any(normal(source, child) == "static" for child in modifiers.children):
        return None
    owner, chain = enclosing(source, node)
    result = normal(source, annotation).removeprefix(":").strip()
    result = context.resolve(result, node) if context else result
    if not owner_result(result, owner):
        return None
    name = node.child_by_field_name("name")
    assert name is not None
    heads = [child for child in node.children if child.type in {"value_binding_pattern", "type_annotation"}]
    form = normal(source, *heads) + " " + property_getter(source, node)
    written = normal(source, annotation).removeprefix(":").strip()
    if result != written:
        form += f" resolved result {result}"
    return SwiftFactory(owner, chain, normal(source, modifiers), form, (), normal(source, name))


def parse_swift_factories(source: str, where: str = SWIFT_FACADE) -> dict[str, SwiftFactory]:
    """Read factories; a release with zero factories remains valid."""
    return _parse_swift(source, where, "function_declaration", swift_factory)


def parse_swift(source: str, where: str = SWIFT_FACADE) -> dict[str, SwiftInitializer]:
    """Read public initializers by their whole declarations, preserving overloads.

    Unread grammar nodes and duplicate declarations fail with a source line. The grammar
    rejects swift-bridge's dollar identifiers: parse a same-byte-length underscore copy,
    then read every declaration from the original bytes so comparisons preserve the dollars.
    """
    return _parse_swift(source, where, "init_declaration", swift_initializer)


SwiftDeclaration = TypeVar("SwiftDeclaration", bound=SwiftInitializer)


def _parse_swift(
    source: str,
    where: str,
    kind: str,
    reader: Callable[[bytes, Node, SwiftContext | None], SwiftDeclaration | None],
) -> dict[str, SwiftDeclaration]:
    data = source.encode()
    tree = Parser(SWIFT).parse(data.replace(b"$", b"_"))
    initializers: dict[str, SwiftDeclaration] = {}

    def error(node: Node, what: str) -> SyntaxError:
        row, column = node.start_point
        line = source.splitlines()[row] if row < len(source.splitlines()) else ""
        return SyntaxError(what, (where, row + 1, column + 1, line))

    if tree.root_node.has_error:
        broken = next(
            node
            for node in nodes(tree.root_node)
            if node.has_error and not any(child.has_error for child in node.children)
        )
        raise error(broken, "text that the Swift grammar does not read")
    context = swift_context(data, tree.root_node, where)
    for node in nodes(tree.root_node):
        property_factory = kind == "function_declaration" and node.type == "property_declaration"
        if node.type != kind and not property_factory:
            continue
        try:
            initializer = swift_property(data, node, context) if property_factory else reader(data, node, context)
        except UnreadNodeError as unread:
            raise error(unread.node, str(unread)) from None
        if initializer and initializer.key in initializers:
            raise error(node, f"a second declaration of `{initializer.key}`")
        if initializer:
            initializers[initializer.key] = initializer
    return initializers


def compare_initializer(was: SwiftInitializer, now: SwiftInitializer, tag: str) -> list[str]:
    """Compare declarations, preserving defaults, argument binding and reference arity.

    The detailed positional/trailing-closure checks explain incompatible call changes. A
    remaining arity difference still fails: defaults cannot adapt a function reference.
    """
    errors = []
    for before, after in zip_longest(was.chain, now.chain, fillvalue="no declaration"):
        if before != after:
            errors.append(f"{was.owner}: around the Swift initializer {tag} has `{before}`, the head has `{after}`")
            break
    if now.modifiers != was.modifiers:
        errors.append(
            f"{was.owner}: the Swift initializer of {tag} has the modifiers `{was.modifiers}`, the head has `{now.modifiers}`"
        )
    if now.form != was.form:
        errors.append(f"{was.owner}: the Swift initializer of {tag} is `{was.form}`, the head is `{now.form}`")
    labels, head_labels = [p.label for p in was.parameters], [p.label for p in now.parameters]
    common_head = [x for x in head_labels if x in labels]
    common_released = [x for x in labels if x in head_labels]
    if Counter(common_head) == Counter(common_released) and common_head != common_released:
        errors.append(f"{was.owner}: the Swift initializer labels of {tag} are in another order")
    remaining = list(was.parameters)
    for parameter in now.parameters:
        if remaining and parameter.label == remaining[0].label:
            before = remaining.pop(0)
            where = f"{was.owner}.{before.where}"
            if parameter.name != before.name:
                errors.append(f"{where}: the Swift argument name changed from `{before.name}` to `{parameter.name}`")
            if parameter.type != before.type or parameter.resolved_type != before.resolved_type:
                errors.append(f"{where}: the Swift argument type changed from `{before.type}` to `{parameter.type}`")
            if before.default is not None and parameter.default is None:
                errors.append(f"{where}: the Swift argument has a default in {tag} and is required now")
            elif before.default is not None and parameter.default != before.default:
                changed = f"changed from `{before.default}` to `{parameter.default}`"
                errors.append(f"{where}: the Swift argument default {changed}")
            continue
        where = f"{was.owner}.{parameter.where}"
        positional = next((other for other in remaining if other.label == "_"), None)
        closure = next((other for other in remaining if other.closure), None)
        if parameter.default is None:
            errors.append(f"{where}: the new Swift initializer argument is required: no call of {tag} compiles")
        elif positional is not None:
            errors.append(
                f"{where}: the new Swift initializer argument comes before the positional argument "
                f"`{positional.name}` of {tag}: a positional call of {tag} binds another value"
            )
        elif closure is not None:
            errors.append(
                f"{where}: the new Swift initializer argument comes before the argument `{closure.where}` of {tag}, "
                f"which has a function type: a trailing closure of {tag} binds another parameter"
            )
        elif parameter.closure:
            errors.append(
                f"{where}: the new Swift initializer argument has a function type: a trailing closure of {tag} binds it"
            )
    removed = f"the Swift initializer argument of {tag} is removed or renamed"
    errors.extend(f"{was.owner}.{parameter.where}: {removed}" for parameter in remaining)
    if not errors and len(now.parameters) != len(was.parameters):
        errors.append(
            f"{was.owner}: the Swift initializer function reference of {tag} changes arity; keep its overload"
        )
    return errors


def compare_swift(released: dict[str, SwiftInitializer], head: dict[str, SwiftInitializer], tag: str) -> list[str]:
    """Match each released declaration to the same owner/name, retaining its exact overload.

    The closest candidate supplies diagnostics; only a candidate with no difference passes.
    """
    errors: list[str] = []
    lost: set[tuple[str, str | None]] = set()
    for was in released.values():
        was_name = was.name if isinstance(was, SwiftFactory) else None
        candidates = [
            now
            for now in head.values()
            if now.owner == was.owner and (now.name if isinstance(now, SwiftFactory) else None) == was_name
        ]
        if candidates:
            found = min((compare_initializer(was, now, tag) for now in candidates), key=len)
            if isinstance(was, SwiftFactory):
                found = [error.replace("Swift initializer", f"Swift factory `{was.name}`") for error in found]
            errors.extend(error for error in found if error not in errors)
        elif (identity := (was.owner, was_name)) not in lost:
            lost.add(identity)
            kind = f"factory `{was.name}`" if isinstance(was, SwiftFactory) else "initializer"
            errors.append(f"{was.owner}: the Swift type of {tag} has no public {kind} at the head")
    return errors


def git(*arguments: str) -> str:
    return subprocess.run(["git", *arguments], check=True, capture_output=True, text=True).stdout


def newest_release_tag(tags: list[str]) -> str | None:
    """Return the release with the highest version, wherever its commit is.

    A release tag can point at a commit of a release branch, which is not an ancestor of the
    head. The newest tag that is an ancestor is then an older release, so ancestry is not used.
    A pre-release (`v1.2.3-rc.1`) and a package tag (`packages/go/v1.2.3`) are not releases here.
    """
    releases = [tag for tag in tags if RELEASE_TAG.match(tag)]
    return max(releases, key=lambda tag: tuple(int(part) for part in tag[1:].split(".")), default=None)


def released_stub(tag: str, path: str = STUB) -> str | None:
    """Return the file in the tree of the tag, or `None` when the tag has no such file.

    Only a tree without the path gives `None`. Any other failure of git raises.
    """
    if not git("ls-tree", "--name-only", tag, "--", path).strip():
        return None
    return git("show", f"{tag}:{path}")


@dataclass(frozen=True)
class Source:
    """One file that the check compares: its path, its reader and its comparison."""

    path: str
    parse: Callable[[str, str], dict[str, Any]]
    compare: Callable[[dict[str, Any], dict[str, Any], str], list[str]]
    # ~keep A primary source must exist at the release tag. A secondary source can be newer than the tag.
    primary: bool


# ~keep The stub does not show a typed facade that drops the recorded default. The Swift
# sources state each default, so they are compared with no Swift toolchain: every file of the
# package that declares a public initializer.
SOURCES = (
    Source(STUB, parse_stub, compare, primary=True),
    Source(SWIFT_FACADE, parse_swift, compare_swift, primary=False),
    Source(SWIFT_BRIDGE, parse_swift, compare_swift, primary=False),
    Source(SWIFT_BRIDGE_CORE, parse_swift, compare_swift, primary=False),
)


class Input(NamedTuple):
    source: Source
    released_text: str | None
    head_path: str
    head_text: str | None


class Result(NamedTuple):
    line: str
    errors: list[str]
    # ~keep `None`: the source is not compared.
    kept: int | None


def check_source(tag: str, item: Input) -> Result:
    """Apply the rule of the module docstring to one source. This is the only place that applies it."""
    path = item.source.path
    if item.released_text is None:
        missing_stub = [f"the stub of {tag} is missing: {path}"] if item.source.primary else []
        return Result(f"not compared: {path} absent at {tag}", missing_stub, None)
    if item.head_text is None:
        error = f"{path} exists at {tag} and {item.head_path} does not exist at the head"
        return Result(f"not compared: {path} absent at the head ({item.head_path})", [error], None)
    try:
        released = item.source.parse(item.released_text, f"{tag}:{path}")
        head = item.source.parse(item.head_text, item.head_path)
    except SyntaxError as error:
        found = f"{error.filename}:{error.lineno}: parse error: {error.msg}"
        return Result(f"not compared: {path} has a parse error", [found], None)
    # ~keep A key is a type, or a type and its whole declaration from `(`. The rule is on the type:
    # the declaration of a kept initializer changes when a parameter with a default is added.
    owners = {name.partition("(")[0] for name in head}
    missing = [name.partition("(")[0] for name in released if name.partition("(")[0] not in owners]
    kept = sum(1 for name, was in released.items() if not item.source.compare({name: was}, head, tag))
    counts = f"{tag} has {len(released)} constructors, the head ({item.head_path}) has {len(head)}"
    errors = []
    if not released or not head or missing:
        first = f", the first missing type is `{missing[0]}`" if missing else ""
        errors.append(f"{path}: {counts}{first}. The check does not pass on a source that it cannot read in full")
    errors.extend(item.source.compare(released, head, tag))
    line = f"{path}: {counts}, {kept} kept"
    if item.source.parse is parse_swift:
        factory_line, factory_errors = check_factories(tag, item)
        line += f"\n{factory_line}"
        errors.extend(factory_errors)
    return Result(line, errors, kept)


def check_factories(tag: str, item: Input) -> tuple[str, list[str]]:
    """Compare factories of a present Swift source; zero released factories is valid."""
    path = item.source.path
    assert item.released_text is not None and item.head_text is not None
    try:
        released = parse_swift_factories(item.released_text, f"{tag}:{path}")
        head = parse_swift_factories(item.head_text, item.head_path)
    except SyntaxError as error:
        found = f"{error.filename}:{error.lineno}: parse error: {error.msg}"
        return f"not compared: {path} factories have a parse error", [found]
    kept = sum(1 for name, was in released.items() if not compare_swift({name: was}, head, tag))
    line = f"{path}: {tag} has {len(released)} factories, the head ({item.head_path}) has {len(head)}, {kept} kept"
    return line, compare_swift(released, head, tag)


def run(tag: str, inputs: list[Input]) -> int:
    results = [check_source(tag, item) for item in inputs]
    for result in results:
        print(result.line)
    errors = [error for result in results for error in result.errors]
    compared = [result.kept for result in results if result.kept is not None]
    if not compared:
        errors.append(f"no source of {tag} was compared with the head: nothing was compared")
    if not errors:
        print(f"ok: the {sum(compared)} constructors of {tag} are kept")
        return 0
    for error in errors:
        print(f"::error::{error}")
    print(_HELP.format(tag=tag))
    return 1


def read_head(path: str) -> str | None:
    file = Path(path)
    return file.read_text(encoding="utf-8") if file.is_file() else None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--head", default=".", help="the tree with the sources of the head (default: %(default)s)")
    options = parser.parse_args()

    tag = newest_release_tag(git("tag", "--list", "v[0-9]*").split())
    if tag is None:
        print("::error::this checkout has no release tag `vX.Y.Z`. Fetch the tags: this check would compare nothing")
        return 1
    inputs = []
    for source in SOURCES:
        head_path = str(Path(options.head) / source.path)
        inputs.append(Input(source, released_stub(tag, source.path), head_path, read_head(head_path)))
    return run(tag, inputs)


_HELP = (
    "A call written against {tag} must still compile and mean the same. For each field above, do one of:\n"
    "  1. Keep the field out of the bindings: put `#[cfg_attr(alef, alef(skip))]` on it.\n"
    "  2. Move the field to the end of the struct and record the released fields on the struct:\n"
    '     `#[cfg_attr(alef, alef(legacy_constructor_fields = ["..."]))]`. The pinned alef must accept the type of the field.\n'
    "  3. Restore the field list of {tag}: the same names, types, order and defaults.\n"
    "Then run `task alef:generate`.\n"
    "A Swift arity change requires the exact released compatibility overload, including defaults.\n"
    "A Swift line above with a recorded constructor on the struct: the pinned alef writes the new\n"
    "field as a required argument of the public Swift initializer. The fix is in alef, not here."
)


if __name__ == "__main__":
    sys.exit(main())
