import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import test from 'node:test';

test('release plugin staging puts both versioned definitions into the real tar archive', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'tb-plugins-'));
  try {
    const stage = path.join(dir, 'terminal-browser');
    fs.mkdirSync(stage);
    execFileSync('bash', [new URL('../../scripts/stage-plugins.sh', import.meta.url).pathname, stage, '0.7.3']);
    const archive = path.join(dir, 'plugins.tar.gz');
    execFileSync('tar', ['-czf', archive, '-C', dir, 'terminal-browser']);
    const listing = execFileSync('tar', ['-tzf', archive], { encoding: 'utf8' });
    for (const runtime of ['herdr', 'ikesu']) {
      const member = `terminal-browser/${runtime}-plugin/${runtime}-plugin.toml`;
      assert.ok(listing.includes(member));
      const text = execFileSync('tar', ['-xOzf', archive, member], { encoding: 'utf8' });
      assert.match(text, /version = "0.7.3"/);
      if (runtime === 'ikesu') assert.match(text, /min_ikesu_version = "0.4.1"/);
      const script = fs.readFileSync(path.join(stage, `${runtime}-plugin/open-split.sh`), 'utf8');
      assert.match(script, /exec "\$ROOT\/bin\/terminal-browser" open --split right/);
    }
  } finally { fs.rmSync(dir, { recursive: true }); }
});
