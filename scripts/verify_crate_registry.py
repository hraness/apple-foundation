#!/usr/bin/env python3
"""Require the registry to serve the exact prepared, available crate archive."""

import json
import re
import sys
from pathlib import Path


def verify_registry_archive(response, name, version, checksum):
    if not re.fullmatch(r"[0-9a-f]{64}", checksum):
        raise ValueError("expected archive checksum must be SHA-256")
    published = response.get("version", {})
    if published.get("crate") != name or published.get("num") != version:
        raise ValueError("registry crate name or version does not match")
    if published.get("yanked") is not False:
        raise ValueError("registry version is yanked or lacks yanked status")
    if published.get("checksum") != checksum:
        raise ValueError("registry archive differs from the verified source")


if __name__ == "__main__":
    name, version, checksum, response_path = sys.argv[1:]
    verify_registry_archive(json.loads(Path(response_path).read_text()), name, version, checksum)
    print(f"Verified {name} {version} registry archive")
