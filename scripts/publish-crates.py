#!/usr/bin/env python3
"""Publish Tokyo workspace crates in dependency order."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import subprocess
import sys
import time
import tomllib
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen


ROOT = Path(__file__).resolve().parent.parent
PACKAGES = [
    "tokyo-ir",
    "tokyo-cli-runtime",
    "tokyo-import-openapi",
    "tokyo-codegen-engine",
    "tokyo-emit-cli",
    "tokyo-codegen",
]
USER_AGENT = "tokyo-release (https://github.com/tokyo-corp/tokyo)"


def workspace_version() -> str:
    with (ROOT / "Cargo.toml").open("rb") as manifest:
        return tomllib.load(manifest)["workspace"]["package"]["version"]


def version_exists(package: str, version: str) -> bool:
    request = Request(
        f"https://crates.io/api/v1/crates/{package}/{version}",
        headers={"User-Agent": USER_AGENT},
    )
    try:
        with urlopen(request, timeout=20) as response:
            json.load(response)
        return True
    except HTTPError as error:
        if error.code == 404:
            return False
        raise


def wait_until_available(package: str, version: str, timeout: int) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            if version_exists(package, version):
                return
        except URLError:
            pass
        time.sleep(5)
    raise RuntimeError(
        f"{package} {version} did not become available on crates.io "
        f"within {timeout} seconds"
    )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="run Cargo package verification without uploading",
    )
    parser.add_argument(
        "--propagation-timeout",
        type=int,
        default=300,
        help="seconds to wait for each published crate to reach the index",
    )
    args = parser.parse_args()

    version = workspace_version()
    subprocess.run(
        [sys.executable, "scripts/verify-release.py", "--tag", f"v{version}"],
        cwd=ROOT,
        check=True,
    )

    for package in PACKAGES:
        if not args.dry_run and version_exists(package, version):
            print(f"skipping {package} {version}: already published")
            continue

        command = ["cargo", "publish", "--locked", "-p", package]
        if args.dry_run:
            command.append("--dry-run")
        print(f"{'verifying' if args.dry_run else 'publishing'} {package} {version}")
        subprocess.run(command, cwd=ROOT, check=True)

        if not args.dry_run:
            wait_until_available(package, version, args.propagation_timeout)


if __name__ == "__main__":
    try:
        main()
    except (HTTPError, URLError, RuntimeError) as error:
        print(f"crate publication failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error
