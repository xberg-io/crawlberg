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
        f"  3. Restore the field list of {tag}: the same names, types, order and defaults.\n"
        "Then run `task alef:generate`."
    )
    return 1


if __name__ == "__main__":
    sys.exit(main())
