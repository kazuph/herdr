// Real isolated server, watch processes, answer commands and production handoff.
// Usage: node test/offline-answer-handoff.mjs RELEASES HERDR OUT
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import readline from 'node:readline';
import {spawn, fork, execFileSync} from 'node:child_process';
import {pathToFileURL} from 'node:url';
import {waitUntil} from './fakes.mjs';

if (process.argv[2] === '--worker') {
  const root = process.argv[3];
  const {main, MacDialogDecisionsDaemon} = await import(pathToFileURL(path.join(root,'mac-dialog-decisions.mjs')));
  const messages = [];
  process.on('message', message => messages.push(message));
  const receive = async type => {await waitUntil(()=>messages.some(m=>m.type===type),{timeout:15000});return messages.splice(messages.findIndex(m=>m.type===type),1)[0];};
  let daemon;
  process.argv[2] = 'run';
  await main({createDaemon: options => {
    const answer = options.runAnswer;
    let watchCount=0;
    daemon = new MacDialogDecisionsDaemon({...options,
      spawnWatch: async () => {
        if(watchCount++ > 0) await receive('reconnect');
        return options.spawnWatch();
      },
      runAnswer: async argv => {
        await receive('send-answer');
        const result = await answer(argv);
        process.send({type:'answer-result',result});
        // Reconnect a real watch and inspect its pending snapshot before accepting
        // the command result. No resolved event is supplied to this daemon.
        await receive('accept-result');
        return result;
      },
      onStateChange: () => {
        process.send({type:'state',entries:daemon.entries.size});
        options.onStateChange();
      },
    });
    return daemon;
  }});
  await waitUntil(()=>daemon.entries.size===1,{timeout:15000});
  const entry = [...daemon.entries.values()][0];
  const sending = daemon.submitAnswer(entry,{optionIds:['yes'],text:''});
  daemon._watchHandle.close(); // closes the actual herdr watch child process
  process.send({type:'disconnected',id:entry.decision.decision_id,pending:entry.answerPending});
  await sending;
} else {
  const [releases, herdr, out] = process.argv.slice(2).map(p=>path.resolve(p));
  fs.mkdirSync(out,{recursive:true});
  const tmp=fs.mkdtempSync('/tmp/md-offline-');
  const children=[];const logs=[];const handoffPids=new Set();
  const env={...process.env,HOME:tmp,XDG_CONFIG_HOME:path.join(tmp,'c'),XDG_STATE_HOME:path.join(tmp,'st'),HERDR_SOCKET_PATH:path.join(tmp,'s'),HERDR_CLIENT_SOCKET_PATH:path.join(tmp,'cl'),HERDR_SOCKET_PATH_EXPLICIT:'1',HERDR_BIN_PATH:herdr};
  delete env.HERDR_PANE_ID;delete env.HERDR_SESSION;
  const run=(args,extra={})=>new Promise(resolve=>{
    const c=spawn(herdr,args,{env:{...env,...extra},stdio:['ignore','pipe','pipe']});children.push(c);
    let stdout='',stderr='';c.stdout.on('data',v=>stdout+=v);c.stderr.on('data',v=>stderr+=v);
    c.on('exit',code=>{logs.push({args,code,stdout,stderr});resolve({code,stdout,stderr});});
  });
  try {
    const server=spawn(herdr,['server'],{env,stdio:['ignore','ignore','pipe']});children.push(server);server.stderr.on('data',v=>logs.push(String(v)));
    await waitUntil(()=>fs.existsSync(env.HERDR_SOCKET_PATH));
    for (const alreadyResolved of [false,true]) {
      const stateDir=path.join(tmp,alreadyResolved?'already':'success');fs.mkdirSync(stateDir);
      const workerEnv={...env,MAC_DIALOG_STATE_DIR:stateDir,HERDR_PLUGIN_STATE_DIR:stateDir};
      const oldRoot=path.join(releases,'0.1.0/mac-dialog-decisions');const nextRoot=path.join(releases,'0.1.1/mac-dialog-decisions');
      assert.equal((await run(['plugin','link',oldRoot])).code,0);
      const worker=fork(new URL(import.meta.url),['--worker',oldRoot,path.join(oldRoot,'mac-dialog-decisions.mjs')],{env:workerEnv,stdio:['ignore','pipe','pipe','ipc']});children.push(worker);
      const messages=[];worker.on('message',m=>{messages.push(m);logs.push(m);});worker.stdout.on('data',v=>logs.push(String(v)));worker.stderr.on('data',v=>logs.push(String(v)));
      const readMeta=()=>{try{return JSON.parse(fs.readFileSync(path.join(stateDir,'daemon.json')));}catch{return {};}};
      await waitUntil(()=>readMeta().version==='0.1.0');
      const ask=run(['decision','ask','--kind','ask','--title',`通知欠落 ${alreadyResolved}`,'--option','yes:はい','--timeout','120']);
      await waitUntil(()=>messages.some(m=>m.type==='disconnected'),{timeout:15000});
      const disconnected=messages.find(m=>m.type==='disconnected');assert.equal(disconnected.pending,true);
      assert.equal((await run(['plugin','link',nextRoot])).code,0);
      const ensure=spawn(process.execPath,[path.join(nextRoot,'mac-dialog-decisions.mjs'),'ensure'],{env:workerEnv,stdio:['ignore','ignore','pipe']});children.push(ensure);ensure.stderr.on('data',v=>logs.push(String(v)));
      assert.equal(await new Promise(resolve=>ensure.on('exit',resolve)),0);
      assert.ok(fs.existsSync(path.join(stateDir,'restart-request.json')));
      assert.equal(readMeta().version,'0.1.0','unconfirmed answer keeps old version');
      if(alreadyResolved) assert.equal((await run(['decision','answer',disconnected.id,'yes','--responder','other'])).code,0);
      worker.send({type:'send-answer'});
      await waitUntil(()=>messages.some(m=>m.type==='answer-result'));
      const result=messages.find(m=>m.type==='answer-result').result;
      if(alreadyResolved) {assert.notEqual(result.code,0);assert.match(result.stdout+result.stderr,/decision_already_resolved/);} else assert.equal(result.code,0);
      await ask;
      // A new real watch only replays pending decisions; no historical resolution.
      const reconnect=spawn(herdr,['decision','watch','--all-machines'],{env,stdio:['ignore','pipe','pipe']});children.push(reconnect);reconnect.stderr.on('data',v=>logs.push(String(v)));
      const lines=[];const rl=readline.createInterface({input:reconnect.stdout});rl.on('line',line=>{if(line.trim()) lines.push(JSON.parse(line));});
      await waitUntil(()=>lines.some(line=>line.event==='machine_up'));
      const list=await run(['decision','list','--status','pending']);
      assert.deepEqual(JSON.parse(list.stdout).result.decisions,[]);
      assert.equal(lines.some(line=>line.decision?.decision_id===disconnected.id),false);
      reconnect.kill('SIGTERM');rl.close();
      worker.send({type:'reconnect'});
      worker.send({type:'accept-result'});
      await waitUntil(()=>messages.slice(messages.findIndex(m=>m.type==='answer-result')+1).some(m=>m.type==='state' && m.entries===0));
      await waitUntil(()=>readMeta().version==='0.1.1' && readMeta().pid!==worker.pid && readMeta().pending_entries===0,{timeout:15000});
      handoffPids.add(readMeta().pid);
      assert.equal(readMeta().pending_entries,0);
      logs.push({PASS:alreadyResolved?'別窓口で先に解決→already_resolved→回収→新版切替':'watch切断→回答成功→pending再購読に解決済みなし→回収→新版切替',reconnected:lines});
      process.kill(readMeta().pid,'SIGTERM');
      await waitUntil(()=>{try{process.kill(readMeta().pid,0);return false;}catch{return true;}});
      handoffPids.delete(readMeta().pid);
    }
    fs.writeFileSync(path.join(out,'summary.json'),JSON.stringify({ok:true,logs},null,2));
    console.log('PASS real offline answer success/already_resolved and reserved version handoff');
  } finally {
    // Event hooks may also have started an isolated plugin job. Its metadata
    // lives under this test's private HOME; never discover or stop global jobs.
    for(const file of fs.readdirSync(tmp,{recursive:true}).filter(file=>file.endsWith('daemon.json'))) {
      try {
        const record=JSON.parse(fs.readFileSync(path.join(tmp,file)));
        if(!record.plugin_root?.startsWith(releases+path.sep)) continue;
        const command=execFileSync('ps',['-o','command=','-p',String(record.pid)],{encoding:'utf8'});
        if(command.includes(path.join(record.plugin_root,'mac-dialog-decisions.mjs'))) handoffPids.add(record.pid);
      } catch {}
    }
    for(const pid of handoffPids) {try{process.kill(pid,'SIGTERM');}catch{}}
    await waitUntil(()=>[...handoffPids].every(pid=>{try{process.kill(pid,0);return false;}catch{return true;}}));
    await Promise.all(children.filter(c=>c.exitCode===null && c.signalCode===null).map(c=>new Promise(resolve=>{c.once('exit',resolve);c.kill('SIGTERM');})));
    fs.writeFileSync(path.join(out,'integration.log'),JSON.stringify(logs,null,2));
    fs.rmSync(tmp,{recursive:true,force:true});
  }
}
