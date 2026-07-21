#!/usr/bin/env python3
"""Validate Tokyo workspace metadata before a release."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import tomllib


ROOT = Path(__file__).resolve().parent.parent
EXPECTED_PACKAGES = {
    "tokyo-ir",
    "tokyo-import-openapi",
    "tokyo-codegen-engine",
    "tokyo-emit-cli",
    "tokyo-cli-runtime",
    "tokyo-cli",
}
EXPECTED_REPOSITORY = "https://github.com/tokyo-corp/tokyo"


def fail(message: str) -> None:
    print(f"release validation failed: {message}", file=sys.stderr)
    raise SystemExit(1)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--tag",
        default=None,
        help="release tag; defaults to GITHUB_REF_NAME when GITHUB_REF_TYPE is 'tag'",
    )
    args = parser.parse_args()
    tag = args.tag
    if tag is None and os.environ.get("GITHUB_REF_TYPE") == "tag":
        tag = os.environ.get("GITHUB_REF_NAME")

    with (ROOT / "Cargo.toml").open("rb") as manifest:
        workspace = tomllib.load(manifest)
    version = workspace["workspace"]["package"]["version"]
    expected_tag = f"v{version}"
    if tag and tag != expected_tag:
        fail(f"tag {tag!r} does not match workspace version {version!r}")

    metadata = json.loads(
        subprocess.check_output(
            ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"],
            cwd=ROOT,
            text=True,
        )
    )
    packages = {package["name"]: package for package in metadata["packages"]}
    missing = EXPECTED_PACKAGES - packages.keys()
    unexpected = packages.keys() - EXPECTED_PACKAGES
    if missing or unexpected:
        fail(
            f"workspace package mismatch; missing={sorted(missing)}, "
            f"unexpected={sorted(unexpected)}"
        )

    for name in sorted(EXPECTED_PACKAGES):
        package = packages[name]
        if package["version"] != version:
            fail(f"{name} has version {package['version']}, expected {version}")
        if package["license"] != "MIT":
            fail(f"{name} must declare the MIT license")
        if package["repository"] != EXPECTED_REPOSITORY:
            fail(f"{name} has unexpected repository {package['repository']!r}")
        if not package["description"]:
            fail(f"{name} must have a crates.io description")
        if package.get("publish") != ["crates-io"]:
            fail(f"{name} must restrict publishing to crates.io")

    print(f"release metadata is valid for {expected_tag}")


if __name__ == "__main__":
    main()
