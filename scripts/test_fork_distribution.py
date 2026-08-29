from __future__ import annotations

import subprocess
import tempfile
import unittest
from pathlib import Path

from scripts import fork_npm_packages, fork_release_input, render_fork_homebrew_formula


ROOT = Path(__file__).resolve().parents[1]
WORKFLOW = ROOT / ".github/workflows/kazuph-release.yml"


class ForkReleaseInputTests(unittest.TestCase):
    def test_accepts_the_fork_repository_matching_tag_and_cargo_version(self) -> None:
        self.assertEqual(
            fork_release_input.version_for_release("kazuph/herdr", "kazuph-v0.1.0", "0.1.0"),
            "0.1.0",
        )

    def test_rejects_other_repository(self) -> None:
        with self.assertRaisesRegex(ValueError, "repository"):
            fork_release_input.version_for_release("ogulcancelik/herdr", "kazuph-v0.1.0", "0.1.0")

    def test_rejects_malformed_or_mismatched_tag(self) -> None:
        for tag in ("v0.1.0", "kazuph-v0.1", "kazuph-v0.1.0-beta", "kazuph-v0.01.0", "kazuph-v01.1.0"):
            with self.subTest(tag=tag), self.assertRaisesRegex(ValueError, "tag"):
                fork_release_input.version_for_release("kazuph/herdr", tag, "0.1.0")
        with self.assertRaisesRegex(ValueError, "Cargo.toml"):
            fork_release_input.version_for_release("kazuph/herdr", "kazuph-v0.1.0", "0.1.1")


class FormulaTests(unittest.TestCase):
    def test_generates_a_platform_specific_tap_formula(self) -> None:
        checksums = {asset: "a" * 64 for asset in render_fork_homebrew_formula.ASSETS}
        generated = render_fork_homebrew_formula.formula("0.1.0", checksums)
        self.assertIn("kazuph-v0.1.0/herdr-macos-aarch64", generated)
        self.assertIn("kazuph-v0.1.0/herdr-linux-x86_64", generated)
        self.assertIn("chmod 0755, artifact", generated)
        self.assertIn('bin.install artifact => "herdr"', generated)
        self.assertIn("assert_match version.to_s", generated)


class NpmPackageTests(unittest.TestCase):
    def test_scoped_wrapper_and_native_package_metadata_are_statically_complete(self) -> None:
        fork_npm_packages.assert_package_contracts("0.2.0")
        launcher = (ROOT / "npm/packages/herdr/bin/herdr").read_text(encoding="utf-8")
        self.assertTrue(launcher.startswith("#!/bin/sh\n"))
        self.assertIn('while [ -L "$launcher" ]', launcher)
        self.assertIn('exec "$binary" "$@"', launcher)
        self.assertTrue((ROOT / "npm/packages/herdr/bin/herdr").stat().st_mode & 0o111)


class WorkflowTests(unittest.TestCase):
    def test_workflow_parses_and_has_only_fork_release_operations(self) -> None:
        subprocess.run(["ruby", "-e", "require 'yaml'; YAML.load_file(ARGV.fetch(0))", str(WORKFLOW)], check=True)
        workflow = WORKFLOW.read_text(encoding="utf-8")
        self.assertIn("Fail closed outside the fork release boundary", workflow)
        self.assertIn('needs: [preflight, build]', workflow)
        self.assertIn('needs: [preflight, test-npm]', workflow)
        self.assertIn('needs: [preflight, package-npm, release]', workflow)
        self.assertIn('needs: [preflight, package-npm, publish-native-npm]', workflow)
        self.assertIn('needs: [preflight, release]', workflow)
        self.assertIn('needs: [preflight, verify-homebrew-formula, publish-wrapper-npm]', workflow)
        self.assertIn('needs: [preflight, publish-homebrew-tap]', workflow)
        self.assertIn('VERSION="${GITHUB_REF_NAME#kazuph-v}"', workflow)
        self.assertIn("repository: kazuph/homebrew-tap", workflow)
        self.assertIn("ssh-key: ${{ secrets.HOMEBREW_TAP_DEPLOY_KEY }}", workflow)
        self.assertIn("git -C tap-credential-check commit --allow-empty", workflow)
        self.assertIn("git -C tap-credential-check push --dry-run origin HEAD:main", workflow)
        self.assertIn("brew tap-new kazuph/herdr-ci", workflow)
        self.assertIn("brew install kazuph/herdr-ci/herdr", workflow)
        self.assertIn("brew test kazuph/herdr-ci/herdr", workflow)
        self.assertIn("brew install kazuph/tap/herdr", workflow)
        self.assertIn("npm publish \"$ARCHIVE\" --access public --provenance", workflow)
        self.assertIn("npm install --global npm@11.18.0", workflow)
        self.assertNotIn("NPM_TOKEN", workflow)
        self.assertNotIn("NODE_AUTH_TOKEN", workflow)
        self.assertIn("oven-sh/setup-bun@0c5077e51419868618aeaa5fe8019c62421857d6", workflow)
        self.assertIn("target: aarch64-unknown-linux-musl\n            os: ubuntu-24.04-arm", workflow)
        self.assertNotIn("Configure Linux aarch64 musl toolchain", workflow)
        self.assertNotIn("aarch64-linux-gnu-gcc", workflow)
        for forbidden in ("website/latest.json", "issues: write", "close-released-issues", "ogulcancelik/herdr"):
            self.assertNotIn(forbidden, workflow)


if __name__ == "__main__":
    unittest.main()
