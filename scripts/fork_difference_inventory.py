#!/usr/bin/env python3
"""Read-only inventory of fork-only commits against frozen formalization baselines."""

from __future__ import annotations

import json
import re
import subprocess
import sys
from fnmatch import fnmatchcase
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
OVERRIDES_PATH = ROOT / "formal" / "fork-difference-overrides.json"
SPEC_PATH = ROOT / "SPEC.md"
MERGE_BASE = "9c9490d764d306b6cc093b5b3de1ccd4e6467c94"
FORK_BASELINE = "88118ae5a17e915883b2fd69562ad24b2a56e905"
CATEGORIES = {"atomic_contract", "existing_contract_evidence", "upstream_equivalent", "non_product", "retired", "migration_carrier"}


def git(*args: str) -> str:
    return subprocess.check_output(["git", *args], cwd=ROOT, text=True).strip()


def commits_in_range() -> list[dict[str, str]]:
    rows = git("log", "--reverse", "--no-merges", "--format=%H%x09%s", f"{MERGE_BASE}..{FORK_BASELINE}")
    return [{"commit": sha, "subject": subject} for row in rows.splitlines() for sha, subject in [row.split("\t", 1)]]


def spec_references(commits: list[dict[str, str]]) -> dict[str, list[dict[str, str]]]:
    references = {entry["commit"]: [] for entry in commits}
    domain = section = ""
    for line in SPEC_PATH.read_text(encoding="utf-8").splitlines():
        if match := re.match(r"^## (G\d+)\.", line):
            domain, section = match.group(1), ""
        if match := re.match(r"^###\s+(.+)$", line):
            section = match.group(1)
        for commit in references:
            if re.search(rf"(?<![0-9a-f])(?:{commit}|{commit[:7]})(?![0-9a-f])", line):
                location = {"domain": domain, "section": section}
                if location not in references[commit]:
                    references[commit].append(location)
    return {commit: locations for commit, locations in references.items() if locations}


def load_overrides() -> dict[str, Any]:
    return json.loads(OVERRIDES_PATH.read_text(encoding="utf-8"))


def carrier_changed_files(commit: str) -> list[str]:
    return [path for path in git("show", "--format=", "--name-only", commit).splitlines() if path]


def validate_carrier_slices(override: dict[str, Any], changed_files: list[str]) -> list[str]:
    errors: list[str] = []
    commit = override["commit"]
    slices = override.get("carrier_slices")
    if not isinstance(slices, list) or not slices:
        return [f"{commit} migration_carrier must define carrier_slices"]

    patterns: list[tuple[str, str]] = []
    for index, slice_ in enumerate(slices):
        label = f"{commit} carrier_slices[{index}]"
        if not isinstance(slice_, dict):
            errors.append(f"{label} must be an object")
            continue
        for field in ("name", "path_patterns", "linked_sections", "disposition", "verification", "unrepresented_dimensions"):
            value = slice_.get(field)
            valid = (isinstance(value, str) and bool(value.strip())) or (isinstance(value, list) and bool(value) and all(isinstance(item, str) and item.strip() for item in value))
            if not valid:
                errors.append(f"{label} has an empty or invalid {field}")
        if slice_.get("disposition") not in {"source_accept", "semantic_port", "reject_hold", "evidence_only"}:
            errors.append(f"{label} has an invalid disposition")
        if isinstance(slice_.get("path_patterns"), list):
            patterns.extend((pattern, label) for pattern in slice_["path_patterns"] if isinstance(pattern, str) and pattern.strip())

    for pattern, label in patterns:
        if not any(fnmatchcase(path, pattern) for path in changed_files):
            errors.append(f"{label} pattern matches no changed file: {pattern}")
    for path in changed_files:
        matches = [(pattern, label) for pattern, label in patterns if fnmatchcase(path, pattern)]
        if not matches:
            errors.append(f"{commit} uncovered carrier file: {path}")
        elif len(matches) > 1:
            errors.append(f"{commit} carrier file matched multiple primary patterns: {path} -> {matches}")
    return errors


def validate(document: dict[str, Any], commits: list[dict[str, str]], references: dict[str, list[dict[str, str]]]) -> list[str]:
    errors: list[str] = []
    metadata, overrides = document.get("metadata"), document.get("overrides")
    if not isinstance(metadata, dict) or metadata.get("merge_base") != MERGE_BASE or metadata.get("fork_baseline") != FORK_BASELINE:
        errors.append("baseline metadata differs from the frozen merge-base or fork baseline")
    if git("merge-base", MERGE_BASE, FORK_BASELINE) != MERGE_BASE:
        errors.append("baseline drift: merge-base is no longer the ancestor of fork baseline")
    if not isinstance(overrides, list):
        return errors + ["overrides must be an array"]
    commit_set, seen = {entry["commit"] for entry in commits}, set()
    for index, override in enumerate(overrides):
        if not isinstance(override, dict):
            errors.append(f"override[{index}] must be an object")
            continue
        commit = override.get("commit")
        if not isinstance(commit, str) or not re.fullmatch(r"[0-9a-f]{40}", commit):
            errors.append(f"override[{index}] has an invalid full commit sha")
            continue
        if commit in seen:
            errors.append(f"duplicate override for {commit}")
        seen.add(commit)
        if commit not in commit_set:
            errors.append(f"unknown override commit {commit}")
        if commit in references:
            errors.append(f"spec-referenced commit must not have an override: {commit}")
        if override.get("category") not in CATEGORIES:
            errors.append(f"{commit} has an invalid category")
        for field in ("rationale", "affected_files", "linked_sections", "verification"):
            value = override.get(field)
            valid = (isinstance(value, str) and bool(value.strip())) or (isinstance(value, list) and bool(value) and all(isinstance(item, str) and item.strip() for item in value))
            if not valid:
                errors.append(f"{commit} has an empty or invalid {field}")
        if override.get("category") == "migration_carrier" and commit in commit_set:
            errors.extend(validate_carrier_slices(override, carrier_changed_files(commit)))
    unreferenced = commit_set - set(references)
    if missing := sorted(unreferenced - seen):
        errors.append(f"unclassified commits: {', '.join(missing)}")
    if extra := sorted(seen - unreferenced):
        errors.append(f"overrides are not exactly the unreferenced commits: {', '.join(extra)}")
    if not references:
        errors.append("SPEC-referenced commit count is zero")
    return errors


def inventory() -> dict[str, Any]:
    commits = commits_in_range()
    references = spec_references(commits)
    document = load_overrides()
    override_by_commit = {entry["commit"]: entry for entry in document.get("overrides", []) if isinstance(entry, dict) and isinstance(entry.get("commit"), str)}
    errors = validate(document, commits, references)
    rows = []
    for entry in commits:
        commit = entry["commit"]
        classification = {"kind": "spec_referenced", "locations": references[commit]} if commit in references else {"kind": "override", "override": override_by_commit.get(commit)}
        rows.append({**entry, "classification": classification})
    return {"metadata": {"merge_base": MERGE_BASE, "fork_baseline": FORK_BASELINE}, "counts": {"non_merge_commits": len(commits), "spec_referenced": len(references), "overrides": len(override_by_commit), "errors": len(errors)}, "errors": errors, "commits": rows}


def main() -> int:
    result = inventory()
    print(json.dumps(result, ensure_ascii=False, indent=2))
    return 1 if result["errors"] else 0


if __name__ == "__main__":
    sys.exit(main())
