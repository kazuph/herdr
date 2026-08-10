from __future__ import annotations

import copy
import json
import unittest

from scripts.upstream_packet_gate import RESULTS_PATH, validate_results


class UpstreamPacketGateTests(unittest.TestCase):
    def setUp(self) -> None:
        self.results = json.loads(RESULTS_PATH.read_text(encoding="utf-8"))

    def test_current_results_are_green(self) -> None:
        self.assertEqual(validate_results(self.results), [])

    def test_stale_model_hash_is_red(self) -> None:
        mutated = copy.deepcopy(self.results)
        mutated["quint"]["model_sha256"] = "0" * 64
        self.assertIn("stale quint verification hash", validate_results(mutated))

    def test_missing_non_vacuity_is_red(self) -> None:
        mutated = copy.deepcopy(self.results)
        mutated["quint"]["non_vacuity_red"] = []
        self.assertTrue(any("non-vacuity" in error for error in validate_results(mutated)))

    def test_quint_command_without_recorded_seed_is_red(self) -> None:
        mutated = copy.deepcopy(self.results)
        mutated["quint"]["green_runs"][0]["command"] = mutated["quint"]["green_runs"][0]["command"].split(" --seed=")[0]
        self.assertTrue(any("not exactly reproducible" in error for error in validate_results(mutated)))

    def test_placeholder_command_is_red(self) -> None:
        mutated = copy.deepcopy(self.results)
        mutated["alloy"]["command"] = "alloy exec <temporary-directory>"
        self.assertIn("alloy command is missing or contains a placeholder", validate_results(mutated))

    def test_missing_drift_red_green_is_red(self) -> None:
        mutated = copy.deepcopy(self.results)
        mutated["drift_and_contract_evidence"]["drift_red_green"]["result"] = "green only"
        self.assertIn("formal drift RED-to-GREEN evidence is incomplete", validate_results(mutated))

    def test_missing_diff_check_is_red(self) -> None:
        mutated = copy.deepcopy(self.results)
        mutated["drift_and_contract_evidence"]["diff_check"]["result"] = "unverified"
        self.assertIn("git diff check is not recorded GREEN", validate_results(mutated))

    def test_stale_evidence_bundle_is_red(self) -> None:
        mutated = copy.deepcopy(self.results)
        mutated["execution_binding"]["evidence_bundle_sha256"] = "0" * 64
        self.assertIn("formal execution evidence bundle hash is stale", validate_results(mutated))

    def test_missing_production_quint_property_is_red(self) -> None:
        mutated = copy.deepcopy(self.results); mutated["production_quint_runs"].pop()
        self.assertIn("production mailbox Quint property runs are incomplete", validate_results(mutated))

    def test_formal_gate_suite_must_match_bundle(self) -> None:
        mutated = copy.deepcopy(self.results); mutated["drift_and_contract_evidence"]["formal_gate_suite"]["evidence_bundle_sha256"] = "0" * 64
        self.assertIn("formal inventory, drift, and packet gate suite is not bound GREEN", validate_results(mutated))


if __name__ == "__main__":
    unittest.main()
