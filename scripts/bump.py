#!/usr/bin/env python3
"""The bump gate's command: move PATCH once, validate it, commit it.

`bump` used to be a harness step whose session ran the version writer and then
committed by hand. The work is mechanical, so this script is the whole step: it
guards the tree, runs `scripts/version.py bump patch` and its `check`, and
commits the writer's staged files as one commit.

The gate runs from the repository root, on `main`, before the merge queue
starts. It never tags and never pushes: a release is a person's action.
"""

from __future__ import annotations

import subprocess
import sys
import tomllib
from pathlib import Path
from typing import NoReturn

ROOT = Path(__file__).resolve().parent.parent
VERSION_FILE = ROOT / "version.toml"
WRITER = ROOT / "scripts" / "version.py"


def fail(message: str) -> NoReturn:
    raise SystemExit(f"bump: {message}")


def git(*args: str) -> str:
    """Run git from the repository root and return its stdout."""
    result = subprocess.run(
        ["git", *args], cwd=ROOT, text=True, capture_output=True
    )
    if result.returncode != 0:
        fail(f"git {' '.join(args)} failed: {result.stderr.strip()}")
    return result.stdout


def run(command: list[str]) -> None:
    """Run a command from the root, forwarding its output; stop when it fails."""
    result = subprocess.run(command, cwd=ROOT)
    if result.returncode != 0:
        raise SystemExit(result.returncode)


def dirty_paths(status: str) -> list[str]:
    """The paths `git status --porcelain -z` reports, in its own order."""
    fields = [field for field in status.split("\0") if field]
    paths: list[str] = []
    index = 0
    while index < len(fields):
        field = fields[index]
        index += 1
        if len(field) < 4:
            continue
        paths.append(field[3:])
        if field[0] in ("R", "C"):
            # A rename or copy carries its source as a second field.
            index += 1
    return paths


def is_engine_path(path: str) -> bool:
    """Whether a path lives under `.scratch/`, the issue area the engine writes.

    This mirrors `prepare::require_clean`/`is_engine_path`: a first component of
    exactly `.scratch`, including the collapsed untracked `?? .scratch/` entry.
    """
    return Path(path).parts[:1] == (".scratch",)


def require_root() -> None:
    top = Path(git("rev-parse", "--show-toplevel").strip()).resolve()
    if top != ROOT:
        fail(f"the bump runs from {ROOT}, but git's top level is {top}")


def require_main() -> None:
    branch = git("rev-parse", "--abbrev-ref", "HEAD").strip()
    if branch != "main":
        if branch == "HEAD":
            fail("HEAD is detached; the bump commits on main")
        fail(f"the bump commits on main, not {branch}")


def require_writer() -> None:
    if not WRITER.is_file():
        fail(f"{WRITER} is missing")


def require_clean() -> None:
    for path in dirty_paths(git("status", "--porcelain", "-z")):
        if not is_engine_path(path):
            fail(
                "the working tree has uncommitted changes outside `.scratch/`: "
                f"{path}; commit or stash them before the bump"
            )


def read_version() -> str:
    try:
        data = tomllib.loads(VERSION_FILE.read_text(encoding="utf-8"))
    except OSError as error:
        fail(f"cannot read {VERSION_FILE}: {error}")
    except tomllib.TOMLDecodeError as error:
        fail(f"{VERSION_FILE} does not parse: {error}")
    version = str(data.get("version", "")).strip()
    if not version:
        fail(f"{VERSION_FILE} has no version")
    return version


def main(argv: list[str]) -> int:
    if argv:
        fail("bump.py takes no arguments")

    require_root()
    require_main()
    require_writer()
    require_clean()

    old = read_version()
    run(["python3", "scripts/version.py", "bump", "patch"])
    run(["python3", "scripts/version.py", "check"])
    new = read_version()

    body = (
        "The run bumps PATCH once, before the merge queue lands a task, so the\n"
        "merged result carries the version it ships. The writer staged\n"
        "version.toml, the Cargo manifests and Cargo.lock; --no-verify is\n"
        "deliberate because .githooks/pre-commit would bump PATCH a second time."
    )
    run(
        [
            "git",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--no-verify",
            "-m",
            f"build: bump to {new}",
            "-m",
            body,
        ]
    )

    short = git("rev-parse", "--short", "HEAD").strip()
    print(f"bump: {old} -> {new} ({short})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
