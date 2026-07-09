#!/usr/bin/env python3
"""Vendor crawlberg core crate into Ruby package.

Used by: ci-ruby.yaml - Vendor crawlberg core crate step

This script:
1. Reads workspace.dependencies from root Cargo.toml
2. Copies core crates to packages/ruby/vendor/
3. Replaces workspace = true with explicit versions
4. Generates vendor/Cargo.toml with proper workspace setup
"""

import os
import re
import shutil
import sys
from pathlib import Path

try:
    import tomllib
except ImportError:
    import tomli as tomllib  # type: ignore[import-untyped]


def get_repo_root() -> Path:
    """Get repository root directory."""
    repo_root_env = os.environ.get("REPO_ROOT")
    if repo_root_env:
        return Path(repo_root_env)

    script_dir = Path(__file__).parent.absolute()
    return (script_dir / ".." / ".." / "..").resolve()


def read_toml(path: Path) -> dict[str, object]:
    """Read TOML file."""
    with open(path, "rb") as f:
        return tomllib.load(f)


def get_workspace_deps(repo_root: Path) -> dict[str, object]:
    """Extract workspace.dependencies from root Cargo.toml."""
    cargo_toml_path = repo_root / "Cargo.toml"
    data = read_toml(cargo_toml_path)
    return data.get("workspace", {}).get("dependencies", {})


def get_workspace_version(repo_root: Path) -> str:
    """Extract version from workspace.package."""
    cargo_toml_path = repo_root / "Cargo.toml"
    data = read_toml(cargo_toml_path)
    return data.get("workspace", {}).get("package", {}).get("version", "4.0.0")


def format_dependency(name: str, dep_spec: object) -> str:
    """Format a dependency spec for Cargo.toml."""
    if isinstance(dep_spec, str):
        return f'{name} = "{dep_spec}"'
    if isinstance(dep_spec, dict):
        version: str = dep_spec.get("version", "")
        package: str | None = dep_spec.get("package")
        features: list[str] = dep_spec.get("features", [])
        default_features: bool | None = dep_spec.get("default-features")

        optional: bool | None = dep_spec.get("optional")

        path: str | None = dep_spec.get("path")
        git: str | None = dep_spec.get("git")
        branch: str | None = dep_spec.get("branch")
        tag: str | None = dep_spec.get("tag")
        rev: str | None = dep_spec.get("rev")

        parts: list[str] = []

        if package:
            parts.append(f'package = "{package}"')

        if git:
            parts.append(f'git = "{git}"')

        if branch:
            parts.append(f'branch = "{branch}"')

        if tag:
            parts.append(f'tag = "{tag}"')

        if rev:
            parts.append(f'rev = "{rev}"')

        if path:
            parts.append(f'path = "{path}"')

        if version:
            parts.append(f'version = "{version}"')

        if features:
            features_str = ", ".join(f'"{f}"' for f in features)
            parts.append(f"features = [{features_str}]")

        if default_features is False:
            parts.append("default-features = false")
        elif default_features is True:
            parts.append("default-features = true")

        if optional is True:
            parts.append("optional = true")
        elif optional is False:
            parts.append("optional = false")

        spec_str = ", ".join(parts)
        return f"{name} = {{ {spec_str} }}"

    return f'{name} = "{dep_spec}"'


def replace_workspace_deps_in_toml(toml_path: Path, workspace_deps: dict[str, object]) -> None:
    """Replace workspace = true with explicit versions in a Cargo.toml file."""
    with open(toml_path) as f:
        content = f.read()

    for name, dep_spec in workspace_deps.items():
        pattern1 = rf"^{re.escape(name)} = \{{ workspace = true \}}$"
        content = re.sub(pattern1, format_dependency(name, dep_spec), content, flags=re.MULTILINE)

        def replace_with_fields(match: re.Match[str]) -> str:
            other_fields_str = match.group(1).strip()
            base_spec = format_dependency(name, dep_spec)
            if " = { " not in base_spec:
                version_val = base_spec.split(" = ", 1)[1].strip('"')
                spec_part = f'version = "{version_val}"'
            else:
                spec_part = base_spec.split(" = { ", 1)[1].rstrip("} ").rstrip("}")

            workspace_fields: dict[str, str] = {}
            bracket_depth = 0
            current_field = ""
            for char in spec_part:
                if char == "[":
                    bracket_depth += 1
                    current_field += char
                elif char == "]":
                    bracket_depth -= 1
                    current_field += char
                elif char == "," and bracket_depth == 0:
                    field = current_field.strip()
                    if field and "=" in field:
                        key, val = field.split("=", 1)
                        workspace_fields[key.strip()] = val.strip()
                    current_field = ""
                else:
                    current_field += char

            if current_field.strip():
                field = current_field.strip()
                if field and "=" in field:
                    key, val = field.split("=", 1)
                    workspace_fields[key.strip()] = val.strip()

            crate_fields: dict[str, str] = {}
            bracket_depth = 0
            current_field = ""
            for char in other_fields_str:
                if char == "[":
                    bracket_depth += 1
                    current_field += char
                elif char == "]":
                    bracket_depth -= 1
                    current_field += char
                elif char == "," and bracket_depth == 0:
                    field = current_field.strip()
                    if field and "=" in field:
                        key, val = field.split("=", 1)
                        crate_fields[key.strip()] = val.strip()
                    current_field = ""
                else:
                    current_field += char

            if current_field.strip():
                field = current_field.strip()
                if field and "=" in field:
                    key, val = field.split("=", 1)
                    crate_fields[key.strip()] = val.strip()

            merged_fields = {**workspace_fields, **crate_fields}

            merged_parts = [f"{k} = {v}" for k, v in merged_fields.items()]
            merged_spec = ", ".join(merged_parts)

            return f"{name} = {{ {merged_spec} }}"

        pattern2 = rf"^{re.escape(name)} = \{{ workspace = true, (.+?) \}}$"
        content = re.sub(pattern2, replace_with_fields, content, flags=re.MULTILINE | re.DOTALL)

    with open(toml_path, "w") as f:
        f.write(content)


