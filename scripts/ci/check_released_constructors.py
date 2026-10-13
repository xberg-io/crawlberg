#!/usr/bin/env python3
"""Check that every constructor of the last release still works at the head.

The generated Python stub lists, for each bound type, the bound fields in struct order and the
native constructor. The check compares the stub of the newest release tag with the stub of the
working tree. Run it from the repository root. It needs git and the tags, and no Rust toolchain.
"""

from __future__ import annotations

import argparse
import ast
import re
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path

STUB = "packages/python/crawlberg/_crawlberg.pyi"
RELEASE_TAG = re.compile(r"^v\d+\.\d+\.\d+$")


@dataclass(frozen=True)
class Parameter:
    name: str
    annotation: str
    has_default: bool


@dataclass
class BoundType:
    # ~keep `fields` is the struct order: the public Python dataclass, Kotlin and Go follow it.
    # `positional` is the native constructor: PHP, WebAssembly, Java, Dart and Zig follow it.
    fields: dict[str, str] = field(default_factory=dict)
    positional: list[Parameter] = field(default_factory=list)
    keyword_only: list[str] = field(default_factory=list)


def parse_stub(source: str) -> dict[str, BoundType]:
    """Return the types of the stub that have a constructor, by name."""
    types: dict[str, BoundType] = {}
    for node in ast.parse(source).body:
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
        arguments = [*init.args.posonlyargs, *init.args.args][1:]
        first_default = len(arguments) - len(init.args.defaults)
        for index, argument in enumerate(arguments):
            annotation = ast.unparse(argument.annotation) if argument.annotation else ""
            bound.positional.append(Parameter(argument.arg, annotation, index >= first_default))
        bound.keyword_only = [argument.arg for argument in init.args.kwonlyargs]
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
    released_names = {parameter.name for parameter in released.positional}
    errors = [
        f"{name}.{parameter.name}: the new field is positional argument {slot + 1} of the constructor, "
        f"with no recorded constructor of {tag}"
        for slot, parameter in enumerate(head.positional)
        if parameter.name not in released_names
    ]
    if not errors and head.positional != released.positional:
        was = ", ".join(parameter.name for parameter in released.positional)
        now = ", ".join(parameter.name for parameter in head.positional)
        errors.append(f"{name}: the positional arguments of the constructor changed from ({was}) to ({now})")
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


def git(*arguments: str) -> str:
    return subprocess.run(["git", *arguments], check=True, capture_output=True, text=True).stdout


def newest_release_tag() -> str | None:
    tags = git("tag", "--list", "v[0-9]*", "--merged", "HEAD", "--sort=-version:refname").split()
    return next((tag for tag in tags if RELEASE_TAG.match(tag)), None)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--stub", default=STUB, help="the stub of the head (default: %(default)s)")
    options = parser.parse_args()

    tag = newest_release_tag()
    if tag is None:
        print(
            "::error::no release tag `vX.Y.Z` is reachable from HEAD. Fetch the tags: this check would compare nothing"
        )
        return 1
    released = parse_stub(git("show", f"{tag}:{STUB}"))
    head = parse_stub(Path(options.stub).read_text(encoding="utf-8"))
    print(f"released constructors: {tag} has {len(released)} bound types, {options.stub} has {len(head)}")
    if not released:
        print(f"::error::{STUB} at {tag} has no type with a constructor. Nothing was compared")
        return 1

    errors = compare(released, head, tag)
    if not errors:
        print(f"ok: the {len(released)} constructors of {tag} are kept")
        return 0
    for error in errors:
        print(f"::error::{error}")
    print(
        f"A call written against {tag} must still compile and mean the same. For each field above, do one of:\n"
        "  1. Keep the field out of the bindings: put `#[cfg_attr(alef, alef(skip))]` on it.\n"
        "  2. Move the field to the end of the struct and record the released fields on the struct:\n"
        '     `#[cfg_attr(alef, alef(legacy_constructor_fields = ["..."]))]`. The pinned alef must accept the type of the field.\n'
        f"  3. Restore the field list of {tag}: the same names, types and order.\n"
        "Then run `task alef:generate`."
    )
    return 1


if __name__ == "__main__":
    sys.exit(main())
