const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { spawn, spawnSync, execFileSync, execFile } = require('node:child_process');
const { promisify } = require('node:util');
const { test } = require('node:test');
const { shellQuote } = require('../dist/shared.js');
const exec = promisify(execFile);
const bins = { ikesu: process.env.IKESU_TB_TEST_BIN, herdr: process.env.HERDR_TB_TEST_BIN };
for (const name of Object.keys(bins)) assert.ok(bins[name], `real ${name} binary required`);

async function eventually(check) {
  const end = Date.now() + 15000;
  let error;
  while (Date.now() < end) {
    try { return await check(); } catch (e) { error = e; }
    await new Promise(resolve => setTimeout(resolve, 50));
  }
  throw error;
}

for (const [outer, inner] of [['ikesu', 'herdr'], ['herdr', 'ikesu']]) for (const mode of ['symlink', 'alias', 'disappeared']) {
  test(`real ${outer} -> ${inner} (${mode}): executable identity and false pane fail closed`, async () => {
    const dir = fs.mkdtempSync('/tmp/tb-nest-');
    const base = { ...process.env };
    for (const key of Object.keys(base)) if (/^(IKESU_|HERDR_|GTX_|GOTERRACE_|XDG_)/.test(key)) delete base[key];
    const contexts = {};
    for (const name of [outer, inner]) {
      const home = path.join(dir, name);
      fs.mkdirSync(home);
      const bin = path.join(home, name === inner && mode !== 'symlink' ? `${name}-dev` : name);
      if (name === inner && mode !== 'symlink') fs.copyFileSync(fs.realpathSync(bins[name]), bin);
      else fs.symlinkSync(bins[name], bin);
      const config = path.join(home, 'config.toml');
      fs.writeFileSync(config, '[experimental]\nallow_nested = true\n');
      const env = { ...base, HOME: home, XDG_CONFIG_HOME: path.join(home, '.config'), XDG_DATA_HOME: path.join(home, 'data'), XDG_STATE_HOME: path.join(home, 'state'), SHELL: '/bin/sh' };
      const prefix = name.toUpperCase();
      env[`${prefix}_BIN_PATH`] = bin;
      if (name === 'ikesu') {
        env.IKESU_CONFIG = config;
        env.IKESU_STATE_DIR = path.join(home, 'runtime');
        env.IKESU_DB_PATH = path.join(home, 'db.sqlite');
        env.IKESU_SOCKET_PATH = path.join(home, 'runtime/default/ikesu.sock');
      } else {
        env.HERDR_CONFIG_PATH = config;
        env.HERDR_SOCKET_PATH = path.join(home, 'herdr.sock');
        env.HERDR_SOCKET_PATH_EXPLICIT = '1';
      }
      contexts[name] = { home, bin, env, cli: async args => { const {stdout}=await exec(bins[name],args,{env}); return stdout.trim() ? JSON.parse(stdout) : null; } };
    }
    const outerLog = fs.openSync(path.join(dir, 'outer.log'), 'w');
    const server = spawn(contexts[outer].bin, ['server'], { env: contexts[outer].env, stdio: ['ignore', outerLog, outerLog] });
    try {
      await eventually(() => contexts[outer].cli(['pane', 'list']));
      const created = await contexts[outer].cli(['workspace', 'create', '--cwd', dir]);
      const outerPane = created.result.root_pane;
      assert.ok(outerPane?.pane_id, JSON.stringify(created));
      const changedEnv = Object.entries(contexts[inner].env).filter(([key, value]) => base[key] !== value).map(([key,value]) => `${key}=${value}`);
      await contexts[outer].cli(['pane', 'run', outerPane.pane_id, shellQuote(['env', ...changedEnv, contexts[inner].bin, 'server']) + ` > ${shellQuote([path.join(dir, 'inner.log')])} 2>&1`]);
      await eventually(() => contexts[inner].cli(['pane', 'list']));
      const child = await contexts[inner].cli(['workspace', 'create', '--cwd', dir]);
      const innerPane = child.result.root_pane;
      assert.ok(innerPane?.pane_id, JSON.stringify(child));
      const output = path.join(dir, 'selection.json');
      if (mode === 'disappeared') fs.unlinkSync(contexts[inner].bin);
      const probe = path.resolve(__dirname, 'nested-probe.cjs');
      const safePath = path.join(dir, 'commands');
      fs.mkdirSync(safePath);
      for (const [name, source] of Object.entries({ ps: '/bin/ps', ...bins })) fs.symlinkSync(source, path.join(safePath, name));
      await contexts[inner].cli(['pane', 'run', innerPane.pane_id, shellQuote(['env', `PATH=${safePath}`, process.execPath, probe, output, inner]) + ' </dev/null']);
      const result = await eventually(() => JSON.parse(fs.readFileSync(output, 'utf8')));
      if (mode === 'disappeared') {
        assert.match(result.error, /Cannot establish the inner/);
        console.log('PASS disappeared executable refuses outer runtime', outer, inner);
        return;
      }
      assert.equal(result.name, inner, JSON.stringify(result));
      assert.ok(result.herdr && result.ikesu, 'both runtime environments must really coexist');
      assert.equal(result.pane.id, innerPane.pane_id);
      assert.equal(result.rejected, true);
      assert.equal(result.upgradeRejected, true);
      assert.equal(result.invalidPaneSpawnedInstall, false);
      assert.equal(result.target.runtime, inner);
      assert.deepEqual(result.commands.find(c => c.args.join(' ') === 'tb install'), { bin: contexts[inner].bin, args: ['tb', 'install'] });
      console.log('PASS nested', outer, '->', inner, JSON.stringify(result), dir);
    } finally {
      const pids = execFileSync('pgrep', ['-u', String(process.getuid()), '-f', dir], { encoding: 'utf8' }).trim().split('\n').map(Number);
      for (const pid of pids) {
        const info = spawnSync('ps', ['-p', String(pid), '-o', 'command='], { encoding: 'utf8' });
        if (info.status !== 0) continue;
        if (info.stdout.includes(dir) && /\/(herdr|ikesu)(-dev)? server\s*$/.test(info.stdout)) execFileSync('kill', [String(pid)]);
      }
      if (server.exitCode === null) await new Promise(resolve => server.once('exit', resolve));
      fs.closeSync(outerLog);
    }
  });
}

