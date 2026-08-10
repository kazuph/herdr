#!/usr/bin/env python3
"""Fail-closed precondition for starting an upstream intake packet."""
from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import sys
from pathlib import Path
from typing import Any

try:
    from scripts.fork_difference_inventory import inventory as difference_inventory
    from scripts.spec_formal_drift_check import load_manifest, validate_manifest
except ModuleNotFoundError:
    from fork_difference_inventory import inventory as difference_inventory
    from spec_formal_drift_check import load_manifest, validate_manifest

ROOT = Path(__file__).resolve().parents[1]
RESULTS_PATH = ROOT / "formal" / "formal-verification-results.json"
MODEL_PATHS = {
    "lean": ROOT / "formal" / "ForkDecisionSpec.lean",
    "alloy": ROOT / "formal" / "fork-ownership.als",
    "quint": ROOT / "formal" / "fork-contract-lifecycle.qnt",
}
REQUIRED_EVIDENCE_PATHS = (
    "SPEC.md",
    "docs/fork-strategy.md",
    "referent-table-codex-restore-permission-parity.md",
    "justfile",
    "formal/ForkDecisionSpec.lean",
    "formal/fork-contract-lifecycle.qnt",
    "formal/fork-difference-overrides.json",
    "formal/fork-ownership.als",
    "formal/spec-evidence-manifest.json",
    "scripts/fork_difference_inventory.py",
    "scripts/spec_formal_drift_check.py",
    "scripts/test_fork_difference_inventory.py",
    "scripts/test_spec_contract_inventory.py",
    "scripts/test_spec_formal_drift_check.py",
    "scripts/test_upstream_packet_gate.py",
    "scripts/upstream_packet_gate.py",
)
EXPECTED_QUINT_GREEN = {
    "deliveredRequiresWrite",
    "cancelledCannotRetry",
    "retryOnlyForQueued",
    "ledgerHasLiveOwner",
    "ledgerMatchesGeneration",
    "removedPaneHasNoLedger",
}
EXPECTED_QUINT_WITNESSES = {
    "deliveredIsReachable",
    "retainedRestorablePaneIsReachable",
    "cancelledIsReachable",
    "failedWriteQueueIsReachable",
    "permanentDeletionIsReachable",
    "reusedGenerationLedgerIsReachable",
}
EXPECTED_PRODUCTION_QUINT = {
    "formal/p428_mailbox_immediate_steering.qnt": {
        "availableQueueHasDeliveryEvent", "workingSteeringIsImmediate",
        "unavailableNeverAccepts", "noDuplicateAcceptance",
        "presentationDoesNotDeliver", "unrelatedApiDoesNotDeliver",
    },
    "formal/p428_mailbox_write_ack.qnt": {
        "deliveredRequiresPtyWrite", "failedWriteRemainsQueued", "appNeverBlocks",
        "completionMatchesInFlight", "markedAtMostOnce",
        "mailboxNeverReservesSharedEventCapacity",
    },
}


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def evidence_bundle_sha256() -> str:
    digest = hashlib.sha256()
    for relative in REQUIRED_EVIDENCE_PATHS:
        digest.update(relative.encode())
        digest.update(b"\0")
        digest.update((ROOT / relative).read_bytes())
        digest.update(b"\0")
    return digest.hexdigest()


def run_git(*args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", *args], cwd=ROOT, text=True, capture_output=True, check=False
    )


