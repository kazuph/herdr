from __future__ import annotations

import copy
import unittest

from scripts.fork_difference_inventory import (
    CATEGORIES,
    FORK_BASELINE,
    MERGE_BASE,
    carrier_changed_files,
    inventory,
    load_overrides,
    validate_carrier_slices,
)


class ForkDifferenceInventoryTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.data = inventory()

    def test_all_frozen_baseline_commits_are_classified(self) -> None:
        self.assertEqual(self.data["metadata"], {"merge_base": MERGE_BASE, "fork_baseline": FORK_BASELINE})
        self.assertEqual(self.data["counts"]["non_merge_commits"], 178)
        self.assertEqual(self.data["counts"]["overrides"], 33)
        self.assertGreater(self.data["counts"]["spec_referenced"], 0)
        self.assertEqual(self.data["errors"], [])

    def test_override_schema_and_known_33_commits(self) -> None:
        overrides = [row["classification"]["override"] for row in self.data["commits"] if row["classification"]["kind"] == "override"]
        self.assertEqual(len(overrides), 33)
        self.assertEqual(len({entry["commit"] for entry in overrides}), 33)
        for entry in overrides:
            self.assertIn(entry["category"], CATEGORIES)
            self.assertTrue(entry["rationale"])
            self.assertTrue(entry["affected_files"])
            self.assertTrue(entry["linked_sections"])
            self.assertTrue(entry["verification"])

    def test_upstream_v074_carrier_is_not_a_single_atomic_contract(self) -> None:
        carrier = next(row["classification"]["override"] for row in self.data["commits"] if row["commit"].startswith("336eecbe"))
        self.assertEqual(carrier["category"], "migration_carrier")
        self.assertIn("src/app", carrier["affected_files"])
        self.assertIn("tests", carrier["affected_files"])
        self.assertEqual({section.split(" ", 1)[0] for section in carrier["linked_sections"]}, {f"G{number}" for number in range(1, 10)})
        self.assertTrue(all("atomic review" in item for item in carrier["verification"]))

    def test_each_carrier_file_has_exactly_one_primary_slice(self) -> None:
        document = load_overrides()
        carriers = [entry for entry in document["overrides"] if entry["category"] == "migration_carrier"]
        self.assertEqual({entry["commit"][:8] for entry in carriers}, {"336eecbe", "10f1c6f3", "4c37ed0c"})
        for carrier in carriers:
            files = carrier_changed_files(carrier["commit"])
            self.assertTrue(files)
            self.assertEqual(validate_carrier_slices(carrier, files), [])

    def test_duplicate_primary_pattern_is_red(self) -> None:
        carrier = next(entry for entry in load_overrides()["overrides"] if entry["commit"].startswith("10f1c6f3"))
        mutated = copy.deepcopy(carrier)
        mutated["carrier_slices"][0]["path_patterns"].append("SPEC.md")
        errors = validate_carrier_slices(mutated, carrier_changed_files(mutated["commit"]))
        self.assertTrue(any("multiple primary patterns" in error for error in errors))

    def test_uncovered_carrier_file_is_red(self) -> None:
        carrier = next(entry for entry in load_overrides()["overrides"] if entry["commit"].startswith("4c37ed0"))
        mutated = copy.deepcopy(carrier)
        mutated["carrier_slices"][0]["path_patterns"].remove("Cargo.lock")
        errors = validate_carrier_slices(mutated, carrier_changed_files(mutated["commit"]))
        self.assertTrue(any("uncovered carrier file: Cargo.lock" in error for error in errors))


if __name__ == "__main__":
    unittest.main()
