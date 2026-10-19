"""Credential-free checks for the fork-only tb distribution surface."""
import os
from pathlib import Path
import subprocess
import unittest

ROOT = Path(__file__).resolve().parents[1]
APP = ROOT / 'apps/terminal-browser'


class TbReleaseTests(unittest.TestCase):
    def test_signing_missing_credentials_stops_before_tools_or_build(self):
        env = dict(os.environ)
        env.pop('MACOS_DEVELOPER_ID_P12', None)
        env.pop('MACOS_DEVELOPER_ID_P12_PASSWORD', None)
        result = subprocess.run(['bash', str(APP / 'scripts/herdr-sign-release.sh'), '0.7.1'], env=env, text=True, capture_output=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('certificate missing', result.stderr)
        self.assertEqual(result.stdout, '')

    def test_release_has_build_free_plugin_and_keeps_source_plugin(self):
        import tomllib
        release = tomllib.loads((APP / 'herdr-plugin-release/herdr-plugin.toml').read_text())
        source = tomllib.loads((APP / 'herdr-plugin/herdr-plugin.toml').read_text())
        self.assertEqual(release['id'], source['id'])
        self.assertNotIn('build', release)
        self.assertEqual(source['build'][0]['command'], ['bash', 'build.sh'])
        self.assertEqual(release['actions'][0]['id'], 'open-split')
        self.assertIn('"$ROOT/bin/terminal-browser" open --split right', (APP / 'herdr-plugin-release/open-split.sh').read_text())
        self.assertIn('dist-release/terminal-browser/bin/terminal-browser', (APP / 'herdr-plugin/open-split.sh').read_text())

    def test_upgrade_has_no_upstream_release_origin_or_close(self):
        upgrade = (APP / 'cli/src/upgrade.ts').read_text()
        self.assertNotIn('terminal-browser.sh', upgrade)
        self.assertNotIn('TERMINAL_BROWSER_RELEASE_ORIGIN', upgrade)
        self.assertNotIn('instances', upgrade)
        self.assertIn('spawn("herdr", ["tb", "install"]', upgrade)


if __name__ == '__main__':
    unittest.main()
