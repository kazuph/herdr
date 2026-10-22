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

for (const [outer, inner] of [['ikesu', 'herdr'], ['herdr', 'ikesu']]) {
  test(`real ${outer} -> ${inner}: nearest runtime wins and false selected pane fails closed`, async () => {
    const dir = fs.mkdtempSync('/tmp/tb-nest-');
    const base = { ...process.env };
    for (const key of Object.keys(base)) if (/^(IKESU_|HERDR_|GTX_|GOTERRACE_|XDG_)/.test(key)) delete base[key];
    const contexts = {};
    for (const name of [outer, inner]) {
      const home = path.join(dir, name);
      fs.mkdirSync(home);
      const bin = path.join(home, name);
      fs.symlinkSync(bins[name], bin);
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
      contexts[name] = { home, bin, env, cli: async args => { const {stdout}=await exec(bin,args,{env}); return stdout.trim() ? JSON.parse(stdout) : null; } };
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
      const probe = path.join(dir, 'probe.cjs');
      const modulePath = path.resolve(__dirname, '../dist/index.js');
      fs.writeFileSync(probe, `const fs=require('fs'); const {detect,currentMultiplexer}=require(${JSON.stringify(modulePath)}); (async()=>{const env={...process.env}; const good=detect(env); const pane=await good.getCurrentPane({tty:null,cwd:process.cwd()}); env[${JSON.stringify(inner.toUpperCase()+'_PANE_ID')}]='p999999999'; let rejected=false; try{await detect(env).getCurrentPane({tty:null,cwd:process.cwd()})}catch(e){rejected=/Cannot verify/.test(e.message)};fs.writeFileSync(${JSON.stringify(output)},JSON.stringify({name:good.name,pane,rejected,herdr:process.env.HERDR_PANE_ID,ikesu:process.env.IKESU_PANE_ID}));})().catch(e=>{fs.writeFileSync(${JSON.stringify(output)},JSON.stringify({error:e.stack}));process.exitCode=1});`);
      await contexts[inner].cli(['pane', 'run', innerPane.pane_id, shellQuote([process.execPath, probe])]);
      const result = await eventually(() => JSON.parse(fs.readFileSync(output, 'utf8')));
      assert.equal(result.name, inner, JSON.stringify(result));
      assert.ok(result.herdr && result.ikesu, 'both runtime environments must really coexist');
      assert.equal(result.pane.id, innerPane.pane_id);
      assert.equal(result.rejected, true);
      console.log('PASS nested', outer, '->', inner, JSON.stringify(result), dir);
    } finally {
      const pids = execFileSync('pgrep', ['-u', String(process.getuid()), '-f', dir], { encoding: 'utf8' }).trim().split('\n').map(Number);
      for (const pid of pids) {
        const info = spawnSync('ps', ['-p', String(pid), '-o', 'command='], { encoding: 'utf8' });
        if (info.status !== 0) continue;
        if (info.stdout.includes(dir) && /\/(herdr|ikesu) server\s*$/.test(info.stdout)) execFileSync('kill', [String(pid)]);
      }
      if (server.exitCode === null) await new Promise(resolve => server.once('exit', resolve));
      fs.closeSync(outerLog);
    }
  });
}

test('inherited variables without any runtime ancestor fail with an explicit error', async () => {
  const dir = fs.mkdtempSync('/tmp/tb-orphan-');
  const probe = path.join(dir, 'probe.cjs');
  const output = path.join(dir, 'result.json');
  const modulePath = path.resolve(__dirname, '../dist/index.js');
  fs.writeFileSync(probe, `const fs=require('fs'); const {currentMultiplexer}=require(${JSON.stringify(modulePath)});const deadline=Date.now()+5000;function run(){if(process.ppid!==1&&Date.now()<deadline){setTimeout(run,10);return;}try{currentMultiplexer({HERDR_PANE_ID:'p1',IKESU_PANE_ID:'p1'});fs.writeFileSync(${JSON.stringify(output)},'unexpected selection');}catch(e){fs.writeFileSync(${JSON.stringify(output)},e.message);}}run();`);
  await exec('/bin/sh', ['-c', shellQuote([process.execPath, probe]) + ' >/dev/null 2>&1 &']);
  const error = await eventually(() => fs.readFileSync(output, 'utf8'));
  assert.match(error, /Cannot establish the inner herdr\/ikesu runtime from process ancestry/);
  console.log('PASS no ancestor refuses inherited variables', error);
});