def generate_vendor_cargo_toml(
    repo_root: Path, workspace_deps: dict[str, object], core_version: str, copied_crates: list[str]
) -> None:
    """Generate vendor/Cargo.toml with workspace setup.

    Args:
        repo_root: Repository root directory
        workspace_deps: Workspace dependencies from Cargo.toml
        core_version: Core version string
        copied_crates: List of crates that were successfully copied
    """
    deps_lines: list[str] = []
    for name, dep_spec in sorted(workspace_deps.items()):
        deps_lines.append(format_dependency(name, dep_spec))

    deps_str = "\n".join(deps_lines)

    members = [
        name
        for name in [
            "crawlberg",
            "crawlberg-ffi",
            "crawlberg-tesseract",
            "crawlberg-paddle-ocr",
            "crawlberg-pdfium-render",
            "rb-sys",
        ]
        if name in copied_crates
    ]
    members_str = ", ".join(f'"{m}"' for m in members)

    vendor_toml = f"""[workspace]
members = [{members_str}]

[workspace.package]
version = "{core_version}"
edition = "2024"
rust-version = "1.91"
authors = ["Na'aman Hirschfeld <naaman@crawlberg.dev>"]
license = "MIT"
repository = "https://github.com/xberg-io/crawlberg"
homepage = "https://crawlberg.dev"

[workspace.dependencies]
{deps_str}
"""

    vendor_dir = repo_root / "packages" / "ruby" / "vendor"
    vendor_dir.mkdir(parents=True, exist_ok=True)

    toml_path = vendor_dir / "Cargo.toml"
    with open(toml_path, "w") as f:
        f.write(vendor_toml)


