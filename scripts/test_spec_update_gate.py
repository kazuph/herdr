from __future__ import annotations

import unittest

from scripts.spec_update_gate import Commit, needs_spec, violations


def commit(subject: str, files: tuple[str, ...], body: str = "") -> Commit:
    return Commit(sha="0123456789abcdef", subject=subject, body=body, files=files)


class SpecUpdateGateTests(unittest.TestCase):
    def test_feature_and_fix_commits_touching_runtime_code_need_spec(self) -> None:
        self.assertTrue(needs_spec(commit("feat: add run --machine", ("src/cli/comms.rs",))))
        self.assertTrue(needs_spec(commit("fix(client): keep keys", ("src/client/endpoint/runtime.rs",))))
        self.assertTrue(needs_spec(commit("feat!: change api", ("tests/multi_client.rs",))))

    def test_other_kinds_exempt_scopes_and_non_runtime_files_do_not(self) -> None:
        self.assertFalse(needs_spec(commit("docs: update readme", ("src/cli/comms.rs",))))
        self.assertFalse(needs_spec(commit("test: widen timeout", ("src/server/headless/tests/endpoints.rs",))))
        self.assertFalse(needs_spec(commit("release: kazuph v0.2.12", ("Cargo.toml",))))
        self.assertFalse(needs_spec(commit("fix(nix): refresh hash", ("nix/package.nix",))))
        self.assertFalse(needs_spec(commit("fix: correct docs link", ("docs/next/CHANGELOG.md",))))

    def test_opt_out_requires_a_reason(self) -> None:
        files = ("src/app/mod.rs",)
        self.assertFalse(needs_spec(commit("fix: rename helper", files, "Spec: none - internal rename only")))
        self.assertFalse(needs_spec(commit("fix: rename helper", files, "body\n\nspec: none: no behaviour change")))
        self.assertTrue(needs_spec(commit("fix: rename helper", files, "Spec: none")))
        self.assertTrue(needs_spec(commit("fix: rename helper", files, "Spec: none -")))

    def test_a_spec_change_anywhere_in_the_range_satisfies_the_gate(self) -> None:
        feature = commit("feat: add run --machine", ("src/cli/comms.rs",))
        spec = commit("docs(spec): record run --machine", ("SPEC.md", "formal/spec-evidence-manifest.json"))
        self.assertEqual(violations([feature]), [feature])
        self.assertEqual(violations([feature, spec]), [])
        self.assertEqual(violations([commit("feat: add x", ("src/a.rs", "SPEC.md"))]), [])


if __name__ == "__main__":
    unittest.main()
