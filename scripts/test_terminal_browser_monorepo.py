import json
import pathlib
import plistlib
import re
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[1]
APP = ROOT / "apps" / "terminal-browser"


class TerminalBrowserMonorepoTests(unittest.TestCase):
    def test_upstream_source_is_pinned(self):
        provenance = json.loads((APP / "UPSTREAM.json").read_text())
        self.assertEqual(provenance["tag"], "v0.7.1")
        self.assertEqual(
            provenance["commit"], "ca1b15a7e19a226b1520d68001d207819f91196f"
        )
        self.assertEqual(provenance["license"], "MIT")

    def test_drizzle_orm_uses_the_patched_release(self):
        package = json.loads((APP / "store" / "package.json").read_text())
        self.assertEqual(package["dependencies"]["drizzle-orm"], "0.45.2")
        lock = (APP / "pnpm-lock.yaml").read_text()
        self.assertIn("drizzle-orm@0.45.2", lock)
        self.assertNotIn("drizzle-orm@0.44.7", lock)

    def test_electron_download_uses_repo_pinned_checksums(self):
        fetch = (APP / "scripts" / "fetch-electron.sh").read_text()
        self.assertNotIn("SHASUMS256.txt", fetch)
        checksums = (APP / "scripts" / "electron-sha256.txt").read_text().splitlines()
        self.assertEqual(len(checksums), 3)
        for line in checksums:
            self.assertRegex(line, r"^[0-9a-f]{64} \*electron-v43\.3\.0-")

    def test_agent_browser_source_and_config_are_pinned(self):
        installer = (APP / "scripts" / "agent-browser.sh").read_text()
        self.assertIn("1ed371f3af472cc0d6cd8fdaea75d1a085ff7534", installer)
        self.assertIn('--rev "$REV"', installer)
        self.assertNotIn('--tag "$REF"', installer)
        self.assertEqual(
            json.loads((APP / "scripts" / "agent-browser.json").read_text()), {}
        )
        action = (APP / "cli" / "src" / "action.ts").read_text()
        self.assertIn("AGENT_BROWSER_CONFIG: agentBrowserConfigPath()", action)

    def test_macos_build_requires_a_valid_identity_and_strict_verification(self):
        release = (APP / "scripts" / "release.sh").read_text()
        self.assertIn("TERMINAL_BROWSER_CODE_SIGN_IDENTITY:?", release)
        self.assertIn("--options runtime --timestamp", release)
        self.assertIn("--verify --deep --strict", release)
        self.assertIn("sign_macos_app \"$APP\"", release)
        self.assertIn('sign_macos "$STAGE/browser/native/pixel.node"', release)
        self.assertIn("codesign team mismatch", release)
        self.assertNotRegex(release, re.compile(r"codesign[^\n]+--sign -"))
        entitlements = plistlib.loads(
            (APP / "scripts" / "entitlements.darwin.plist").read_bytes()
        )
        self.assertEqual(entitlements, {"com.apple.security.cs.allow-jit": True})

    def test_herdr_plugin_builds_and_runs_only_monorepo_source(self):
        manifest = (APP / "herdr-plugin" / "herdr-plugin.toml").read_text()
        build = (APP / "herdr-plugin" / "build.sh").read_text()
        opener = (APP / "herdr-plugin" / "open-split.sh").read_text()
        self.assertNotIn("curl", manifest)
        self.assertNotIn("http", manifest)
        self.assertIn('command = ["bash", "build.sh"]', manifest)
        self.assertIn("CI=1 corepack pnpm install --frozen-lockfile", build)
        self.assertIn("dist-release/terminal-browser/bin/terminal-browser", opener)
        self.assertNotIn("command -v terminal-browser", opener)


if __name__ == "__main__":
    unittest.main()
