#!/usr/bin/env python3
"""Check that every constructor of the last release still works at the head.

The generated Python stub lists, for each bound type, the bound fields in struct order and the
native constructor. The check compares the stub of the newest release tag with the stub of the
working tree. Public static/class Swift factories returning `Self` or their owner are checked
separately, with the same declaration rules and owner plus function name matching; overloads
are retained. Zero factories at the release is valid and never relaxes constructor counts.
The Swift sources are compared the same way: every public initializer of every
type, read with the Swift grammar of tree-sitter (`tree-sitter-swift`, pinned in
`scripts/ci/requirements-released-constructors.txt`). Run it from the repository root. It needs
git and the tags, and no Rust or Swift toolchain.

The Swift comparison is by exclusion, in `compare_initializer`: for each released initializer,
the head must hold an initializer of the same owner whose whole declaration is equal. The whole
declaration is the chain of enclosing declarations (each one's attributes, modifiers, kind, name,
generic clause, inheritance and `where` clause, and each `#if` open around the initializer), the
initializer's attributes and modifiers, `init` with `?` or `!`, its generic clause, and everything
between `)` and the body, as text from the parse tree with comments and whitespace out. The one
permitted difference: the head may add parameters, each with a default, none that a released call
can pass without its label (`compare_initializer` states the rule). Every released parameter is
present in the released order with an equal label, internal name, type text (with `inout`,
`@escaping` and `...`) and default text. An initializer counts when a caller outside the module
can call it: `public` decides that, once, from the initializer's own modifier, the enclosing
extension's, and every enclosing declaration. A source holds every initializer it declares: the
whole declaration is the key, and two equal declarations fail the parse. A node of a declaration
that the reader does not classify fails the parse with the kind of the node, so a new grammar
form fails closed. Each rule can give a false failure, never a false pass.

The check fails closed. `check_source` applies one rule to every source in `SOURCES`:

- A source that exists at the release tag must give at least one constructor at the release and
  at the head, and every released type must be found at the head. If not, the check fails and
  names the source, the two counts and the first missing type.
- A source that exists at the release tag and is absent at the head fails.
- The Python stub is the primary source. If it is absent at the release tag, the check fails.
- A Swift source is a secondary source: a release can be older than the Swift package. If it
  is absent at the release tag, it is not compared, and the output says so in a line
  `not compared: <path> absent at <tag>`.
- If the number of sources that are compared is zero, the check fails.
- A source that a reader cannot parse fails with the file and the line. No reader returns an
  empty result for text that it does not understand.

The output has one line for each source: the path, the tag, the count at the release, the count
at the head and the count kept.
"""

from __future__ import annotations