def validate_results(results: dict[str, Any]) -> list[str]:
    errors: list[str] = []
    binding = results.get("execution_binding", {})
    if binding.get("code_commit") != "88118ae5a17e915883b2fd69562ad24b2a56e905":
        errors.append("formal execution code commit is not the immutable runtime baseline")
    if binding.get("evidence_bundle_sha256") != evidence_bundle_sha256():
        errors.append("formal execution evidence bundle hash is stale")
    for model, path in MODEL_PATHS.items():
        recorded = results.get(model, {}).get("model_sha256")
        if recorded != sha256(path):
            errors.append(f"stale {model} verification hash")
    alloy = results.get("alloy", {})
    if len(alloy.get("unsat_assertions", [])) < 4 or alloy.get("sat_witnesses") != [
        "OwnershipWitness"
    ]:
        errors.append("Alloy UNSAT assertions or SAT witness are incomplete")
    quint = results.get("quint", {})
    green_runs = quint.get("green_runs", [])
    red_runs = quint.get("non_vacuity_red", [])
    if {run.get("invariant") for run in green_runs} != EXPECTED_QUINT_GREEN or {
        run.get("invariant") for run in red_runs
    } != EXPECTED_QUINT_WITNESSES:
        errors.append("Quint GREEN runs or non-vacuity RED witnesses are incomplete")
    for run in [*green_runs, *red_runs]:
        command = run.get("command", "")
        seed = run.get("seed", "")
        invariant = run.get("invariant", "")
        if (
            not command
            or not seed
            or f"--invariant={invariant}" not in command
            or f"--seed={seed}" not in command
            or "<" in command
            or ">" in command
        ):
            errors.append(f"Quint run is not exactly reproducible: {invariant}")
    for model in ("lean", "alloy"):
        command = results.get(model, {}).get("command", "")
        if not command or "<" in command or ">" in command:
            errors.append(f"{model} command is missing or contains a placeholder")
    production_runs = results.get("production_quint_runs", [])
    actual_production = {
        model: {run.get("invariant") for run in production_runs if run.get("model") == model}
        for model in EXPECTED_PRODUCTION_QUINT
    }
    if actual_production != EXPECTED_PRODUCTION_QUINT:
        errors.append("production mailbox Quint property runs are incomplete")
    for run in production_runs:
        command, seed, invariant = run.get("command", ""), run.get("seed", ""), run.get("invariant", "")
        if f"--invariant={invariant}" not in command or f"--seed={seed}" not in command or run.get("result") != "green":
            errors.append(f"production mailbox Quint run is not reproducible GREEN: {invariant}")
    separation = results.get("mailbox_model_separation", {})
    current_models = separation.get("production_current", [])
    repaired_models = separation.get("historical_repaired_candidate", [])
    for record in [*current_models, *[item for item in repaired_models if isinstance(item, dict)]]:
        path = ROOT / record.get("path", "")
        if not path.is_file() or record.get("sha256") != sha256(path):
            errors.append(f"stale mailbox model verification hash: {record.get('path', '')}")
    evidence = results.get("drift_and_contract_evidence", {})
    if evidence.get("contract_suite", {}).get("exit_code") != 0:
        errors.append("fork contract suite is not recorded GREEN")
    inventory = evidence.get("fork_difference_inventory", {})
    if (
        inventory.get("errors") != 0
        or inventory.get("non_merge") != 178
        or inventory.get("spec_referenced") + inventory.get("overrides") != 178
    ):
        errors.append("fork difference inventory is not recorded zero-error")
    if evidence.get("drift_red_green", {}).get("result") != "all current-tree and intentional mutation checks passed":
        errors.append("formal drift RED-to-GREEN evidence is incomplete")
    if evidence.get("diff_check", {}).get("result") != "green":
        errors.append("git diff check is not recorded GREEN")
    formal_gate = evidence.get("formal_gate_suite", {})
    if formal_gate.get("result") != "green" or formal_gate.get("tests") != 31 or formal_gate.get("evidence_bundle_sha256") != evidence_bundle_sha256():
        errors.append("formal inventory, drift, and packet gate suite is not bound GREEN")
    return errors


def validate_current_tree(require_pushed: bool) -> list[str]:
    results = json.loads(RESULTS_PATH.read_text(encoding="utf-8"))
    errors = validate_results(results)
    errors.extend(validate_manifest(load_manifest()))
    diff = difference_inventory()
    if diff["counts"]["errors"] != 0:
        errors.extend(diff["errors"])
    if require_pushed:
        status = run_git("status", "--porcelain")
        if status.returncode != 0 or status.stdout.strip():
            errors.append("packet parent worktree is not clean")
        local = run_git("rev-parse", "HEAD")
        upstream = run_git("rev-parse", "@{upstream}")
        if local.returncode != 0 or upstream.returncode != 0:
            errors.append("local HEAD or upstream evidence HEAD cannot be resolved")
        elif local.stdout.strip() != upstream.stdout.strip():
            errors.append("local HEAD does not equal pushed evidence HEAD")
        remote = run_git("config", "--get", "branch." + run_git("branch", "--show-current").stdout.strip() + ".remote")
        if remote.returncode != 0 or remote.stdout.strip() != "origin":
            errors.append("tracking remote is not origin")
        tracked = run_git("ls-files", "--error-unmatch", *REQUIRED_EVIDENCE_PATHS, "formal/formal-verification-results.json")
        if tracked.returncode != 0:
            errors.append("required evidence files are not committed")
        code_commit = results.get("execution_binding", {}).get("code_commit", "")
        source_diff = run_git("diff", "--quiet", code_commit, "HEAD", "--", "src", "tests")
        if source_diff.returncode != 0:
            errors.append("runtime source or Rust tests changed after recorded execution commit")
    return errors


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--require-pushed", action="store_true")
    errors = validate_current_tree(parser.parse_args().require_pushed)
    if errors:
        print("upstream packet gate failed:", file=sys.stderr)
        print("\n".join(errors), file=sys.stderr)
        return 1
    print("upstream packet gate: GREEN")
    return 0


if __name__ == "__main__":
    sys.exit(main())
