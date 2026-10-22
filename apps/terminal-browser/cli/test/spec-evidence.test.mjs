import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import test from 'node:test';
const root = path.resolve(import.meta.dirname, '../../../..');
const manifest = JSON.parse(fs.readFileSync(path.join(root, 'formal/spec-evidence-manifest.json'), 'utf8'));
const inventory = JSON.parse(execFileSync('python3', ['-c', 'from scripts.spec_contract_inventory import inventory; from pathlib import Path; import json; print(json.dumps(inventory(Path("SPEC.md"))))'], { cwd: root, encoding: 'utf8' }));
const active = inventory.contracts.filter(c => c.status === 'unverified');
const contract = text => active.find(c => c.section === '同梱 terminal-browser の独立配布と安全な導入' && c.text.includes(text));
const evidence = text => { const c = contract(text); assert.ok(c, text); const row = manifest.contracts.find(r => r.contract_id === c.id); assert.ok(row, c.id); return row; };
test('manifest active count matches the actual SPEC inventory', () => assert.equal(manifest.metadata.active_contract_count, active.length));
test('retained tb installation contract keeps all four Rust regression mappings and adds upgrade evidence', () => {
  const row = evidence('wrapper は導入版');
  for (const name of ['tb_local_archive_install_upgrade_reinstall_and_registration_rollback','tb_extract_checksum_switch_failures_preserve_old_version','tb_registration_restore_preserves_exact_entry_and_other_plugins','tb_running_old_version_survives_upgrade']) assert.ok(row.verification_methods.includes(`rust_test_target:${name}`), name);
  assert.ok(row.verification_methods.includes('node_test_target:apps/terminal-browser/cli/test/upgrade.test.mjs'));
});
for (const [text, source, file] of [
  ['tb 0.7.3 は', 'apps/terminal-browser/terminals/src/multiplexer.ts', 'apps/terminal-browser/terminals/test/nested.integration.cjs'],
  ['ikesu の準備処理は', 'apps/terminal-browser/terminals/src/terminals/ikesu.ts', 'apps/terminal-browser/terminals/test/ikesu.integration.cjs'],
  ['release tar.gz は', 'apps/terminal-browser/scripts/stage-plugins.sh', 'apps/terminal-browser/cli/test/plugin-release.test.mjs'],
]) test(`manifest maps ${text} to its specific source and regression`, () => {
  const row = evidence(text);
  assert.ok(row.verification_methods.includes(`drift_source_anchor:${source}`));
  assert.ok(row.verification_methods.includes(`node_test_target:${file}`));
  for (const item of [source,file]) assert.ok(fs.existsSync(path.join(root,item)));
});
