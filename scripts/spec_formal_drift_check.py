#!/usr/bin/env python3
"""Read-only completeness checks for the formal evidence manifest."""
from __future__ import annotations
import argparse, functools, json, sys
from pathlib import Path
from typing import Any
try:
    from scripts.spec_contract_inventory import inventory
except ModuleNotFoundError:
    from spec_contract_inventory import inventory

ROOT = Path(__file__).resolve().parents[1]
MANIFEST_PATH = ROOT / "formal" / "spec-evidence-manifest.json"
SPEC_PATH = ROOT / "SPEC.md"
REQUIRED_FIELDS = ("dimensions", "verification_methods", "source_anchors", "test_status", "live_status", "unrepresented_dimensions")
METHOD_PREFIXES = ("rust_test_target:", "lean_theorem:", "alloy_assert_witness:", "quint_invariant:", "platform_live_acceptance:")
MODEL_FILES = {
    "lean_theorem:": ROOT / "formal" / "ForkDecisionSpec.lean",
    "alloy_assert_witness:": ROOT / "formal" / "fork-ownership.als",
}
QNT_MODEL_PATHS = {
    "formal/p428_mailbox_immediate_steering.qnt",
    "formal/p428_mailbox_write_ack.qnt",
    "formal/fork-contract-lifecycle.qnt",
}
HISTORICAL_DELIVERY_MODEL = "formal/p428_mailbox_delivery.qnt"
HISTORICAL_REPAIRED_MODEL = "formal/p428_mailbox_repaired.qnt"
BROAD_DOMAIN_TEST_TARGETS = {
    "workspace_context_menu",
    "cancelled_job_is_durable",
    "pane_context_menu_matches_legacy_fork_order_and_separators",
    "api_pane_current_resolves_one",
    "session_ids_follow_the_fork_fail_closed_contract",
    "self_update_is_disabled_in_the_fork",
    "pane_help_distinguishes_literal_text_from_submitted_commands",
    "notification_show_api_creates_herdr_toast_with_position",
    "worktree_context_menu",
}

def load_manifest(path: Path = MANIFEST_PATH) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))

@functools.lru_cache(maxsize=None)
def source_anchor_exists(anchor: str) -> bool:
    try:
        path, line = anchor.rsplit(":", 1)
        target = ROOT / path
        line_number = int(line)
        return target.is_file() and 0 < line_number <= len(target.read_text(encoding="utf-8").splitlines())
    except (ValueError, TypeError):
        return False

@functools.lru_cache(maxsize=None)
def rust_test_target_exists(method: str) -> bool:
    target = method.removeprefix("rust_test_target:")
    if not target:
        return False
    roots = [ROOT / "src", ROOT / "tests"]
    return any(
        f"fn {target}" in line or ("fn " in line and target in line)
        for source_root in roots if source_root.is_dir()
        for path in source_root.rglob("*.rs")
        for line in path.read_text(encoding="utf-8").splitlines()
    )

def model_property_exists(method: str) -> bool:
    if method.startswith("quint_invariant:"):
        model_and_property = method.removeprefix("quint_invariant:")
        model_path, separator, property_name = model_and_property.rpartition(":")
        if not separator or model_path not in QNT_MODEL_PATHS or not property_name:
            return False
        model = ROOT / model_path
        return model.is_file() and f"val {property_name}" in model.read_text(encoding="utf-8")
    prefix = next((prefix for prefix in MODEL_FILES if method.startswith(prefix)), None)
    if prefix is None: return True
    property_name = method.removeprefix(prefix)
    text = MODEL_FILES[prefix].read_text(encoding="utf-8")
    if prefix == "lean_theorem:": return f"theorem {property_name}" in text
    assertion, separator, witness = property_name.partition("/")
    return bool(separator) and f"assert {assertion}" in text and f"pred {witness}" in text

