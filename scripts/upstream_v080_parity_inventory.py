#!/usr/bin/env python3
"""Validate the exact upstream v0.8.0 non-merge commit inventory."""

from __future__ import annotations

import argparse
from collections import Counter, defaultdict
from dataclasses import dataclass
import json
from pathlib import Path
import subprocess
import sys
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_INVENTORY_PATH = ROOT / "formal/upstream-v080-parity-inventory.json"
PACKETS_PATH = ROOT / "formal/upstream-packets"
MERGE_BASE = "9c9490d764d306b6cc093b5b3de1ccd4e6467c94"
RELEASE = "346411fa21afd297f5ed3b3fa56f9e3fbf7654b7"
EXPECTED_NON_MERGE_COUNT = 175
VALID_DISPOSITIONS = {
    "unassessed",
    "already_equivalent",
    "source_accept",
    "semantic_port",
    "policy_reject",
    "not_applicable",
}
KNOWN_CARRIER_COMMITS = {
    "e0758c32",
    "4a3302d1",
    "3f809476",
    "d30ab1b5",
    "1d238bc9",
    "8afd52ae",
    "0e434881",
    "22bb476d",
}
REQUIRED_ROW_FIELDS = {
    "commit",
    "subject",
    "changed_files",
    "feature_group",
    "domain",
    "preliminary_disposition",
    "evidence",
    "missing_work",
    "dependencies",
    "platform_gap",
    "behavior_slices",
    "carrier_review_required",
}
REQUIRED_SLICE_FIELDS = {"files", "feature_group", "domain", "status", "primary"}


@dataclass(frozen=True)
class CommitMetadata:
    commit: str
    subject: str
    changed_files: tuple[str, ...]


def git(*args: str) -> str:
    result = subprocess.run(
        ["git", *args],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    )
    return result.stdout


def current_metadata() -> list[CommitMetadata]:
    commits = git("rev-list", "--no-merges", "--reverse", f"{MERGE_BASE}..{RELEASE}").splitlines()
    return [
        CommitMetadata(
            commit,
            git("show", "-s", "--format=%s", commit).rstrip("\n"),
            tuple(git("diff-tree", "--no-commit-id", "--name-only", "-r", commit).splitlines()),
        )
        for commit in commits
    ]


def load_json(path: Path) -> dict[str, Any]:
    with path.open(encoding="utf-8") as file:
        value = json.load(file)
    if not isinstance(value, dict):
        raise ValueError(f"{path} must contain a JSON object")
    return value


def accepted_packets() -> list[dict[str, str]]:
    packets: list[dict[str, str]] = []
    for path in sorted(PACKETS_PATH.glob("*.json")):
        packet = load_json(path)
        packet_id = packet.get("id")
        source = packet.get("source")
        commit = source.get("commit") if isinstance(source, dict) else None
        if not isinstance(packet_id, str) or not isinstance(commit, str):
            raise ValueError(f"{path} must contain id and source.commit strings")
        packets.append({"id": packet_id, "source_commit": commit, "manifest": str(path.relative_to(ROOT))})
    return packets


def classify_file(path: str) -> tuple[str, str]:
    if path.startswith(("src/cli", "src/main.rs", "src/config")):
        return "CLI and configuration", "CLI, help, argument parsing, config, and error behavior"
    if path.startswith(("src/app", "src/layout", "src/ui", "src/input", "src/workspace", "src/pane")):
        return "Pane workspace UI", "Pane/workspace/tab lifecycle, layout, focus, copy, mouse, and input"
    if path.startswith(("src/detect", "src/integration", "src/agent")):
        return "Agent automation", "Agent automation, detection, integrations, and startup behavior"
    if path.startswith(("src/persist", "src/session", "src/handoff", "src/remote")):
        return "Persistence and remote", "Session persistence, restore, handoff, headless, and remote attach"
    if path.startswith(("src/api", "src/server", "src/protocol", "src/client", "src/ipc", "src/dispatch")):
        return "Runtime protocol", "Server/client protocol, API, events, identity, sockets, and runtime ownership"
    if path.startswith(("src/msg", "src/job", "src/sound", "src/popup", "src/terminal_notify")):
        return "Mailbox and jobs", "Mailbox, jobs, notifications, sound, popup, and cancellation"
    if path.startswith(("src/platform", "src/pty", "src/terminal")):
        return "Platform runtime", "Windows, macOS, Linux, ConPTY, Git Bash, SSH, IME, and process ownership"
    return "Build and distribution", "Build, dependencies, vendor, package contents, docs, installer, updater, release channels, npm, Homebrew, Nix, and website behavior"


