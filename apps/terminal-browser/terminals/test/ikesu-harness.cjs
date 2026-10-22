const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const net = require('node:net');
const { spawn, spawnSync, execFileSync, execFile } = require('node:child_process');
const { promisify } = require('node:util');
const exec = promisify(execFile);
async function eventually(check) {
  const end = Date.now() + 15000;
  let error;
  while (Date.now() < end) {
    try { return await check(); } catch (e) { error = e; }
    await new Promise(resolve => setTimeout(resolve, 50));
  }
  throw error;
}
function request(socket, body) {
  return new Promise((resolve,reject) => {
    const connection=net.connect(socket);
    connection.setEncoding('utf8');
    connection.setTimeout(5000,()=>connection.destroy(new Error('real server response timeout')));
    let buffer='';
    connection.on('error',reject);
    connection.on('data',chunk=>{buffer+=chunk;const end=buffer.indexOf('\n');if(end>=0){connection.end();try{resolve(JSON.parse(buffer.slice(0,end)));}catch(e){reject(e);}}});
    connection.write(JSON.stringify(body)+'\n');
  });
}
async function startIkesu(options={}) {
  assert.ok(process.env.IKESU_TB_TEST_BIN, 'real IKESU_TB_TEST_BIN required');
  const dir=fs.mkdtempSync('/tmp/tb-config-');
  const binary=path.join(dir,'ikesu');fs.symlinkSync(process.env.IKESU_TB_TEST_BIN,binary);
  const config=path.join(dir,'server.toml');
  const initial=options.initial ?? '[experimental]\nallow_nested = true\nkitty_graphics = false\n';
  fs.writeFileSync(config,initial);
  const base=Object.fromEntries(Object.entries(process.env).filter(([key])=>!/^(HERDR_|IKESU_|GTX_|GOTERRACE_|XDG_)/.test(key)));
  const realSocket=path.join(dir,'state/default/ikesu.sock');
  const env={...base,HOME:dir,IKESU_STATE_DIR:path.join(dir,'state'),IKESU_DB_PATH:path.join(dir,'db.sqlite'),IKESU_SOCKET_PATH:realSocket,IKESU_CONFIG:config,SHELL:'/bin/sh'};
  const log=fs.openSync(path.join(dir,'server.log'),'w');
  const server=spawn(binary,['server'],{env,stdio:['ignore',log,log]});
  const cli=async args=>{const {stdout}=await exec(binary,args,{env});return stdout.trim()?JSON.parse(stdout):null;};
  const connections=new Set();let reloads=0;
  const proxy=net.createServer(connection=>{
    connections.add(connection);connection.on('close',()=>connections.delete(connection));connection.setEncoding('utf8');let buffer='';
    connection.on('data',async chunk=>{buffer+=chunk;const end=buffer.indexOf('\n');if(end<0)return;const body=JSON.parse(buffer.slice(0,end));buffer='';
      try {
        const reply=await request(realSocket,body);
        if(body.method==='server.reload_config') {
          reloads++;
          if(options.advertise !== false && reply.result) reply.result.config_path=config;
          if(options.onReload) await options.onReload({count:reloads,config,reply});
          if(options.dropReload===reloads){connection.destroy();return;}
        }
        connection.end(JSON.stringify(reply)+'\n');
      } catch(e){connection.destroy(e);}
    });
    connection.on('error',()=>{});
  });
  try {
    await eventually(()=>cli(['pane','list']));
    const {result:{root_pane:pane}}=await cli(['workspace','create','--cwd',dir]);
    const socket=path.join(dir,'proxy.sock');await new Promise(resolve=>proxy.listen(socket,resolve));
    const clientEnv={...env,IKESU_SOCKET_PATH:socket,IKESU_BIN_PATH:binary,IKESU_PANE_ID:pane.pane_id,IKESU_TAB_ID:pane.tab_id,IKESU_CONFIG_PATH:config};
    return {dir,binary,config,initial,env:clientEnv,cli,pane,reloads:()=>reloads,
      rpc:(method,params={pane_id:pane.pane_id})=>request(realSocket,{id:'tb-config-test',method,params}),
      async close(){
        for(const connection of connections)connection.destroy();await new Promise(resolve=>proxy.close(resolve));
        if(server.exitCode===null){const pids=execFileSync('pgrep',['-u',String(process.getuid()),'-f',dir],{encoding:'utf8'}).trim().split('\n');assert.ok(pids.includes(String(server.pid)));execFileSync('kill',[String(server.pid)]);await new Promise(resolve=>server.once('exit',resolve));}
        fs.closeSync(log);
      }
    };
  } catch(error) {
    if(server.exitCode===null){const pids=spawnSync('pgrep',['-u',String(process.getuid()),'-f',dir],{encoding:'utf8'});if(pids.stdout.split('\n').includes(String(server.pid)))execFileSync('kill',[String(server.pid)]);}
    fs.closeSync(log);throw error;
  }
}
module.exports={startIkesu,eventually,exec};