def validate_manifest(manifest: dict[str, Any]) -> list[str]:
    errors: list[str] = []
    rows = manifest.get("contracts")
    if not isinstance(rows, list): return ["manifest contracts must be an array"]
    active_contracts = {c["id"]: c for c in inventory(SPEC_PATH)["contracts"] if c["status"] == "unverified"}
    active = set(active_contracts)
    seen: set[str] = set()
    for index, row in enumerate(rows):
        if not isinstance(row, dict) or not isinstance(row.get("contract_id"), str):
            errors.append(f"contracts[{index}] has no contract_id"); continue
        contract_id = row["contract_id"]
        if contract_id in seen: errors.append(f"duplicate manifest contract_id: {contract_id}")
        seen.add(contract_id)
        if contract_id not in active: errors.append(f"unknown or inactive manifest contract_id: {contract_id}")
        expected_spec_anchor = f"SPEC.md:{active_contracts[contract_id]['line']}" if contract_id in active_contracts else None
        if expected_spec_anchor and row.get("source_anchors", [None])[0] != expected_spec_anchor:
            errors.append(f"{contract_id} has a stale SPEC line anchor: expected {expected_spec_anchor}")
        for field in REQUIRED_FIELDS:
            value = row.get(field)
            if not isinstance(value, list) or not value or not all(isinstance(item, str) and item.strip() for item in value): errors.append(f"{contract_id} has an empty or invalid {field}")
        methods = row.get("verification_methods", [])
        if isinstance(methods, list):
            if not any(method.startswith(METHOD_PREFIXES) for method in methods): errors.append(f"{contract_id} has no concrete verification method")
            for method in methods:
                if method.startswith(tuple(MODEL_FILES) + ("quint_invariant:",)) and not model_property_exists(method): errors.append(f"{contract_id} names an unknown model property: {method}")
                if method.startswith("rust_test_target:") and not rust_test_target_exists(method): errors.append(f"{contract_id} names an unknown Rust test target: {method}")
                if method.startswith("drift_source_anchor:") and not (ROOT / method.removeprefix("drift_source_anchor:")).is_file():
                    errors.append(f"{contract_id} names a missing implementation routing file: {method}")
                if method.removeprefix("rust_test_target:") in BROAD_DOMAIN_TEST_TARGETS:
                    errors.append(f"{contract_id} uses a broad domain test as contract evidence: {method}")
                if method.startswith(f"quint_invariant:{HISTORICAL_DELIVERY_MODEL}:"):
                    errors.append(f"{contract_id} treats the historical wait-for-Idle model as active evidence")
                if method.startswith(f"quint_invariant:{HISTORICAL_REPAIRED_MODEL}:"):
                    errors.append(f"{contract_id} treats the historical repaired model as active evidence")
        for status in ("test_status", "live_status"):
            if not all(value in {"unverified", "static_verified", "formal_unverified"} for value in row.get(status, [])): errors.append(f"{contract_id} has an invalid {status}")
        if any(not isinstance(anchor, str) or not source_anchor_exists(anchor) for anchor in row.get("source_anchors", [])): errors.append(f"{contract_id} has a dead source anchor")
        if any(anchor != "SPEC.md:1" and anchor.endswith(":1") for anchor in row.get("source_anchors", [])):
            errors.append(f"{contract_id} uses file line 1 as implementation evidence")
        if "platform_live_acceptance:unverified" in methods and "verified" in row.get("live_status", []): errors.append(f"{contract_id} marks unexecuted live acceptance verified")
    if missing := sorted(active - seen): errors.append(f"active contracts missing from manifest: {', '.join(missing)}")
    for name in ("ForkDecisionSpec.lean", "fork-ownership.als", "fork-contract-lifecycle.qnt"):
        if not (ROOT / "formal" / name).exists(): errors.append(f"missing formal model: {name}")
    metadata = manifest.get("metadata", {})
    execution = metadata.get("execution", {}) if isinstance(metadata, dict) else {}
    if any("unavailable" in str(value).lower() for value in execution.values()): errors.append("metadata must use pending_manager_verification instead of unavailable")
    results_path = ROOT / "formal" / "formal-verification-results.json"
    if not results_path.is_file():
        errors.append("missing formal verification results")
    elif any("pending_manager_verification" in str(value) for value in execution.values()):
        errors.append("formal execution metadata is still pending manager verification")
    return errors

def main() -> int:
    parser = argparse.ArgumentParser(); parser.add_argument("--manifest", type=Path, default=MANIFEST_PATH)
    errors = validate_manifest(load_manifest(parser.parse_args().manifest))
    if errors:
        print("spec formal drift check failed:", file=sys.stderr); print("\n".join(errors), file=sys.stderr); return 1
    print("spec formal evidence manifest covers every active contract"); return 0

if __name__ == "__main__": sys.exit(main())