def behavior_slices(files: tuple[str, ...], disposition: str) -> list[dict[str, Any]]:
    grouped: dict[tuple[str, str], list[str]] = defaultdict(list)
    for path in files:
        grouped[classify_file(path)].append(path)
    return [
        {
            "files": sorted(paths),
            "feature_group": feature_group,
            "domain": domain,
            "status": disposition,
            "primary": True,
        }
        for (feature_group, domain), paths in sorted(grouped.items())
    ]


def carrier_review_required(item: CommitMetadata, slices: list[dict[str, Any]], disposition: str) -> bool:
    return disposition == "unassessed" and (
        item.commit.startswith(tuple(KNOWN_CARRIER_COMMITS))
        or (len(item.changed_files) > 1 and len(slices) == 1)
    )


def template_inventory(metadata: list[CommitMetadata], packets: list[dict[str, str]]) -> dict[str, Any]:
    packet_by_commit = {packet["source_commit"]: packet for packet in packets}
    rows = []
    for item in metadata:
        packet = packet_by_commit.get(item.commit)
        disposition = "source_accept" if packet else "unassessed"
        slices = behavior_slices(item.changed_files, disposition)
        feature_groups = sorted({slice_["feature_group"] for slice_ in slices})
        domains = sorted({slice_["domain"] for slice_ in slices})
        row: dict[str, Any] = {
            "commit": item.commit,
            "subject": item.subject,
            "changed_files": list(item.changed_files),
            "feature_group": " + ".join(feature_groups),
            "domain": domains[0] if len(domains) == 1 else "multiple upstream feature domains",
            "preliminary_disposition": disposition,
            "evidence": (
                f"accepted packet {packet['id']} ({packet['manifest']})"
                if packet
                else "upstream commit metadata only; fork behavior is unassessed"
            ),
            "missing_work": "none recorded by this inventory" if packet else "assess fork behavior, contracts, evidence, and packet dependencies",
            "dependencies": [],
            "platform_gap": ["unassessed"],
            "behavior_slices": slices,
            "carrier_review_required": carrier_review_required(item, slices, disposition),
        }
        if packet:
            row["accepted_packet"] = packet["id"]
        rows.append(row)
    return {
        "schema_version": 1,
        "scope": {
            "kind": "upstream commit index",
            "parity_completion_denominator": False,
            "reason": "One upstream commit can contain multiple observable behaviors; behavior-level parity remains separately unassessed.",
        },
        "upstream": {
            "merge_base": MERGE_BASE,
            "release": RELEASE,
            "expected_non_merge_count": EXPECTED_NON_MERGE_COUNT,
        },
        "accepted_packets": packets,
        "rows": rows,
    }


