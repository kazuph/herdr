#!/usr/bin/env python3
"""Fail when feature or fix commits change runtime code without updating SPEC.md.

SPEC.md is the fork's behaviour contract. A `feat` or `fix` commit that touches
runtime code (`src/`, `tests/`) must land in a range that also changes
SPEC.md. A commit whose change has no observable behaviour can opt out with a
body line `Spec: none - <reason>`; the reason is required.
"""
from __future__ import annotations

import argparse
import re
import subprocess
import sys
from dataclasses import dataclass

GATED_SUBJECT_RE = re.compile(r"^(?P<kind>feat|fix)(?:\((?P<scope>[^)]+)\))?!?:\s+\S")
# Scopes that never change fork behaviour (packaging hashes, CI, tests only).
EXEMPT_SCOPES = {"nix", "ci", "test", "tests"}
RUNTIME_PREFIXES = ("src/", "tests/")
SPEC_FILE = "SPEC.md"
OPT_OUT_RE = re.compile(r"^spec:\s*none\s*[-:–—]\s*\S", re.IGNORECASE | re.MULTILINE)


@dataclass(frozen=True)
class Commit:
    sha: str
    subject: str
    body: str
    files: tuple[str, ...]


def needs_spec(commit: Commit) -> bool:
    match = GATED_SUBJECT_RE.match(commit.subject)
    if not match or (match.group("scope") or "") in EXEMPT_SCOPES:
        return False
    if not any(path.startswith(RUNTIME_PREFIXES) for path in commit.files):
        return False
    return not OPT_OUT_RE.search(commit.body)


def violations(commits: list[Commit]) -> list[Commit]:
    if any(SPEC_FILE in commit.files for commit in commits):
        return []
    return [commit for commit in commits if needs_spec(commit)]


def git_commits(rev_range: str) -> list[Commit]:
    separator = "\x1e"
    log = subprocess.check_output(
        ["git", "log", "--no-merges", f"--format={separator}%H%x00%s%x00%b", rev_range],
        text=True,
    )
    commits = []
    for record in log.split(separator):
        if not record.strip():
            continue
        sha, subject, body = record.strip("\n").split("\x00", 2)
        files = subprocess.check_output(
            ["git", "diff-tree", "--no-commit-id", "--name-only", "-r", sha], text=True
        ).split()
        commits.append(Commit(sha=sha, subject=subject, body=body, files=tuple(files)))
    return commits


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--range", required=True, help="git revision range, e.g. origin/main..HEAD")
    args = parser.parse_args()
    missing = violations(git_commits(args.range))
    if not missing:
        print("spec update gate: every feature/fix commit has a SPEC.md update or an explicit opt-out")
        return 0
    print("spec update gate failed: these commits change runtime code without a SPEC.md update:", file=sys.stderr)
    for commit in missing:
        print(f"  {commit.sha[:10]} {commit.subject}", file=sys.stderr)
    print(
        "Add or update the matching SPEC.md contract (and its formal/spec-evidence-manifest.json row) in this PR,\n"
        "or, only when the change has no observable behaviour, add a commit body line `Spec: none - <reason>`.",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    sys.exit(main())
