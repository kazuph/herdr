#!/usr/bin/env python3
"""Check freshness of the P428 G9 mailbox contract-to-evidence inventory.

This checker only proves that the content-stable SPEC IDs still point at the
named Quint properties and Rust regression tests.  p428 reviews semantic
equivalence; it is deliberately not inferred from string anchors here.
"""

from pathlib import Path
import re
import sys

try:
    from scripts.spec_contract_inventory import inventory
except ModuleNotFoundError:
    from spec_contract_inventory import inventory


ROOT = Path(__file__).resolve().parents[1]
MAILBOX_SECTION = "herdr msg durable mailbox"
CONTRACT_EVIDENCE = {
    "G9-f86e9d2db8": {
        "quint": (("formal/p428_mailbox_immediate_steering.qnt", "availableQueueHasDeliveryEvent"),),
        "rust": (
            "reopened_app_with_same_stable_pane_id_delivers_regular_mail_once",
            "startup_flush_submits_current_working_regular_mail_once",
        ),
    },
    "G9-97e894730a": {
        "quint": (("formal/p428_mailbox_input_repaired.qnt", "noPartialPrompt"),),
        "rust": (
            "regular_message_delivery_submits_body_and_enter_atomically",
            "idle_direct_message_submits_its_exact_body_without_an_inbox_command",
        ),
    },
    "G9-6462664a8c": {
        "quint": (("formal/p428_mailbox_immediate_steering.qnt", "workingSteeringIsImmediate"),),
        "rust": (
            "working_recipient_receives_regular_message_as_immediate_steering",
            "working_send_submits_immediately_and_idle_does_not_duplicate",
        ),
    },
    "G9-cafbb89bc7": {
        "quint": (("formal/p428_mailbox_immediate_steering.qnt", "workingSteeringIsImmediate"),),
        "rust": (
            "working_recipient_receives_regular_message_as_immediate_steering",
            "idle_direct_message_submits_its_exact_body_without_an_inbox_command",
        ),
    },
    "G9-edb5ccfcdc": {
        "quint": (("formal/p428_mailbox_delivery.qnt", "noAliasFallback"),),
        "rust": (
            "restarted_same_name_does_not_nudge_or_read_old_regular_mail",
            "inbox_reads_pane_id_and_agent_name_recipients_for_the_same_pane",
        ),
    },
    "G9-b23748e9e3": {
        "quint": (
            ("formal/p428_mailbox_immediate_steering.qnt", "unrelatedApiDoesNotDeliver"),
        ),
        "rust": ("agent_and_pane_list_do_not_flush_queued_regular_messages",),
    },
    "G9-a3caf10402": {
        "quint": (
            ("formal/p428_mailbox_immediate_steering.qnt", "availableQueueHasDeliveryEvent"),
            ("formal/p428_mailbox_immediate_steering.qnt", "presentationDoesNotDeliver"),
        ),
        "rust": (
            "startup_flush_submits_current_working_regular_mail_once",
            "idle_direct_message_submits_its_exact_body_without_an_inbox_command",
            "idle_transition_submits_regular_messages_in_global_creation_order",
            "unknown_then_idle_submits_queued_regular_message",
            "blocked_message_submits_when_recipient_returns_to_working",
            "repeated_working_report_retries_regular_message_after_runtime_is_ready",
            "done_recipient_receives_regular_message_immediately",
            "blocked_message_submits_when_recipient_transitions_to_done",
            "presentation_only_working_update_does_not_flush_regular_messages",
        ),
    },
}


def main() -> int:
    contracts = inventory(ROOT / "SPEC.md")["contracts"]
    live_ids = {
        contract["id"]
        for contract in contracts
        if contract["domain"] == "G9"
        and contract["section"] == MAILBOX_SECTION
        and contract["status"] != "retired"
    }
    missing = []
    if live_ids != set(CONTRACT_EVIDENCE):
        missing.append(
            "SPEC G9 mailbox content IDs differ from CONTRACT_EVIDENCE: "
            f"live={sorted(live_ids)}, mapped={sorted(CONTRACT_EVIDENCE)}"
        )

    for content_id, evidence in CONTRACT_EVIDENCE.items():
        for relative, property_name in evidence["quint"]:
            text = (ROOT / relative).read_text(encoding="utf-8")
            if not re.search(rf"^\s*val\s+{re.escape(property_name)}\b", text, re.MULTILINE):
                missing.append(f"{content_id}: {relative} missing {property_name!r}")
        rust = (ROOT / "src/app/msg.rs").read_text(encoding="utf-8")
        for test_name in evidence["rust"]:
            pattern = rf"#\[test\]\s*fn\s+{re.escape(test_name)}\s*\("
            if not re.search(pattern, rust):
                missing.append(f"{content_id}: src/app/msg.rs missing Rust test {test_name!r}")

    if missing:
        print("P428 mailbox contract inventory is stale:", file=sys.stderr)
        print("\n".join(missing), file=sys.stderr)
        return 1
    print("P428 mailbox G9 contract inventory is fresh (semantic review remains human)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
