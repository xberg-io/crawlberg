"""Check the independently versioned browser fork without converting registry errors to missing."""

from __future__ import annotations

import json
import os
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path


def registry_exists(version: str) -> bool:
    url = f"https://crates.io/api/v1/crates/crawlberg-chromiumoxide/{urllib.parse.quote(version, safe='')}"
    request = urllib.request.Request(url, headers={"User-Agent": "crawlberg-release/1", "Accept": "application/json"})
    try:
        with urllib.request.urlopen(request, timeout=30) as response:  # noqa: S310  # ~keep Fixed HTTPS origin and escaped path.
            if response.status != 200:
                raise ValueError(f"Registry returned unexpected HTTP status {response.status}")
            payload = json.load(response)
    except urllib.error.HTTPError as error:
        error.close()
        if error.code == 404:
            return False
        raise
    package = payload.get("version") if isinstance(payload, dict) else None
    if (
        not isinstance(package, dict)
        or package.get("crate") != "crawlberg-chromiumoxide"
        or package.get("num") != version
    ):
        raise ValueError("Registry returned an unexpected browser fork package identity or version")
    return True


def main() -> None:
    version = os.environ.get("FORK_VERSION", "")
    output = os.environ.get("GITHUB_OUTPUT", "")
    if not version.strip() or any(character.isspace() for character in version) or not output.strip():
        raise ValueError("FORK_VERSION and GITHUB_OUTPUT must be nonempty; the version must not contain whitespace")
    exists = registry_exists(version)
    with Path(output).open("a") as stream:
        stream.write(f"exists={str(exists).lower()}\n")


if __name__ == "__main__":
    main()
