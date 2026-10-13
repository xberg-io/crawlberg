#!/usr/bin/env python3
"""Check that every constructor of the last release still works at the head.

The generated Python stub lists, for each bound type, the bound fields in struct order and the
native constructor. The check compares the stub of the newest release tag with the stub of the
working tree. The Swift sources are compared the same way: every public initializer of every
type, read with the Swift grammar of tree-sitter (`tree-sitter-swift`, pinned in
`scripts/ci/requirements-released-constructors.txt`). Run it from the repository root. It needs
git and the tags, and no Rust or Swift toolchain.

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
from collections.abc import Callable, Iterator
from dataclasses import dataclass, field, replace
from pathlib import Path
from typing import Any, NamedTuple

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
    # ~keep The text after the colon: the type with `inout` and `@escaping`, as a call sees it.
    type: str
    default: str | None

    @property
    def declaration(self) -> str:
        head = self.label if self.label == self.name else f"{self.label} {self.name}"
        return f"{head}: {self.type}"


@dataclass(frozen=True)
class SwiftInitializer:
    owner: str
    parameters: tuple[SwiftParameter, ...]
    # ~keep `?`, `async`, `throws`: a call written without one does not compile when it is added.
    effects: tuple[str, ...]

    @property
    def key(self) -> str:
        """The signature as a caller reads it: `Owner(label: Type, ...)`."""
        return f"{self.owner}({', '.join(parameter.declaration for parameter in self.parameters)})"

    @property
    def form(self) -> str:
        return "init" + "".join(effect if effect == "?" else f" {effect}" for effect in self.effects)


def nodes(node: Node) -> Iterator[Node]:
    yield node
    for child in node.children:
        yield from nodes(child)


def text(node: Node) -> str:
    return node.text.decode() if node.text else ""


def swift_initializer(node: Node) -> SwiftInitializer | None:
    """Return the public initializer that `node` declares, or `None` when a caller cannot call it.

    A caller cannot call an initializer that is not `public`, that `@available` marks
    `unavailable`, or that a protocol requires.
    """
    modifiers = next((child for child in node.children if child.type == "modifiers"), None)
    modifier_texts = [text(child) for child in modifiers.children] if modifiers else []
    if "public" not in modifier_texts and "open" not in modifier_texts:
        return None
    if any(item.startswith("@available") and "unavailable" in item for item in modifier_texts):
        return None
    owner = node.parent
    while owner is not None and owner.type not in ("class_declaration", "protocol_declaration"):
        owner = owner.parent
    if owner is None or owner.type == "protocol_declaration":
        return None
    name = owner.child_by_field_name("name")
    parameters: list[SwiftParameter] = []
    effects = []
    for index, child in enumerate(node.children):
        if child.type == "parameter":
            internal = child.child_by_field_name("name")
            assert internal is not None
            external = child.child_by_field_name("external_name") or internal
            declared = text(child)[internal.end_byte - child.start_byte :].removeprefix(":").strip()
            parameters.append(SwiftParameter(text(external), text(internal), declared, None))
        elif node.field_name_for_child(index) == "default_value":
            parameters[-1] = replace(parameters[-1], default=text(child))
        elif child.type in ("?", "async", "throws", "rethrows"):
            effects.append(text(child))
    return SwiftInitializer(text(name) if name else "", tuple(parameters), tuple(effects))


def parse_swift(source: str, where: str = SWIFT_FACADE) -> dict[str, SwiftInitializer]:
    """Return every public initializer of the file by its signature `Owner(label: Type, ...)`.

    The Swift grammar of tree-sitter reads the file: a `struct`, `class`, `enum` or `extension`
    owns each initializer, whatever its attributes, modifiers and generic parameters. Text that
    the grammar does not read raises `SyntaxError` with `where` and the line: an `ERROR` or a
    missing node anywhere in the file fails. The grammar does not know `$` in an identifier
    (swift-bridge writes `__swift_bridge__$Vec_String$len`), so the reader reads `$` as `_`.
    """
    tree = Parser(SWIFT).parse(source.replace("$", "_").encode())
    initializers: dict[str, SwiftInitializer] = {}
    for node in nodes(tree.root_node):
        if node.type == "ERROR" or node.is_missing:
            row, column = node.start_point
            line = source.splitlines()[row] if row < len(source.splitlines()) else ""
            raise SyntaxError("text that the Swift grammar does not read", (where, row + 1, column + 1, line))
        if node.type == "init_declaration" and (initializer := swift_initializer(node)):
            initializers[initializer.key] = initializer
    return initializers


def compare_initializer(was: SwiftInitializer, now: SwiftInitializer, tag: str) -> list[str]:
    """Return why a call written against `was` does not compile against `now`, or bind another value."""
    errors = []
    labels, head_labels = [p.label for p in was.parameters], [p.label for p in now.parameters]
    if [x for x in head_labels if x in labels] != [x for x in labels if x in head_labels]:
        errors.append(f"{was.owner}: the Swift initializer labels of {tag} are in another order")
    remaining = list(was.parameters)
    for parameter in now.parameters:
        where = f"{was.owner}.{parameter.name}"
        if remaining and parameter.label == remaining[0].label:
            before = remaining.pop(0)
            if parameter.type != before.type:
                errors.append(f"{where}: the Swift argument type changed from `{before.type}` to `{parameter.type}`")
            if before.default is not None and parameter.default is None:
                errors.append(f"{where}: the Swift argument has a default in {tag} and is required now")
            elif before.default is not None and parameter.default != before.default:
                changed = f"changed from `{before.default}` to `{parameter.default}`"
                errors.append(f"{where}: the Swift argument default {changed}")
        elif parameter.default is None:
            errors.append(f"{where}: the new Swift initializer argument is required: no call of {tag} compiles")
    removed = f"the Swift initializer argument of {tag} is removed or renamed"
    errors.extend(f"{was.owner}.{parameter.name}: {removed}" for parameter in remaining)
    if now.effects != was.effects:
        errors.append(f"{was.owner}: the Swift initializer of {tag} is `{was.form}`, the head is `{now.form}`")
    return errors


def compare_swift(released: dict[str, SwiftInitializer], head: dict[str, SwiftInitializer], tag: str) -> list[str]:
    """Return why a Swift call written against a released initializer does not compile at the head.

    A released initializer is compared with the head initializer of its owner that differs least.
    """
    errors = []
    lost: set[str] = set()
    for was in released.values():
        candidates = [now for now in head.values() if now.owner == was.owner]
        if candidates:
            errors.extend(min((compare_initializer(was, now, tag) for now in candidates), key=len))
        elif was.owner not in lost:
            lost.add(was.owner)
            errors.append(f"{was.owner}: the Swift type of {tag} has no public initializer at the head")
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
    # ~keep A key is a type, or a type and its signature in parentheses. The rule is on the type:
    # the signature of a kept initializer changes when a parameter with a default is added.
    owners = {name.partition("(")[0] for name in head}
    missing = [name.partition("(")[0] for name in released if name.partition("(")[0] not in owners]
    kept = sum(1 for name, was in released.items() if not item.source.compare({name: was}, head, tag))
    counts = f"{tag} has {len(released)} constructors, the head ({item.head_path}) has {len(head)}"
    errors = []
    if not released or not head or missing:
        first = f", the first missing type is `{missing[0]}`" if missing else ""
        errors.append(f"{path}: {counts}{first}. The check does not pass on a source that it cannot read in full")
    errors.extend(item.source.compare(released, head, tag))
    return Result(f"{path}: {counts}, {kept} kept", errors, kept)


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