import argparse
import ast
import re
import subprocess
import sys
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
    """Return the types of the stub that have a constructor, by name.

    Text that is not Python raises `SyntaxError` with `where` and the line.
    """
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
    """Return why a call that is valid against the released constructor is not valid at the head.

    The rule: every call that the released signature accepts, the head signature accepts, and it
    binds the same parameters. So a released parameter keeps its name, its type, its positional
    slot, its keyword and its default, and a new parameter takes nothing from an old call.
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
        """The whole declaration as one text, the signature `Owner(label: Type = default, ...)` first.

        Two initializers of one source have one key only when they are one declaration twice, so a
        dictionary by key holds every overload: `init<G>(_ value: G)` and its twin with
        `<G: Equatable>` differ in `form`, an `async` twin too, and two extensions differ in `chain`.
        """
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
    """The text of `node` in `source`, the bytes the file has: never the text the tree was parsed from."""
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


def swift_parameter(source: bytes, node: Node) -> SwiftParameter:
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
    return SwiftParameter(label or name, name, normal(source, *declared), None, closure)


def swift_initializer(source: bytes, node: Node) -> SwiftInitializer | None:
    """Return the public initializer that `node` declares, or `None` when a caller cannot call it.

    A caller cannot call an initializer that `public` refuses, or that `@available(*, unavailable)`
    marks. Every other initializer is read whole. A child of the initializer, of a parameter, of
    the modifiers or of an enclosing declaration that the reader does not classify raises
    `UnreadNodeError`.
    """
    modifiers = next((child for child in node.children if child.type == "modifiers"), None)
    if not public(source, node) or unavailable(source, modifiers):
        return None
    if modifiers:
        classified(modifiers, MODIFIERS, "of the modifiers")
    owner, chain = enclosing(source, node)
    parameters: list[SwiftParameter] = []
    form: list[Node] = []
    for index, child in enumerate(node.children):
        if child.type == "parameter":
            parameters.append(swift_parameter(source, child))
        elif node.field_name_for_child(index) == "default_value" and parameters:
            parameters[-1] = replace(parameters[-1], default=normal(source, child))
        elif child.type in INITIALIZER_HEAD or child.type in INITIALIZER_TAIL:
            form.append(child)
        elif child.type not in PARAMETER_LIST | COMMENTS | {"modifiers", "function_body"}:
            raise UnreadNodeError(child, f"a `{child.type}` node of the initializer that the reader does not classify")
    written = normal(source, modifiers) if modifiers else ""
    return SwiftInitializer(owner, chain, written, normal(source, *form), tuple(parameters))


def swift_factory(source: bytes, node: Node) -> SwiftFactory | None:
    """Read a public static/class function returning Self or its enclosing type."""
    modifiers = next((child for child in node.children if child.type == "modifiers"), None)
    if not modifiers or not public(source, node) or unavailable(source, modifiers):
        return None
    kinds = {normal(source, child) for child in modifiers.children}
    if not kinds.intersection({"static", "class"}):
        return None
    owner, chain = enclosing(source, node)
    returned = next(
        (child for index, child in enumerate(node.children) if index and node.children[index - 1].type == "->"), None
    )
    if returned is None:
        return None
    result = normal(source, returned).partition("<")[0]
    if result not in {"Self", owner, owner.rsplit(".", 1)[-1]}:
        return None
    classified(modifiers, MODIFIERS, "of the factory modifiers")
    parameters: list[SwiftParameter] = []
    form: list[Node] = []
    name = ""
    allowed = INITIALIZER_HEAD | INITIALIZER_TAIL | {"func", "simple_identifier", "->", returned.type}
    for index, child in enumerate(node.children):
        if child.type == "parameter":
            parameters.append(swift_parameter(source, child))
        elif node.field_name_for_child(index) == "default_value" and parameters:
            parameters[-1] = replace(parameters[-1], default=normal(source, child))
        elif child.type in allowed:
            form.append(child)
            if child.type == "simple_identifier" and node.field_name_for_child(index) == "name":
                name = text(source, child)
        elif child.type not in PARAMETER_LIST | COMMENTS | {"modifiers", "function_body"}:
            raise UnreadNodeError(child, f"a `{child.type}` node of the factory that the reader does not classify")
    return SwiftFactory(owner, chain, normal(source, modifiers), normal(source, *form), tuple(parameters), name)


def parse_swift_factories(source: str, where: str = SWIFT_FACADE) -> dict[str, SwiftFactory]:
    """Return every public type factory without treating factory absence as unread constructors."""
    return _parse_swift(source, where, "function_declaration", swift_factory)


def parse_swift(source: str, where: str = SWIFT_FACADE) -> dict[str, SwiftInitializer]:
    """Return every public initializer of the file by its whole declaration (`SwiftInitializer.key`).

    The Swift grammar of tree-sitter reads the file: a `struct`, `class`, `enum`, `actor` or
    `extension` owns each initializer, and the reader keeps the whole declaration. Text that the
    grammar does not read raises `SyntaxError` with `where` and the line: an error anywhere in the
    file fails before any initializer is read. A node of a declaration that the reader does not
    classify raises `SyntaxError` with the kind of the node. Two initializers with one whole
    declaration raise `SyntaxError` too: no declaration overwrites another.

    The grammar does not know `$` in an identifier (swift-bridge writes `CbWrapper$crawl` and
    `__swift_bridge__$Vec_String$len`), so the tree is parsed from a copy with `_` for each `$`.
    The copy has the byte length of the file (one byte for one byte), so every node range is a
    range of the file, and every text that the reader keeps or compares is read from the file.
    """
    return _parse_swift(source, where, "init_declaration", swift_initializer)


SwiftDeclaration = TypeVar("SwiftDeclaration", bound=SwiftInitializer)


def _parse_swift(
    source: str,
    where: str,
    kind: str,
    reader: Callable[[bytes, Node], SwiftDeclaration | None],
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
    for node in nodes(tree.root_node):
        if node.type != kind:
            continue
        try:
            initializer = reader(data, node)
        except UnreadNodeError as unread:
            raise error(unread.node, str(unread)) from None
        if initializer and initializer.key in initializers:
            raise error(node, f"a second declaration of `{initializer.key}`")
        if initializer:
            initializers[initializer.key] = initializer
    return initializers


def compare_initializer(was: SwiftInitializer, now: SwiftInitializer, tag: str) -> list[str]:
    """Return why the declaration of `now` is not the declaration of `was` with defaulted parameters added.

    The rule is by exclusion: the whole declaration is equal, text by text, but for one permitted
    difference. The head may add parameters, each with a default. Swift skips a defaulted
    parameter whose label the call does not write, so an added parameter is refused only where a
    released call passes an argument with no label: before a released positional (`_`)
    parameter, which a positional call binds to the added one; before a released parameter of
    function type, which a trailing closure of the release may bind, and the added one then takes
    the closure when it has a function type itself; and anywhere when the added parameter has a
    function type, since a trailing closure of the release may bind it. A type that holds a
    function type anywhere (`(() -> Void)?`, `[() -> Void]`) counts as a function type: a false
    failure at worst. Every rule can give a false failure, never a false pass.
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
    if [x for x in head_labels if x in labels] != [x for x in labels if x in head_labels]:
        errors.append(f"{was.owner}: the Swift initializer labels of {tag} are in another order")
    remaining = list(was.parameters)
    for parameter in now.parameters:
        if remaining and parameter.label == remaining[0].label:
            before = remaining.pop(0)
            where = f"{was.owner}.{before.where}"
            if parameter.name != before.name:
                errors.append(f"{where}: the Swift argument name changed from `{before.name}` to `{parameter.name}`")
            if parameter.type != before.type:
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
    return errors


def compare_swift(released: dict[str, SwiftInitializer], head: dict[str, SwiftInitializer], tag: str) -> list[str]:
    """Return why a Swift call written against a released initializer does not compile at the head.

    A released initializer is compared with the head initializer of its owner that differs least,
    and it passes only with no difference. One head initializer may satisfy several released
    ones: each satisfaction is the rule of `compare_initializer`, so every call of each satisfied
    released initializer compiles against it. A removed initializer therefore passes only when a
    kept one of the same owner is its declaration with defaulted parameters added, and that is a
    removal no released call sees. A removed overload whose twin differs anywhere else (the
    generic clause, `async`, the extension) has no head initializer with no difference and fails.
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
    """Print one line for each source and each error. Return the exit status."""
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
    """Return the file of the working tree, or `None` when there is no such file."""
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
    "A Swift line above with a recorded constructor on the struct: the pinned alef writes the new\n"
    "field as a required argument of the public Swift initializer. The fix is in alef, not here."
)


if __name__ == "__main__":
    sys.exit(main())
