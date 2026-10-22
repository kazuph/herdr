const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { test } = require('node:test');
const { startIkesu, exec } = require('./ikesu-harness.cjs');
const root=process.env.TB_TEST_APP_ROOT || path.resolve(__dirname,'../..');
const { detect }=require(path.join(root,'terminals/dist/index.js'));

async function withServer(options, check) {const server=await startIkesu(options);try{await check(server);}finally{await server.close();}}

test('server-owned config path: real server enables graphics only in the attested file',()=>withServer({initial:' [experimental] # features\nallow_nested = true\nkitty_graphics = false # disabled\n'},async s=>{
  assert.equal((await s.rpc('pane.graphics.info')).error?.code,'feature_disabled');
  const alias=path.join(s.dir,'config-link.toml');fs.symlinkSync(s.config,alias);
  await detect({...s.env,IKESU_CONFIG_PATH:alias}).prepare();
  const state=await s.rpc('pane.graphics.info');
  assert.equal(state.error?.code,'cell_size_unavailable',JSON.stringify(state));
  assert.match(state.error.message,/exact terminal cell pixels/);
  assert.equal(fs.lstatSync(alias).isSymbolicLink(),true);
  console.log('PASS response-path compatibility test with real 0.4.0 reload; native 0.4.1 attestation remains NOT_RUN',s.dir);
}));

for(const mode of ['missing-env','relative-env','mismatch','missing-response'])test(`server-owned config path: ${mode} makes no edits`,()=>withServer({advertise:mode!=='missing-response'},async s=>{
  const other=path.join(s.dir,'other.toml');fs.writeFileSync(other,s.initial);
  const env={...s.env};
  if(mode==='missing-env')delete env.IKESU_CONFIG_PATH;
  if(mode==='relative-env')env.IKESU_CONFIG_PATH='server.toml';
  if(mode==='mismatch')env.IKESU_CONFIG_PATH=other;
  const previous = process.cwd();
  try {
    if(mode==='relative-env')process.chdir(s.dir);
    await assert.rejects(detect(env).prepare(),/ikesu 0\.4\.1|config.*path/i);
  } finally {process.chdir(previous);}
  assert.equal(fs.readFileSync(s.config,'utf8'),s.initial);
  assert.equal(fs.readFileSync(other,'utf8'),s.initial);
  assert.equal((await s.rpc('pane.graphics.info')).error?.code,'feature_disabled');
  assert.equal(fs.existsSync(path.join(s.dir,'.config/ikesu/config.toml')),false);
}));

for(const existed of [true,false])for(const concurrent of [true,false])test(`conditional rollback: ${existed?'existing':'new'} config, ${concurrent?'preserve concurrent edit':'restore own edit'} after real reload response loss`,async()=>{
  let other;
  await withServer({dropReload:2,onReload:({count,config})=>{if(count===2&&concurrent){other='[experimental]\nkitty_graphics = true\n[ui]\nsidebar_width = 31\n';fs.writeFileSync(config,other);}}},async s=>{
    if(!existed)fs.unlinkSync(s.config);
    await assert.rejects(detect(s.env).prepare(),concurrent?/changed|concurrent|conflict/i:undefined);
    assert.equal(s.reloads(),2,'preflight and real post-write reload were both reached');
    if(concurrent)assert.equal(fs.readFileSync(s.config,'utf8'),other);
    else if(existed)assert.equal(fs.readFileSync(s.config,'utf8'),s.initial);
    else assert.equal(fs.existsSync(s.config),false);
    await s.cli(['server','reload-config']);
  });
});

for(const existed of [true,false])test(`atomic file replacement: real write failure leaves ${existed?'existing bytes':'missing file'} intact`,()=>withServer({},async s=>{
  if(!existed)fs.unlinkSync(s.config);
  const probe=path.join(s.dir,'write-failure.cjs');
  fs.writeFileSync(probe,`process.on('SIGXFSZ',()=>{});const {detect}=require(${JSON.stringify(path.join(root,'terminals/dist/index.js'))});detect().prepare().then(()=>console.log(JSON.stringify({unexpected:true}))).catch(e=>console.log(JSON.stringify({error:e.message})));`);
  const result=await exec('/bin/sh',['-c',`ulimit -f 0; exec '${process.execPath}' '${probe}'`],{env:s.env});
  const receipt=JSON.parse(result.stdout.trim());
  assert.match(receipt.error,/EFBIG|file too large/i);
  if(existed)assert.equal(fs.readFileSync(s.config,'utf8'),s.initial);
  else assert.equal(fs.existsSync(s.config),false);
  assert.equal(fs.readdirSync(s.dir).filter(name=>name.includes('.tb-')).length,0);
}));