def validate_inventory(data: dict[str, Any], metadata: list[CommitMetadata]) -> list[str]:
    errors: list[str] = []
    scope = data.get("scope")
    if not isinstance(scope, dict):
        errors.append("missing scope metadata")
    elif (
        scope.get("kind") != "upstream commit index"
        or scope.get("parity_completion_denominator") is not False
        or not isinstance(scope.get("reason"), str)
        or not scope["reason"]
    ):
        errors.append("inventory scope must state that commit rows are not the parity completion denominator")
    upstream = data.get("upstream")
    if not isinstance(upstream, dict):
        return ["missing upstream metadata"]
    if upstream.get("merge_base") != MERGE_BASE:
        errors.append("unexpected merge_base")
    if upstream.get("release") != RELEASE:
        errors.append("unexpected release")
    if upstream.get("expected_non_merge_count") != len(metadata):
        errors.append("stale expected_non_merge_count")

    rows = data.get("rows")
    if not isinstance(rows, list):
        return [*errors, "rows must be an array"]
    expected = {item.commit: item for item in metadata}
    row_commits = [row.get("commit") for row in rows if isinstance(row, dict)]
    for commit, count in Counter(row_commits).items():
        if isinstance(commit, str) and count > 1:
            errors.append(f"duplicate commit {commit}")
    for commit in sorted(expected.keys() - {commit for commit in row_commits if isinstance(commit, str)}):
        errors.append(f"missing commit {commit}")

    packet_entries = data.get("accepted_packets", [])
    packet_sources = {
        entry.get("id"): entry.get("source_commit")
        for entry in packet_entries
        if isinstance(entry, dict)
        and isinstance(entry.get("id"), str)
        and isinstance(entry.get("source_commit"), str)
    }
    packet_ids = set(packet_sources)
    packet_row_uses: Counter[str] = Counter()
    for row in rows:
        if not isinstance(row, dict):
            errors.append("row must be an object")
            continue
        commit = row.get("commit")
        if not isinstance(commit, str):
            errors.append("row missing commit")
            continue
        missing_fields = sorted(field for field in REQUIRED_ROW_FIELDS if field not in row)
        if missing_fields:
            errors.append(f"missing row fields for {commit}: {', '.join(missing_fields)}")
            continue
        actual = expected.get(commit)
        if actual is None:
            errors.append(f"out-of-range commit {commit}")
            continue
        if row["subject"] != actual.subject:
            errors.append(f"stale subject for {commit}")
        if row["changed_files"] != list(actual.changed_files):
            errors.append(f"stale changed_files for {commit}")
        disposition = row["preliminary_disposition"]
        if disposition not in VALID_DISPOSITIONS:
            errors.append(f"unknown preliminary_disposition for {commit}: {disposition}")
        evidence = row["evidence"]
        missing_work = row["missing_work"]
        if not isinstance(evidence, str) or not evidence:
            errors.append(f"evidence must be a non-empty string for {commit}")
        if not isinstance(missing_work, str) or not missing_work:
            errors.append(f"missing_work must be a non-empty string for {commit}")
        carrier_review = row["carrier_review_required"]
        if not isinstance(carrier_review, bool):
            errors.append(f"carrier_review_required must be boolean for {commit}")
        elif carrier_review and disposition != "unassessed":
            errors.append(f"carrier review must remain unassessed for {commit}")
        dependencies = row["dependencies"]
        if not isinstance(dependencies, list):
            errors.append(f"dependencies must be an array for {commit}")
        else:
            for dependency in dependencies:
                if dependency not in expected and dependency not in packet_ids:
                    errors.append(f"dead dependency for {commit}: {dependency}")
        accepted_packet = row.get("accepted_packet")
        if accepted_packet is not None and accepted_packet not in packet_ids:
            errors.append(f"unlinked accepted_packet for {commit}: {accepted_packet}")
        if accepted_packet in packet_ids:
            packet_row_uses.update([accepted_packet])
            if packet_sources[accepted_packet] != commit:
                errors.append(
                    f"accepted packet source commit does not match row for {commit}: {accepted_packet}"
                )
        if disposition in {"source_accept", "semantic_port"} and accepted_packet not in packet_ids:
            errors.append(f"terminal implementation disposition requires an accepted packet for {commit}")
        if accepted_packet is not None and disposition not in {"source_accept", "semantic_port"}:
            errors.append(f"accepted packet has incompatible disposition for {commit}: {disposition}")
        if disposition != "unassessed" and (
            evidence == "upstream commit metadata only; fork behavior is unassessed"
            or missing_work == "assess fork behavior, contracts, evidence, and packet dependencies"
        ):
            errors.append(f"terminal disposition still uses unassessed evidence for {commit}")
        validate_slices(row, actual, errors)
        slices = row["behavior_slices"]
        if isinstance(slices, list):
            for slice_ in slices:
                if isinstance(slice_, dict) and slice_.get("status") != disposition:
                    errors.append(f"slice status does not match row disposition for {commit}")
                    break
        must_review_carrier = actual.commit.startswith(tuple(KNOWN_CARRIER_COMMITS))
        must_review_carrier = must_review_carrier or (
            isinstance(slices, list) and len(actual.changed_files) > 1 and len(slices) == 1
        )
        if must_review_carrier and carrier_review is not True:
            errors.append(f"carrier review required for {commit}")
    for packet_id, count in sorted(packet_row_uses.items()):
        if count != 1:
            errors.append(f"accepted packet must be used by exactly one row: {packet_id}")
    return errors


