const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { test } = require('node:test');
const { detect, shellIn } = require('../dist/index.js');
const { startIkesu, eventually } = require('./ikesu-harness.cjs');

test('real isolated ikesu: pane identity, all split arguments, attested config, and false values', async () => {
  const server = await startIkesu();
  const { dir, env, pane, binary: localBin, config } = server;
  const calls = [];
  const realRun = shellIn(env);
  const run = async (bin,args,input) => {calls.push([bin,...args]);return realRun(bin,args,input);};
  try {
    const terminal = detect(env,run);
    assert.deepEqual(await terminal.getCurrentPane({tty:null,cwd:dir}),{id:pane.pane_id,tab:pane.tab_id});
    const herdrConfig = path.join(dir,'.config/herdr/config.toml');
    fs.mkdirSync(path.dirname(herdrConfig),{recursive:true});fs.writeFileSync(herdrConfig,'onboarding = false\n');
    await terminal.prepare();
    assert.match(fs.readFileSync(config,'utf8'),/kitty_graphics = true/);
    assert.equal(fs.readFileSync(herdrConfig,'utf8'),'onboarding = false\n');
    for(const direction of ['right','left','down','up']) {
      const marker=path.join(dir,`ran-${direction}`),start=calls.length;
      await terminal.split({from:{id:pane.pane_id,tab:pane.tab_id},direction,command:[process.execPath,'-e',`require('fs').writeFileSync(${JSON.stringify(marker)},'ran')`],size:0.4,tty:null});
      await eventually(()=>assert.equal(fs.readFileSync(marker,'utf8'),'ran'));
      const commands=calls.slice(start);
      assert.deepEqual(commands[1],[localBin,'pane','split','--pane',pane.pane_id,'--direction',['up','down'].includes(direction)?'down':'right','--focus','--ratio','0.4']);
      assert.equal(commands.length,['left','up'].includes(direction)?4:3);
      console.log(direction,JSON.stringify(commands));
    }
    const before=fs.readFileSync(config,'utf8');
    const bad=detect({...env,IKESU_PANE_ID:'p999999999'});
    await assert.rejects(bad.getCurrentPane({tty:null,cwd:dir}),/Cannot verify ikesu pane/);
    await assert.rejects(bad.prepare(),/Cannot verify ikesu pane/);
    assert.equal(fs.readFileSync(config,'utf8'),before);
    fs.writeFileSync(config,'[experimental\n');
    await assert.rejects(terminal.prepare(),/configuration was not changed/);
    assert.equal(fs.readFileSync(config,'utf8'),'[experimental\n');
    console.log('PASS actual ikesu split and pane checks; config_path field supplied by compatibility proxy',dir);
  } finally {await server.close();}
});
