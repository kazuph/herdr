from __future__ import annotations

import copy
import unittest

from scripts.upstream_v080_parity_inventory import (
    CommitMetadata,
    behavior_slices,
    carrier_review_required,
    validate_inventory,
    validate_packet_links,
)


COMMIT = "a" * 40
OTHER_COMMIT = "b" * 40
PACKET_ID = "UP-0001-non-utf8-argv"


def metadata() -> list[CommitMetadata]:
    return [CommitMetadata(COMMIT, "feat(cli): add a command", ("src/cli.rs",))]


def inventory() -> dict:
    return {
        "schema_version": 1,
        "scope": {
            "kind": "upstream commit index",
            "parity_completion_denominator": False,
            "reason": "Commit rows are not behavior-level parity evidence.",
        },
        "upstream": {
            "merge_base": "9c9490d764d306b6cc093b5b3de1ccd4e6467c94",
            "release": "346411fa21afd297f5ed3b3fa56f9e3fbf7654b7",
            "expected_non_merge_count": 1,
        },
        "accepted_packets": [{"id": PACKET_ID, "source_commit": COMMIT}],
        "rows": [
            {
                "commit": COMMIT,
                "subject": "feat(cli): add a command",
                "changed_files": ["src/cli.rs"],
                "feature_group": "CLI",
                "domain": "CLI, help, argument parsing, config, and error behavior",
                "preliminary_disposition": "source_accept",
                "evidence": "accepted packet manifest",
                "missing_work": "none",
                "dependencies": [],
                "platform_gap": ["unassessed"],
                "accepted_packet": PACKET_ID,
                "carrier_review_required": False,
                "behavior_slices": [
                    {
                        "files": ["src/cli.rs"],
                        "feature_group": "CLI",
                        "domain": "CLI, help, argument parsing, config, and error behavior",
                        "status": "source_accept",
                        "primary": True,
                    }
                ],
            }
        ],
    }


