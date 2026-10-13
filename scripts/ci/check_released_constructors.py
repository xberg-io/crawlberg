#!/usr/bin/env python3
"""Check that every constructor of the last release still works at the head.

The generated Python stub lists, for each bound type, the bound fields in struct order and the
native constructor. The check compares the stub of the newest release tag with the stub of the
working tree. The Swift facade is compared the same way, because it states each default in its
source. Run it from the repository root. It needs git and the tags, and no Rust toolchain.

The check fails closed. `check_source` applies one rule to every source in `SOURCES`:

- A source that exists at the release tag must give at least one constructor at the release and
  at the head, and every released type must be found at the head. If not, the check fails and
  names the source, the two counts and the first missing type.
- A source that exists at the release tag and is absent at the head fails.
- A source that is absent at the release tag is not compared. The output says so in a line
  `not compared: <path> absent at <tag>`. If no source is compared, the check fails.
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
from collections.abc import Callable
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, NamedTuple

STUB = "packages/python/crawlberg/_crawlberg.pyi"
SWIFT_FACADE = "packages/swift/Sources/Crawlberg/Crawlberg.swift"
RELEASE_TAG = re.compile(r"^v\d+\.\d+\.\d+$")
SWIFT_STRUCT = re.compile(r"^public struct (\w+)\b")
SWIFT_INIT = re.compile(r"^    public init\((.*)\) \{$")
SWIFT_ANY_INIT = re.compile(r"^\s*public init\b")
# ~keep The `Decodable` initializer is not a call that a caller writes with labels.
SWIFT_DECODER_INIT = re.compile(r"^\s*public init\(from \w+: (any )?Decoder\) throws \{$")


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


def split_swift_parameters(parameters: str) -> list[str]:
    """Split at each comma that is not in brackets or in a string literal."""
    parts, depth, quoted, start = [], 0, False, 0
    for index, character in enumerate(parameters):
        if character == '"' and parameters[index - 1 : index] != "\\":
            quoted = not quoted
        elif not quoted and character in "([<":
            depth += 1
        elif not quoted and character in ")]>":
            depth -= 1
        elif not quoted and depth == 0 and character == ",":
            parts.append(parameters[start:index].strip())
            start = index + 1
    return [*parts, parameters[start:].strip()] if parameters.strip() else []


def parse_swift_facade(source: str, where: str = SWIFT_FACADE) -> dict[str, dict[str, bool]]:
    """Return, for each public struct, the labels of its memberwise initializer: label -> has a default.

    A public initializer in a form that this reader does not know raises `SyntaxError` with
    `where` and the line. The reader does not skip it: a skipped initializer is not compared.
    """
    types: dict[str, dict[str, bool]] = {}
    name = None
    for number, line in enumerate(source.splitlines(), start=1):
        if declared := SWIFT_STRUCT.match(line):
            name = declared[1]
        elif init := SWIFT_INIT.match(line):
            if name and name not in types and not init[1].startswith("from "):
                parameters = split_swift_parameters(init[1])
                types[name] = {parameter.split(":")[0]: " = " in parameter for parameter in parameters}
        elif SWIFT_ANY_INIT.match(line) and not SWIFT_DECODER_INIT.match(line):
            message = "a public initializer in a form that the check does not read"
            raise SyntaxError(message, (where, number, 1, line))
    return types


def compare_swift(released: dict[str, dict[str, bool]], head: dict[str, dict[str, bool]], tag: str) -> list[str]:
    """Return why a Swift call with the labels of the released initializer does not compile at the head."""
    errors = []
    for name, labels in released.items():
        now = head.get(name)
        if now is None:
            errors.append(f"{name}: the Swift struct of {tag} has no public initializer at the head")
            continue
        if [label for label in now if label in labels] != [label for label in labels if label in now]:
            errors.append(f"{name}: the Swift initializer labels of {tag} are in another order")
        for label, had_default in labels.items():
            if label not in now:
                errors.append(f"{name}.{label}: the Swift initializer argument of {tag} is removed or renamed")
            elif had_default and not now[label]:
                errors.append(f"{name}.{label}: the Swift argument has a default in {tag} and is required now")
        for label in (label for label, has_default in now.items() if label not in labels and not has_default):
            errors.append(f"{name}.{label}: the new Swift initializer argument is required: no call of {tag} compiles")
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


# ~keep The stub does not show a typed facade that drops the recorded default. The Swift
# facade states each default in its source, so it is compared with no Swift toolchain.
SOURCES = (Source(STUB, parse_stub, compare), Source(SWIFT_FACADE, parse_swift_facade, compare_swift))


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
        return Result(f"not compared: {path} absent at {tag}", [], None)
    if item.head_text is None:
        error = f"{path} exists at {tag} and {item.head_path} does not exist at the head"
        return Result(f"{path}: {tag} has the file, the head ({item.head_path}) does not", [error], 0)
    try:
        released = item.source.parse(item.released_text, f"{tag}:{path}")
        head = item.source.parse(item.head_text, item.head_path)
    except SyntaxError as error:
        found = f"{error.filename}:{error.lineno}: parse error: {error.msg}"
        return Result(f"{path}: {tag} and the head ({item.head_path}) are not compared: parse error", [found], 0)
    missing = [name for name in released if name not in head]
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
        errors.append(f"no source exists at {tag}. The check does not use an older release: nothing was compared")
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
    parser.add_argument("--stub", default=STUB, help="the stub of the head (default: %(default)s)")
    parser.add_argument("--swift", default=SWIFT_FACADE, help="the Swift facade of the head (default: %(default)s)")
    options = parser.parse_args()

    tag = newest_release_tag(git("tag", "--list", "v[0-9]*").split())
    if tag is None:
        print("::error::this checkout has no release tag `vX.Y.Z`. Fetch the tags: this check would compare nothing")
        return 1
    head_paths = (options.stub, options.swift)
    return run(
        tag,
        [
            Input(source, released_stub(tag, source.path), head_path, read_head(head_path))
            for source, head_path in zip(SOURCES, head_paths, strict=True)
        ],
    )


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
