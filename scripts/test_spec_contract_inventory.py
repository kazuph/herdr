from __future__ import annotations

import unittest
from pathlib import Path

from scripts.spec_contract_inventory import inventory


ROOT = Path(__file__).resolve().parents[1]


class SpecContractInventoryTests(unittest.TestCase):
    def test_current_spec_contract_counts_are_stable(self) -> None:
        data = inventory(ROOT / "SPEC.md")

        self.assertEqual(data["counts"], {"raw": 322, "active": 314, "retired": 8})
        self.assertEqual(
            data["domains"],
            {
                "G1": {"raw": 41, "active": 41, "retired": 0},
                "G2": {"raw": 76, "active": 76, "retired": 0},
                "G3": {"raw": 30, "active": 30, "retired": 0},
                "G4": {"raw": 23, "active": 23, "retired": 0},
                "G5": {"raw": 40, "active": 40, "retired": 0},
                "G6": {"raw": 34, "active": 26, "retired": 8},
                "G7": {"raw": 9, "active": 9, "retired": 0},
                "G8": {"raw": 24, "active": 24, "retired": 0},
                "G9": {"raw": 45, "active": 45, "retired": 0},
            },
        )

    def test_each_contract_has_a_content_stable_domain_id_and_source_line(self) -> None:
        contracts = inventory(ROOT / "SPEC.md")["contracts"]

        self.assertEqual(len({contract["id"] for contract in contracts}), len(contracts))
        self.assertTrue(all(contract["domain"] in contract["id"] for contract in contracts))
        self.assertTrue(all(contract["line"] > 0 for contract in contracts))
        self.assertTrue(all(contract["text"] for contract in contracts))


if __name__ == "__main__":
    unittest.main()