def main() -> None:
    """Main vendoring function."""
    repo_root: Path = get_repo_root()

    print("=== Vendoring crawlberg core crate ===")

    workspace_deps: dict[str, object] = get_workspace_deps(repo_root)
    core_version: str = get_workspace_version(repo_root)

    print(f"Core version: {core_version}")
    print(f"Workspace dependencies: {len(workspace_deps)}")

    vendor_base: Path = repo_root / "packages" / "ruby" / "vendor"

    crate_names = [
        "crawlberg",
        "crawlberg-ffi",
        "crawlberg-tesseract",
        "crawlberg-paddle-ocr",
        "crawlberg-pdfium-render",
        "rb-sys",
    ]
    for name in crate_names:
        crate_path = vendor_base / name
        if crate_path.exists():
            shutil.rmtree(crate_path)
    vendor_cargo = vendor_base / "Cargo.toml"
    if vendor_cargo.exists():
        vendor_cargo.unlink()
    print("Cleaned vendor crate directories")

    vendor_base.mkdir(parents=True, exist_ok=True)

    crates_to_copy: list[tuple[str, str]] = [
        ("crates/crawlberg", "crawlberg"),
        ("crates/crawlberg-ffi", "crawlberg-ffi"),
        ("crates/crawlberg-tesseract", "crawlberg-tesseract"),
        ("crates/crawlberg-paddle-ocr", "crawlberg-paddle-ocr"),
        ("crates/crawlberg-pdfium-render", "crawlberg-pdfium-render"),
        ("vendor/rb-sys", "rb-sys"),
    ]

    copied_crates: list[str] = []
    for src_rel, dest_name in crates_to_copy:
        src: Path = repo_root / src_rel
        dest: Path = vendor_base / dest_name
        if src.exists():
            try:
                shutil.copytree(src, dest)
                copied_crates.append(dest_name)
                print(f"Copied {dest_name}")
            except OSError as e:
                print(f"Warning: Failed to copy {dest_name}: {e}", file=sys.stderr)
        else:
            print(f"Warning: Source directory not found: {src_rel}")

    artifact_dirs: list[str] = [".fastembed_cache", "target"]
    temp_patterns: list[str] = ["*.swp", "*.bak", "*.tmp", "*~"]

    for crate_dir in copied_crates:
        crate_path: Path = vendor_base / crate_dir
        if crate_path.exists():
            for artifact_dir in artifact_dirs:
                artifact: Path = crate_path / artifact_dir
                if artifact.exists():
                    shutil.rmtree(artifact)

            for pattern in temp_patterns:
                for f in crate_path.rglob(pattern):
                    f.unlink()

    print("Cleaned build artifacts")

    for crate_dir in copied_crates:
        crate_toml = vendor_base / crate_dir / "Cargo.toml"
        if crate_toml.exists():
            with open(crate_toml) as f:
                content = f.read()

            content = re.sub(r"^version\.workspace = true$", f'version = "{core_version}"', content, flags=re.MULTILINE)
            content = re.sub(r"^edition\.workspace = true$", 'edition = "2024"', content, flags=re.MULTILINE)
            content = re.sub(r"^rust-version\.workspace = true$", 'rust-version = "1.91"', content, flags=re.MULTILINE)
            content = re.sub(
                r"^authors\.workspace = true$",
                'authors = ["Na\'aman Hirschfeld <naaman@crawlberg.dev>"]',
                content,
                flags=re.MULTILINE,
            )
            content = re.sub(r"^license\.workspace = true$", 'license = "MIT"', content, flags=re.MULTILINE)

            with open(crate_toml, "w") as f:
                f.write(content)

            replace_workspace_deps_in_toml(crate_toml, workspace_deps)
            print(f"Updated {crate_dir}/Cargo.toml")

    if "crawlberg-ffi" in copied_crates and "crawlberg" in copied_crates:
        ffi_toml = vendor_base / "crawlberg-ffi" / "Cargo.toml"
        if ffi_toml.exists():
            with open(ffi_toml) as f:
                content = f.read()

            content = re.sub(
                r'(crawlberg = \{) (?:(?:path|version) = "[^"]*", )?', r'\1 path = "../crawlberg", ', content
            )

            with open(ffi_toml, "w") as f:
                f.write(content)

    if "crawlberg" in copied_crates:
        crawlberg_toml = vendor_base / "crawlberg" / "Cargo.toml"
        if crawlberg_toml.exists():
            with open(crawlberg_toml) as f:
                content = f.read()

            if "crawlberg-tesseract" in copied_crates:
                content = re.sub(
                    r'crawlberg-tesseract = \{ (?:path = "[^"]*", )?version = "[^"]*", optional = true \}',
                    'crawlberg-tesseract = { path = "../crawlberg-tesseract", optional = true }',
                    content,
                )
            if "crawlberg-paddle-ocr" in copied_crates:
                content = re.sub(
                    r'crawlberg-paddle-ocr = \{ (?:path = "[^"]*", )?version = "[^"]*", optional = true \}',
                    'crawlberg-paddle-ocr = { path = "../crawlberg-paddle-ocr", optional = true }',
                    content,
                )
            if "crawlberg-pdfium-render" in copied_crates:
                content = re.sub(
                    r'pdfium-render = \{ package = "crawlberg-pdfium-render", (?:path = "[^"]*", )?version = "[^"]*"',
                    'pdfium-render = { package = "crawlberg-pdfium-render", path = "../crawlberg-pdfium-render"',
                    content,
                )

            with open(crawlberg_toml, "w") as f:
                f.write(content)

    generate_vendor_cargo_toml(repo_root, workspace_deps, core_version, copied_crates)
    print("Generated vendor/Cargo.toml")

    native_toml = repo_root / "packages" / "ruby" / "ext" / "crawlberg_rb" / "native" / "Cargo.toml"
    if native_toml.exists():
        with open(native_toml) as f:
            content = f.read()

        content = re.sub(
            r'path = "\.\./\.\./\.\./\.\./\.\./crates/crawlberg"', 'path = "../../../vendor/crawlberg"', content
        )
        content = re.sub(
            r'path = "\.\./\.\./\.\./\.\./\.\./crates/crawlberg-ffi"',
            'path = "../../../vendor/crawlberg-ffi"',
            content,
        )

        with open(native_toml, "w") as f:
            f.write(content)

        print("Updated native extension Cargo.toml to use vendored crates")

    print(f"\nVendoring complete (core version: {core_version})")
    print(f"Copied crates: {', '.join(sorted(copied_crates))}")

    if "crawlberg" in copied_crates and "crawlberg-ffi" in copied_crates:
        print("Native extension Cargo.toml uses:")
        print("  - path '../../../vendor/crawlberg' for crawlberg crate")
        print("  - path '../../../vendor/crawlberg-ffi' for crawlberg-ffi crate")
        if "rb-sys" in copied_crates:
            print("  - path '../../../vendor/rb-sys' for rb-sys crate")
        else:
            print("  - rb-sys from crates.io")
    else:
        print("Warning: Some required crates were not copied. Check for missing source directories.")


if __name__ == "__main__":
    try:
        main()
    except Exception as e:
        print(f"Error: {e}", file=sys.stderr)
        sys.exit(1)