test('orphaned runtime variables fail pane detection but upgrade really spawns herdr tb install', async () => {
  const dir = fs.mkdtempSync('/tmp/tb-orphan-');
  const output = path.join(dir, 'result.json');
  const safePath = path.join(dir, 'commands');
  fs.mkdirSync(safePath);
  for (const [name, source] of Object.entries({ ps: '/bin/ps', ...bins })) fs.symlinkSync(source, path.join(safePath, name));
  const env = { TB_TEST_APP_ROOT: process.env.TB_TEST_APP_ROOT, HOME: dir, PATH: safePath, HERDR_PANE_ID: 'p1', IKESU_PANE_ID: 'p1', HERDR_BIN_PATH: bins.herdr, IKESU_BIN_PATH: bins.ikesu };
  await exec('/bin/sh', ['-c', shellQuote([process.execPath, path.resolve(__dirname, 'nested-probe.cjs'), output, 'herdr', 'orphan']) + ' </dev/null >/dev/null 2>&1 &'], { env });
  const result = await eventually(() => JSON.parse(fs.readFileSync(output, 'utf8')));
  assert.match(result.selectionError, /Cannot establish the inner herdr\/ikesu runtime/);
  assert.equal(result.target?.runtime, 'herdr', JSON.stringify(result));
  assert.deepEqual(result.commands.find(c => c.args.join(' ') === 'tb install'), { bin: bins.herdr, args: ['tb', 'install'] });
  console.log('PASS orphan fallback observes actual subprocess', JSON.stringify(result));
});
