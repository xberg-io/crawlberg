#!/usr/bin/env python3
"""Pin the prepared Swift bundle into Package.swift and create the release tag exactly once.

SwiftPM resolves `from:`/`exact:` requirements to the core `vX.Y.Z` tag, so the tagged root
Package.swift must already carry the final checksum. This script runs before the tag exists:
`verify` proves the version-sync commit changes nothing but that checksum, and `finalize`
commits it to the release branch and creates the annotated tag, never moving an existing ref.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
from pathlib import Path

MANIFEST = "Package.swift"
CHECKSUM_FIELD = re.compile(r'(checksum:\s*")([^"]*)(")')
SHA256 = re.compile(r"[0-9a-f]{64}")
COMMIT = re.compile(r"[0-9a-f]{40}")


class ReleaseError(Exception):
    """The prepared release is inconsistent; nothing may be pushed."""


def git(*args: str, cwd: Path | None = None) -> str:
    return subprocess.check_output(["git", *args], cwd=cwd, text=True, encoding="utf-8").strip()


def git_bytes(*args: str, cwd: Path | None = None) -> bytes:
    return subprocess.check_output(["git", *args], cwd=cwd)


def digest(path: Path) -> str:
    with path.open("rb") as handle:
        return hashlib.file_digest(handle, "sha256").hexdigest()


def load_receipt(path: Path) -> dict[str, object]:
    data = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(data, dict):
        raise ReleaseError(f"{path.name} is not a JSON object")
    return data


def verify_manifest(source: str, pinned: str, checksum: str, url: str) -> None:
    """Accept `pinned` only if it is `source` with its one checksum value set to `checksum`."""
    if "__ALEF_" in pinned:
        raise ReleaseError("pinned Package.swift still holds an __ALEF_*__ placeholder")
    fields = CHECKSUM_FIELD.findall(pinned)
    if len(fields) != 1:
        raise ReleaseError(f"pinned Package.swift must hold exactly one checksum field, found {len(fields)}")
    if fields[0][1] != checksum:
        raise ReleaseError(f"pinned checksum {fields[0][1]!r} is not the bundle's SHA-256 {checksum}")
    if pinned.count(f'url: "{url}"') != 1:
        raise ReleaseError(f"pinned Package.swift does not name the release asset URL {url} exactly once")
    if len(CHECKSUM_FIELD.findall(source)) != 1:
        raise ReleaseError("source Package.swift must hold exactly one checksum field")
    expected = CHECKSUM_FIELD.sub(lambda match: f"{match[1]}{checksum}{match[3]}", source)
    if pinned != expected:
        raise ReleaseError("pinned Package.swift changes more than the checksum value")


def verify(repo: Path, zip_path: Path, receipt_path: Path, version: str, repository: str) -> str:
    """Check the working tree holds exactly the checksum pin for this receipt; return the checksum."""
    receipt = load_receipt(receipt_path)
    checksum = digest(zip_path)
    head = git("rev-parse", "HEAD", cwd=repo)
    expected = {
        "repository": repository,
        "version": version,
        "archive": zip_path.name,
        "sha256": checksum,
        "source": head,
    }
    for key, value in expected.items():
        if receipt.get(key) != value:
            raise ReleaseError(f"receipt {key} is {receipt.get(key)!r}, expected {value!r}")
    if not SHA256.fullmatch(checksum):
        raise ReleaseError("bundle digest is not a SHA-256")
    status = git_bytes("status", "--porcelain", "-z", "--untracked-files=all", cwd=repo).decode("utf-8")
    changed = sorted(entry[3:] for entry in status.split("\0") if entry)
    if changed != [MANIFEST]:
        raise ReleaseError(f"version sync must change only {MANIFEST}, changed: {changed}")
    source = git_bytes("show", f"HEAD:{MANIFEST}", cwd=repo).decode("utf-8")
    pinned = (repo / MANIFEST).read_text(encoding="utf-8")
    url = f"https://github.com/{repository}/releases/download/v{version}/{zip_path.name}"
    verify_manifest(source, pinned, checksum, url)
    return checksum


def remote_commit(repo: Path, remote: str, ref: str) -> str | None:
    """Resolve a remote ref to the commit it peels to, or None when it does not exist."""
    # ~keep ls-remote prints the peeled `^{}` line only for a glob, never for the exact ref.
    lines = git("ls-remote", remote, f"{ref}*", cwd=repo).splitlines()
    found = {name: sha for sha, name in (line.split("\t", 1) for line in lines if line)}
    return found.get(f"{ref}^{{}}") or found.get(ref)


def is_pin_of(repo: Path, remote: str, ref: str, *, commit: str, source: str, pinned: bytes) -> bool:
    """True when `commit` is the single checksum commit on top of `source` holding `pinned`."""
    git("fetch", "--quiet", "--no-tags", remote, ref, cwd=repo)
    parents = git("rev-list", "--parents", "-n", "1", commit, cwd=repo).split()[1:]
    if parents != [source]:
        return False
    changed = git("diff", "--no-renames", "--name-only", source, commit, cwd=repo).splitlines()
    return changed == [MANIFEST] and git_bytes("show", f"{commit}:{MANIFEST}", cwd=repo) == pinned


def finalize(repo: Path, tag: str, remote: str, branch: str, dry_run: bool) -> dict[str, str]:
    """Commit the verified pin onto `branch` and create `tag` at it, both create-only.

    Every partial state a failed run leaves (pin pushed, tag not yet created) is one a rerun
    recognises by content and completes; any other state of `branch` or `tag` fails.
    """
    source = git("rev-parse", "HEAD", cwd=repo)
    pinned = (repo / MANIFEST).read_bytes()
    tag_ref = f"refs/tags/{tag}"
    tagged = remote_commit(repo, remote, tag_ref)
    if tagged is not None:
        if not is_pin_of(repo, remote, tag_ref, commit=tagged, source=source, pinned=pinned):
            raise ReleaseError(f"{tag} already exists at {tagged}, which is not the checksum pin of {source}")
        return {"commit": tagged, "tag-created": "false", "branch-updated": "false"}

    tip = remote_commit(repo, remote, f"refs/heads/{branch}")
    if tip == source:
        commit, branch_updated = None, "true"
    elif tip is not None and is_pin_of(repo, remote, f"refs/heads/{branch}", commit=tip, source=source, pinned=pinned):
        commit, branch_updated = tip, "false"
    else:
        raise ReleaseError(f"{branch} moved from the prepared source {source} to {tip}; dispatch the preparation again")

    if commit is None:
        git("add", "--", MANIFEST, cwd=repo)
        git("commit", "--quiet", "-m", f"chore(release): pin the Swift artifact bundle checksum for {tag}", cwd=repo)
        commit = git("rev-parse", "HEAD", cwd=repo)
    if dry_run:
        return {"commit": commit, "tag-created": "false", "branch-updated": "false"}
    if branch_updated == "true":
        git("push", "--quiet", remote, f"{commit}:refs/heads/{branch}", cwd=repo)
    git("tag", "-a", tag, "-m", tag, commit, cwd=repo)
    git("push", "--quiet", remote, tag_ref, cwd=repo)
    if remote_commit(repo, remote, tag_ref) != commit:
        raise ReleaseError(f"{tag} does not peel to {commit} after the push")
    return {"commit": commit, "tag-created": "true", "branch-updated": branch_updated}


def write_outputs(values: dict[str, str]) -> None:
    for key, value in values.items():
        print(f"{key}={value}")
    output = os.environ.get("GITHUB_OUTPUT")
    if output:
        with open(output, "a", encoding="utf-8") as handle:
            handle.writelines(f"{key}={value}\n" for key, value in values.items())


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    check = sub.add_parser("verify", help="check the version-sync checksum pin")
    check.add_argument("--zip", type=Path, required=True)
    check.add_argument("--receipt", type=Path, required=True)
    check.add_argument("--version", required=True)
    check.add_argument("--repository", required=True)
    final = sub.add_parser("finalize", help="push the pin and create the tag, create-only")
    final.add_argument("--tag", required=True)
    final.add_argument("--remote", default="origin")
    final.add_argument("--branch", default="main")
    final.add_argument("--dry-run", action="store_true")
    args = parser.parse_args(argv)
    repo = Path.cwd()
    try:
        if args.command == "verify":
            write_outputs({"checksum": verify(repo, args.zip, args.receipt, args.version, args.repository)})
        else:
            if not re.fullmatch(r"v\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?", args.tag):
                raise ReleaseError(f"{args.tag!r} is not a vX.Y.Z release tag")
            write_outputs(finalize(repo, args.tag, args.remote, args.branch, args.dry_run))
    except (ReleaseError, subprocess.CalledProcessError) as error:
        print(f"::error::{error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
