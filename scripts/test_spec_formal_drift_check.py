from __future__ import annotations
import copy, unittest
from scripts.spec_formal_drift_check import load_manifest, validate_manifest

class SpecFormalDriftCheckTests(unittest.TestCase):
    def test_current_manifest_is_green(self) -> None: self.assertEqual(validate_manifest(load_manifest()), [])
    def test_missing_active_contract_is_red(self) -> None:
        manifest = copy.deepcopy(load_manifest()); manifest["contracts"].pop()
        self.assertTrue(any("active contracts missing" in error for error in validate_manifest(manifest)))
    def test_empty_dimensions_is_red(self) -> None:
        manifest = copy.deepcopy(load_manifest()); manifest["contracts"][0]["dimensions"] = []
        self.assertTrue(any("empty or invalid dimensions" in error for error in validate_manifest(manifest)))

    def test_dead_source_anchor_is_red(self) -> None:
        manifest = copy.deepcopy(load_manifest()); manifest["contracts"][0]["source_anchors"] = ["src/missing.rs:1"]
        self.assertTrue(any("dead source anchor" in error for error in validate_manifest(manifest)))

    def test_unknown_model_property_is_red(self) -> None:
        manifest = copy.deepcopy(load_manifest()); manifest["contracts"][0]["verification_methods"] = ["lean_theorem:not_a_real_theorem"]
        self.assertTrue(any("unknown model property" in error for error in validate_manifest(manifest)))

    def test_stale_spec_line_anchor_is_red(self) -> None:
        manifest = copy.deepcopy(load_manifest()); manifest["contracts"][0]["source_anchors"][0] = "SPEC.md:1"
        self.assertTrue(any("stale SPEC line anchor" in error for error in validate_manifest(manifest)))

    def test_unknown_precise_quint_property_is_red(self) -> None:
        manifest = copy.deepcopy(load_manifest())
        row = next(row for row in manifest["contracts"] if row["contract_id"] == "G9-32cefaa757")
        row["verification_methods"][0] = "quint_invariant:formal/p428_mailbox_write_ack.qnt:notAProperty"
        self.assertTrue(any("unknown model property" in error for error in validate_manifest(manifest)))

    def test_historical_delivery_model_is_red_for_active_g9(self) -> None:
        manifest = copy.deepcopy(load_manifest())
        row = next(row for row in manifest["contracts"] if row["contract_id"] == "G9-edb5ccfcdc")
        row["verification_methods"].insert(0, "quint_invariant:formal/p428_mailbox_delivery.qnt:noAliasFallback")
        self.assertTrue(any("historical wait-for-Idle" in error for error in validate_manifest(manifest)))

    def test_historical_repaired_model_is_red_for_active_g9(self) -> None:
        manifest = copy.deepcopy(load_manifest())
        row = next(row for row in manifest["contracts"] if row["contract_id"] == "G9-f86e9d2db8")
        row["verification_methods"].append("quint_invariant:formal/p428_mailbox_repaired.qnt:creationOrder")
        self.assertTrue(any("historical repaired model" in error for error in validate_manifest(manifest)))

    def test_unknown_rust_test_target_is_red(self) -> None:
        manifest = copy.deepcopy(load_manifest()); manifest["contracts"][0]["verification_methods"] = ["rust_test_target:not_a_real_test"]
        self.assertTrue(any("unknown Rust test target" in error for error in validate_manifest(manifest)))

    def test_generic_only_row_is_red(self) -> None:
        manifest = copy.deepcopy(load_manifest()); manifest["contracts"][0]["verification_methods"] = ["SPEC acceptance-condition review"]
        self.assertTrue(any("no concrete verification method" in error for error in validate_manifest(manifest)))

    def test_file_line_one_is_not_implementation_evidence(self) -> None:
        manifest = copy.deepcopy(load_manifest()); manifest["contracts"][0]["source_anchors"].append("src/app/actions.rs:1")
        self.assertTrue(any("file line 1" in error for error in validate_manifest(manifest)))

    def test_broad_domain_test_is_not_contract_evidence(self) -> None:
        manifest = copy.deepcopy(load_manifest()); manifest["contracts"][0]["verification_methods"].append("rust_test_target:workspace_context_menu")
        self.assertTrue(any("broad domain test" in error for error in validate_manifest(manifest)))

if __name__ == "__main__": unittest.main()