class UpstreamV080ParityInventoryTests(unittest.TestCase):
    def test_accepts_complete_current_metadata(self) -> None:
        self.assertEqual(validate_inventory(inventory(), metadata()), [])

    def test_rejects_missing_commit(self) -> None:
        data = inventory()
        data["rows"] = []

        self.assertIn(f"missing commit {COMMIT}", validate_inventory(data, metadata()))

    def test_rejects_inventory_scope_that_claims_a_parity_denominator(self) -> None:
        data = inventory()
        data["scope"]["parity_completion_denominator"] = True

        self.assertIn(
            "inventory scope must state that commit rows are not the parity completion denominator",
            validate_inventory(data, metadata()),
        )

    def test_rejects_duplicate_commit(self) -> None:
        data = inventory()
        data["rows"].append(copy.deepcopy(data["rows"][0]))

        self.assertIn(f"duplicate commit {COMMIT}", validate_inventory(data, metadata()))

    def test_rejects_out_of_range_commit(self) -> None:
        data = inventory()
        data["rows"][0]["commit"] = OTHER_COMMIT

        self.assertIn(f"out-of-range commit {OTHER_COMMIT}", validate_inventory(data, metadata()))

    def test_rejects_stale_subject_and_changed_files(self) -> None:
        data = inventory()
        data["rows"][0]["subject"] = "stale subject"
        data["rows"][0]["changed_files"] = ["src/stale.rs"]

        errors = validate_inventory(data, metadata())
        self.assertIn(f"stale subject for {COMMIT}", errors)
        self.assertIn(f"stale changed_files for {COMMIT}", errors)

    def test_rejects_unknown_preliminary_disposition(self) -> None:
        data = inventory()
        data["rows"][0]["preliminary_disposition"] = "equivalent"

        self.assertIn(
            f"unknown preliminary_disposition for {COMMIT}: equivalent",
            validate_inventory(data, metadata()),
        )

    def test_rejects_dead_dependency(self) -> None:
        data = inventory()
        data["rows"][0]["dependencies"] = ["UP-9999-missing"]

        self.assertIn(
            f"dead dependency for {COMMIT}: UP-9999-missing",
            validate_inventory(data, metadata()),
        )

    def test_rejects_unlinked_accepted_packet(self) -> None:
        data = inventory()
        data["rows"][0]["accepted_packet"] = "UP-9999-missing"

        self.assertIn(
            f"unlinked accepted_packet for {COMMIT}: UP-9999-missing",
            validate_inventory(data, metadata()),
        )

    def test_validates_accepted_packet_source_row_link(self) -> None:
        packets = [{"id": PACKET_ID, "source_commit": COMMIT, "manifest": "packet.json"}]
        self.assertEqual(validate_packet_links(inventory(), packets), [])

        data = inventory()
        del data["rows"][0]["accepted_packet"]
        self.assertIn(
            f"accepted packet source row not linked: {PACKET_ID}",
            validate_packet_links(data, packets),
        )

    def test_rejects_empty_behavior_slices(self) -> None:
        data = inventory()
        data["rows"][0]["behavior_slices"] = []

        self.assertIn(f"empty behavior_slices for {COMMIT}", validate_inventory(data, metadata()))

    def test_rejects_file_without_a_primary_behavior_slice(self) -> None:
        data = inventory()
        data["rows"][0]["behavior_slices"][0]["files"] = []

        self.assertIn(
            f"file not covered by a primary slice for {COMMIT}: src/cli.rs",
            validate_inventory(data, metadata()),
        )

    def test_rejects_file_in_multiple_primary_behavior_slices(self) -> None:
        data = inventory()
        data["rows"][0]["behavior_slices"].append(
            {
                "files": ["src/cli.rs"],
                "feature_group": "CLI duplicate",
                "domain": "CLI, help, argument parsing, config, and error behavior",
                "status": "unassessed",
                "primary": True,
            }
        )

        self.assertIn(
            f"multiple primary slices for {COMMIT}: src/cli.rs",
            validate_inventory(data, metadata()),
        )

    def test_requires_unassessed_carrier_review_for_single_slice_multi_file_commit(self) -> None:
        data = inventory()
        data["rows"][0]["changed_files"] = ["src/cli.rs", "src/main.rs"]
        data["rows"][0]["behavior_slices"][0]["files"] = ["src/cli.rs", "src/main.rs"]
        data["rows"][0]["carrier_review_required"] = False
        multi_file_metadata = [
            CommitMetadata(COMMIT, "feat(cli): add a command", ("src/cli.rs", "src/main.rs"))
        ]

        self.assertIn(
            f"carrier review required for {COMMIT}",
            validate_inventory(data, multi_file_metadata),
        )

        data["rows"][0]["carrier_review_required"] = True
        data["rows"][0]["preliminary_disposition"] = "source_accept"
        self.assertIn(
            f"carrier review must remain unassessed for {COMMIT}",
            validate_inventory(data, multi_file_metadata),
        )

    def test_requires_review_for_every_known_multi_domain_carrier(self) -> None:
        for prefix in ("3f809476", "d30ab1b5", "8afd52ae"):
            item = CommitMetadata(
                prefix + "0" * 32,
                "known carrier",
                ("src/cli.rs", "src/server/headless.rs"),
            )
            slices = behavior_slices(item.changed_files, "unassessed")
            self.assertGreater(len(slices), 1)
            self.assertTrue(carrier_review_required(item, slices, "unassessed"))

    def test_rejects_source_accept_without_packet_and_matching_slice_status(self) -> None:
        data = inventory()
        del data["rows"][0]["accepted_packet"]
        data["rows"][0]["behavior_slices"][0]["status"] = "unassessed"

        errors = validate_inventory(data, metadata())
        self.assertIn(
            f"terminal implementation disposition requires an accepted packet for {COMMIT}",
            errors,
        )
        self.assertIn(f"slice status does not match row disposition for {COMMIT}", errors)

    def test_rejects_terminal_disposition_with_unassessed_evidence(self) -> None:
        data = inventory()
        data["rows"][0]["preliminary_disposition"] = "already_equivalent"
        data["rows"][0]["behavior_slices"][0]["status"] = "already_equivalent"
        del data["rows"][0]["accepted_packet"]
        data["rows"][0]["evidence"] = "upstream commit metadata only; fork behavior is unassessed"
        data["rows"][0]["missing_work"] = (
            "assess fork behavior, contracts, evidence, and packet dependencies"
        )

        self.assertIn(
            f"terminal disposition still uses unassessed evidence for {COMMIT}",
            validate_inventory(data, metadata()),
        )

    def test_rejects_stale_inventory_packet_entry(self) -> None:
        data = inventory()
        data["accepted_packets"].append({"id": "UP-9999-stale", "source_commit": OTHER_COMMIT})

        self.assertIn(
            "stale accepted packet entry: UP-9999-stale",
            validate_packet_links(
                data,
                [{"id": PACKET_ID, "source_commit": COMMIT, "manifest": "packet.json"}],
            ),
        )

    def test_rejects_reusing_an_accepted_packet_for_another_commit(self) -> None:
        data = inventory()
        reused = copy.deepcopy(data["rows"][0])
        reused["commit"] = OTHER_COMMIT
        reused["subject"] = "another upstream behavior"
        data["rows"].append(reused)
        other_metadata = [
            *metadata(),
            CommitMetadata(OTHER_COMMIT, "another upstream behavior", ("src/cli.rs",)),
        ]

        errors = validate_inventory(data, other_metadata)
        self.assertIn(
            f"accepted packet source commit does not match row for {OTHER_COMMIT}: {PACKET_ID}",
            errors,
        )
        self.assertIn(
            f"accepted packet must be used by exactly one row: {PACKET_ID}",
            errors,
        )


if __name__ == "__main__":
    unittest.main()
