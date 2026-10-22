const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawn, execFileSync, execFile } = require('node:child_process');
const { promisify } = require('node:util');
const { test } = require('node:test');
const { detect, shellIn } = require('../dist/index.js');
const exec = promisify(execFile);
const bin = process.env.IKESU_TB_TEST_BIN;
assert.ok(bin, 'IKESU_TB_TEST_BIN must name the real ikesu binary');

async function eventually(check) {
  const end = Date.now() + 10000;
  let error;
  while (Date.now() < end) {
    try { return await check(); } catch (e) { error = e; }
    await new Promise(resolve => setTimeout(resolve, 50));
  }
  throw error;
}

test('real isolated ikesu: pane identity, all split arguments, config destinations, and false values', async () => {
  const dir = fs.mkdtempSync('/tmp/tb-ikesu-');
  const localBin = path.join(dir, 'ikesu');
  fs.symlinkSync(bin, localBin);
  const env = { ...process.env, HOME: dir, IKESU_SOCKET_PATH: path.join(dir, 'state/default/ikesu.sock'), IKESU_STATE_DIR: path.join(dir, 'state'), IKESU_DB_PATH: path.join(dir, 'state.sqlite'), IKESU_CONFIG: path.join(dir, '.config/ikesu/config.toml'), XDG_CONFIG_HOME: path.join(dir, '.config'), SHELL: '/bin/sh' };
  for (const key of Object.keys(env)) if (/^(HERDR_|GTX_|GOTERRACE_)/.test(key) || /^IKESU_(PANE_ID|TAB_ID|WORKSPACE_ID|CONFIG_PATH)$/.test(key)) delete env[key];
  fs.mkdirSync(path.dirname(env.IKESU_CONFIG), { recursive: true });
  fs.writeFileSync(env.IKESU_CONFIG, '[experimental]\nallow_nested = true\n');
  const log = fs.openSync(path.join(dir, 'server.log'), 'w');
  const server = spawn(localBin, ['server'], { env, stdio: ['ignore', log, log] });
  const cli = async (args) => JSON.parse((await exec(localBin, args, { env })).stdout);
  try {
    await eventually(async () => { if (server.exitCode !== null) throw new Error(fs.readFileSync(path.join(dir, 'server.log'), 'utf8')); return await cli(['server', 'ping']); });
    const created = await cli(['workspace', 'create', '--cwd', dir]);
    console.log('workspace', JSON.stringify(created));
    const pane = created.result.root_pane;
    assert.ok(pane?.pane_id);
    Object.assign(env, { IKESU_PANE_ID: pane.pane_id, IKESU_TAB_ID: pane.tab_id, IKESU_BIN_PATH: localBin });
    const calls = [];
    const realRun = shellIn(env);
    const run = async (bin, args, input) => { calls.push([bin, ...args]); return realRun(bin, args, input); };
    const terminal = detect(env, run);
    assert.deepEqual(await terminal.getCurrentPane({ tty: null, cwd: dir }), { id: pane.pane_id, tab: pane.tab_id });
    const herdrConfig = path.join(dir, '.config/herdr/config.toml');
    fs.mkdirSync(path.dirname(herdrConfig), { recursive: true });
    fs.writeFileSync(herdrConfig, 'onboarding = false\n');
    await terminal.prepare();
    assert.match(fs.readFileSync(env.IKESU_CONFIG, 'utf8'), /kitty_graphics = true/);
    assert.equal(fs.readFileSync(herdrConfig, 'utf8'), 'onboarding = false\n');
    const explicit = path.join(dir, 'explicit.toml');
    fs.writeFileSync(explicit, '[experimental]\nkitty_graphics = false\n');
    await detect({ ...env, IKESU_CONFIG_PATH: explicit }, run).prepare();
    assert.equal(fs.readFileSync(explicit, 'utf8'), '[experimental]\nkitty_graphics = true\n');
    for (const direction of ['right', 'left', 'down', 'up']) {
      const marker = path.join(dir, `ran-${direction}`);
      const start = calls.length;
      await terminal.split({ from: { id: pane.pane_id, tab: pane.tab_id }, direction, command: [process.execPath, '-e', `require('fs').writeFileSync(${JSON.stringify(marker)},'ran')`], size: 0.4, tty: null });
      await eventually(() => assert.equal(fs.readFileSync(marker, 'utf8'), 'ran'));
      const commands = calls.slice(start);
      assert.deepEqual(commands[1], [localBin, 'pane', 'split', '--pane', pane.pane_id, '--direction', ['up','down'].includes(direction) ? 'down' : 'right', '--focus', '--ratio', '0.4']);
      assert.equal(commands.length, ['left','up'].includes(direction) ? 4 : 3);
      console.log(direction, JSON.stringify(commands));
    }
    const bad = detect({ ...env, IKESU_PANE_ID: 'p999999999' });
    await assert.rejects(bad.getCurrentPane({ tty: null, cwd: dir }), /Cannot verify ikesu pane/);
    await assert.rejects(bad.prepare(), /Cannot verify ikesu pane/);
    const before = fs.readFileSync(env.IKESU_CONFIG, 'utf8');
    assert.equal(fs.readFileSync(env.IKESU_CONFIG, 'utf8'), before);
    fs.writeFileSync(env.IKESU_CONFIG, '[experimental\n');
    fs.writeFileSync(explicit, '[experimental]\nkitty_graphics = false\n');
    await assert.rejects(detect({ ...env, IKESU_CONFIG_PATH: explicit }, run).prepare(), /configuration was not changed/);
    assert.equal(fs.readFileSync(explicit, 'utf8'), '[experimental]\nkitty_graphics = false\n');
    console.log('PASS real pane validation / four split directions / default + explicit config / herdr untouched / false pane fails closed', dir);
  } finally {
    if (server.exitCode === null) {
    const pids = execFileSync('pgrep', ['-u', String(process.getuid()), '-f', dir], { encoding: 'utf8' }).trim().split('\n');
    assert.ok(pids.includes(String(server.pid)), 'only terminate the proven isolated server');
    execFileSync('kill', [String(server.pid)]);
    await new Promise(resolve => server.once('exit', resolve));
    }
    fs.closeSync(log);
  }
});