def validate_slices(row: dict[str, Any], actual: CommitMetadata, errors: list[str]) -> None:
    commit = actual.commit
    slices = row["behavior_slices"]
    if not isinstance(slices, list) or not slices:
        errors.append(f"empty behavior_slices for {commit}")
        return
    primary_coverage: Counter[str] = Counter()
    for index, slice_ in enumerate(slices):
        if not isinstance(slice_, dict):
            errors.append(f"behavior slice {index} is not an object for {commit}")
            continue
        missing_fields = sorted(field for field in REQUIRED_SLICE_FIELDS if field not in slice_)
        if missing_fields:
            errors.append(f"missing behavior slice fields for {commit}: {', '.join(missing_fields)}")
            continue
        files = slice_["files"]
        if not isinstance(files, list) or not files:
            errors.append(f"empty behavior slice {index} for {commit}")
            continue
        status = slice_["status"]
        if status not in VALID_DISPOSITIONS:
            errors.append(f"unknown slice status for {commit}: {status}")
        if slice_["primary"] is True:
            primary_coverage.update(files)
        for path in files:
            if path not in actual.changed_files:
                errors.append(f"slice file not changed by {commit}: {path}")
    for path in actual.changed_files:
        if primary_coverage[path] == 0:
            errors.append(f"file not covered by a primary slice for {commit}: {path}")
        elif primary_coverage[path] > 1:
            errors.append(f"multiple primary slices for {commit}: {path}")


def validate_packet_links(data: dict[str, Any], packets: list[dict[str, str]]) -> list[str]:
    errors: list[str] = []
    inventory_packets = {
        entry.get("id"): entry.get("source_commit")
        for entry in data.get("accepted_packets", [])
        if isinstance(entry, dict)
    }
    actual_packet_ids = {packet["id"] for packet in packets}
    for packet_id in sorted(set(inventory_packets) - actual_packet_ids):
        errors.append(f"stale accepted packet entry: {packet_id}")
    rows_by_commit = {
        row.get("commit"): row for row in data.get("rows", []) if isinstance(row, dict)
    }
    for packet in packets:
        packet_id = packet["id"]
        source_commit = packet["source_commit"]
        if inventory_packets.get(packet_id) != source_commit:
            errors.append(f"accepted packet not linked: {packet_id}")
            continue
        row = rows_by_commit.get(source_commit)
        if not isinstance(row, dict) or row.get("accepted_packet") != packet_id:
            errors.append(f"accepted packet source row not linked: {packet_id}")
    return errors


def validate_current_tree(inventory_path: Path) -> list[str]:
    data = load_json(inventory_path)
    metadata = current_metadata()
    errors = validate_inventory(data, metadata)
    if len(metadata) != EXPECTED_NON_MERGE_COUNT:
        errors.append(f"expected {EXPECTED_NON_MERGE_COUNT} non-merge commits, found {len(metadata)}")
    errors.extend(validate_packet_links(data, accepted_packets()))
    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--inventory", type=Path, default=DEFAULT_INVENTORY_PATH)
    parser.add_argument("--template", action="store_true", help="print the current range as an unassessed inventory template")
    args = parser.parse_args()
    if args.template:
        print(json.dumps(template_inventory(current_metadata(), accepted_packets()), indent=2))
        return 0
    try:
        errors = validate_current_tree(args.inventory)
    except (OSError, subprocess.CalledProcessError, ValueError, json.JSONDecodeError) as error:
        print(error, file=sys.stderr)
        return 1
    if errors:
        print("upstream v0.8.0 parity inventory is invalid:", file=sys.stderr)
        print("\n".join(errors), file=sys.stderr)
        return 1
    print(f"upstream v0.8.0 parity inventory is current: {EXPECTED_NON_MERGE_COUNT} non-merge commits")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
