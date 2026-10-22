"""Credential-free checks of the decision dialog release contract."""
import os
from pathlib import Path
import subprocess
import unittest

ROOT = Path(__file__).resolve().parents[1]
APP = ROOT / 'apps/mac-dialog-decisions'

class MacDialogReleaseTests(unittest.TestCase):
    def test_missing_signing_credentials_fails_before_build(self):
        env = dict(os.environ)
        env.pop('MACOS_DEVELOPER_ID_P12', None)
        env.pop('MACOS_DEVELOPER_ID_P12_PASSWORD', None)
        result = subprocess.run(['bash', str(APP / 'scripts/herdr-sign-release.sh'), '0.1.0'], env=env, capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('certificate missing', result.stderr)
        self.assertEqual(result.stdout, '')

    def test_workflow_is_separate_pinned_fail_closed_and_keeps_latest(self):
        import re
        workflow = (ROOT / '.github/workflows/kazuph-mac-dialog-release.yml').read_text()
        self.assertIn("tags: ['kazuph-mac-dialog-v*']", workflow)
        self.assertTrue(all(re.fullmatch(r'[^@]+@[0-9a-f]{40}', value) for value in re.findall(r'uses: (\S+)', workflow)))
        self.assertIn('test "$status" = 404', workflow)
        self.assertIn('--draft', workflow)
        self.assertIn('--latest=false', workflow)
        self.assertNotIn('kazuph-tb-v', workflow)
        self.assertNotIn('website/latest.json', workflow)
        signer = (APP / 'scripts/herdr-sign-release.sh').read_text()
        for flag in ['--verify --deep --strict', '--test-requirement', '67M8FDFZ28', 'anchor apple generic', 'subject.CN', 'subject.OU', 'trap cleanup EXIT']:
            self.assertIn(flag, signer)
        self.assertNotIn('notarytool', signer)

if __name__ == '__main__':
    unittest.main()
