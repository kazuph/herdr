// Explicit integration command: node test/distribution-local.mjs RELEASES HERDR OUT.
// Uses real Developer ID apps, a real isolated server and both real watch streams.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import readline from 'node:readline';
import { spawn, execFileSync } from 'node:child_process';
import { MacDialogDecisionsDaemon, ensureNativeDialog, spawnHelperDialog, createRunAnswer } from '../mac-dialog-decisions.mjs';
import { waitUntil } from './fakes.mjs';
const [releases, herdr, out] = process.argv.slice(2).map(p => path.resolve(p));
fs.mkdirSync(out, {recursive:true});
const tmp = fs.mkdtempSync('/tmp/md-dist-');
const logs = [];
const children = [];
const daemons = [];
const isolatedPids = new Set();
const env = {...process.env, HOME: tmp, XDG_CONFIG_HOME:path.join(tmp,'c'), HERDR_SOCKET_PATH:path.join(tmp,'s'), HERDR_SOCKET_PATH_EXPLICIT:'1', HERDR_CLIENT_SOCKET_PATH:path.join(tmp,'client'), HERDR_BIN_PATH:herdr, MAC_DIALOG_STATE_DIR:path.join(tmp,'handoff')};
delete env.HERDR_PANE_ID;
delete env.HERDR_SESSION;
Object.assign(process.env, env);
delete process.env.HERDR_PANE_ID;
delete process.env.HERDR_SESSION;
const run = args => new Promise(resolve => {
 const c=spawn(herdr,args,{env,stdio:['ignore','pipe','pipe']}); children.push(c);
 let stdout='',stderr='';c.stdout.on('data',v=>stdout+=v);c.stderr.on('data',v=>stderr+=v);c.on('exit',code=>{logs.push(JSON.stringify({args,code,stdout,stderr}));resolve({code,stdout,stderr});});
});
function watch() {
 const c=spawn(herdr,['decision','watch','--all-machines'],{env,stdio:['ignore','pipe','pipe']}); children.push(c);
 const rl=readline.createInterface({input:c.stdout});c.stderr.on('data',v=>logs.push(String(v)));
 return {lines:(async function*(){ for await(const line of rl) if(line.trim()) yield JSON.parse(line); })(), close(){c.kill('SIGTERM');rl.close();}};
}
try {
 const server=spawn(herdr,['server'],{env,stdio:['ignore','pipe','pipe']});children.push(server);server.stderr.on('data',v=>logs.push(String(v)));server.stdout.resume();
 await waitUntil(()=>fs.existsSync(env.HERDR_SOCKET_PATH));
 const root=path.join(releases,'0.1.0/mac-dialog-decisions');
 const native=ensureNativeDialog({stateDir:tmp},m=>logs.push(m),root);
 assert.ok(native,'real expected Developer ID app');
 const damaged=path.join(tmp,'damaged');fs.cpSync(root,damaged,{recursive:true});
 fs.rmSync(path.join(damaged,'herdr-decision-dialog.app'),{recursive:true});
 assert.equal(ensureNativeDialog({stateDir:tmp},m=>logs.push(m),damaged),null,'missing bundle never compiles');
 fs.cpSync(path.join(root,'herdr-decision-dialog.app'),path.join(damaged,'herdr-decision-dialog.app'),{recursive:true});
 fs.appendFileSync(path.join(damaged,'herdr-decision-dialog.app/Contents/MacOS/herdr-decision-dialog'),'tampered');
 assert.equal(ensureNativeDialog({stateDir:tmp},m=>logs.push(m),damaged),null,'tampered bundle rejected before launch');
 for (const side of ['parent','child']) {
   const stateDir=path.join(tmp,side);fs.mkdirSync(stateDir);
   const config={stateDir,herdrBin:herdr,helperCmd:[native]};
   const d=new MacDialogDecisionsDaemon({config,spawnWatch:async()=>watch(),runAnswer:createRunAnswer(config,m=>logs.push(m)),spawnDialog:()=>spawnHelperDialog(config,m=>logs.push(m)),specFilePath:key=>path.join(stateDir,`${key.replaceAll(' ','_')}.json`),log:m=>logs.push(`${side}: ${m}`)});
   daemons.push(d);await d.start();
 }
 const pending=async()=>JSON.parse((await run(['decision','list','--status','pending'])).stdout).result.decisions;
 for (const side of [0,1]) {
   const ask=run(['decision','ask','--kind','ask','--title',`双方向 ${side}`,'--option','yes:はい','--option','no:いいえ','--timeout','120']);
   await waitUntil(()=>daemons.every(d=>d.showing.size===1));
   const id=(await pending())[0].decision_id;
   const argv=['decision','answer',id,'yes','--machine','local','--responder','mac-dialog'];
   const result=await daemons[side].runAnswer(argv);
   assert.equal(result.code,0);await ask;
   await waitUntil(()=>daemons.every(d=>d.showing.size===0 && d.entries.size===0));
   logs.push(`PASS ${side===0?'親から子':'子から親'}の解決通知で双方を閉鎖`);
 }
 const ask=run(['decision','ask','--kind','ask','--title','同時回答','--option','yes:はい','--option','no:いいえ','--timeout','120']);
 await waitUntil(()=>daemons.every(d=>d.showing.size===1));const id=(await pending())[0].decision_id;
 const results=await Promise.all(['yes','no'].map(option=>run(['decision','answer',id,option,'--machine','local','--responder','mac-dialog'])));
 assert.equal(results.filter(r=>r.code===0).length,1,'exactly one simultaneous answer wins');
 await ask;await waitUntil(()=>daemons.every(d=>d.showing.size===0 && d.entries.size===0));
 logs.push('PASS 実サーバーの同時回答は1件だけ確定し双方閉鎖');

 // Exercise the real new-version handoff with a pending decision first.
 for (const d of daemons) await d.stop();
 const stateDir=path.join(tmp,'handoff');fs.mkdirSync(stateDir);
 const daemonEnv={...env,MAC_DIALOG_STATE_DIR:stateDir,HERDR_PLUGIN_STATE_DIR:stateDir};
 const startNode=(root,action,extra={})=>{
   const child=spawn(process.execPath,[path.join(root,'mac-dialog-decisions.mjs'),action],{env:{...daemonEnv,...extra},stdio:['ignore','pipe','pipe']});children.push(child);
   let stdout='',stderr='';child.stdout.on('data',v=>stdout+=v);child.stderr.on('data',v=>{stderr+=v;logs.push(String(v));});
   return {child,done:new Promise(resolve=>child.on('exit',code=>resolve({code,stdout,stderr})))};
 };
 const metadata=()=>{try{return JSON.parse(fs.readFileSync(path.join(stateDir,'daemon.json'),'utf8'));}catch{return {};}};
 assert.equal((await run(['plugin','link',root])).code,0);
 const first=startNode(root,'run');
 await waitUntil(()=>metadata().version==='0.1.0');
 const handoffAsk=run(['decision','ask','--kind','ask','--title','切替待ちの質問','--option','yes:はい','--timeout','120']);
 await waitUntil(()=>metadata().open_windows===1);
 const nextRoot=path.join(releases,'0.1.1/mac-dialog-decisions');
 assert.equal((await run(['plugin','link',nextRoot])).code,0);
 assert.equal((await startNode(nextRoot,'ensure').done).code,0);
 assert.equal(metadata().version,'0.1.0');assert.ok(fs.existsSync(path.join(stateDir,'restart-request.json')));
 // Re-register the still-running old version before resolving its question.
 assert.equal((await run(['plugin','link',root])).code,0);
 assert.equal((await startNode(root,'ensure').done).code,0);
 assert.equal(fs.existsSync(path.join(stateDir,'restart-request.json')),false);
 const handoffId=(await pending())[0].decision_id;
 assert.equal((await run(['decision','answer',handoffId,'yes','--responder','mac-dialog'])).code,0);await handoffAsk;
 await waitUntil(()=>metadata().open_windows===0);
 assert.equal(metadata().pid,first.child.pid);
 assert.equal(metadata().version,'0.1.0');
 const rolledBack=JSON.parse((await startNode(root,'status').done).stdout);
 assert.equal(rolledBack.restart_requested,false);
 assert.equal(first.child.exitCode,null);
 logs.push('PASS 版1 pending→版2登録→版1登録へ戻す→解決、版1を保持し切替待ちなし');
 const staleAsk=run(['decision','ask','--kind','ask','--title','登録先の再確認','--option','yes:はい','--timeout','120']);
 await waitUntil(()=>metadata().open_windows===1);
 assert.equal((await run(['plugin','link',nextRoot])).code,0);
 assert.equal((await startNode(nextRoot,'ensure').done).code,0);
 assert.equal((await run(['plugin','link',root])).code,0);
 const staleId=(await pending())[0].decision_id;
 assert.equal((await run(['decision','answer',staleId,'yes','--responder','mac-dialog'])).code,0);
 await staleAsk;
 await waitUntil(()=>!fs.existsSync(path.join(stateDir,'restart-request.json')));
 assert.equal(metadata().pid,first.child.pid);
 assert.equal(metadata().version,'0.1.0');
 logs.push('PASS ensure を呼ばず登録先だけ戻しても、切替直前に不一致を検出して旧版を保持');
 const upgradeAsk=run(['decision','ask','--kind','ask','--title','再度の新版切替','--option','yes:はい','--timeout','120']);
 await waitUntil(()=>metadata().open_windows===1);
 assert.equal((await run(['plugin','link',nextRoot])).code,0);
 assert.equal((await startNode(nextRoot,'ensure').done).code,0);
 const upgradeId=(await pending())[0].decision_id;
 assert.equal((await run(['decision','answer',upgradeId,'yes','--responder','mac-dialog'])).code,0);
 await upgradeAsk;
 await waitUntil(()=>metadata().version==='0.1.1' && metadata().pid!==first.child.pid && metadata().open_windows===0,{timeout:15000});
 const silentJobs=JSON.parse((await run(['run','list'])).stdout).jobs.filter(job=>job.label==='mac-dialog-decisions');
 assert.ok(silentJobs.length>0);
 assert.ok(silentJobs.every(job=>job.completion==='none' && job.caller_pane==='' && job.caller_agent===''));
 logs.push('PASS ensure-daemon の実ジョブは caller pane/agent が空で通知なし');
 isolatedPids.add(metadata().pid);
 assert.equal(metadata().open_windows,0);
 assert.equal((await first.done).code,0);
 logs.push('PASS 判断待ちが残る間は旧版を保ち、解決後だけ新版へ起動し直す');
 // Only this isolated handoff daemon is terminated, never the real plugin job.
 process.kill(metadata().pid,'SIGTERM');
 await waitUntil(()=>{try{process.kill(metadata().pid,0);return false;}catch{return true;}});
 const legacyRoot=path.join(tmp,'legacy');fs.mkdirSync(legacyRoot);
 fs.writeFileSync(path.join(legacyRoot,'mac-dialog-decisions.mjs'),execFileSync('git',['show','de8f4bc6:apps/mac-dialog-decisions/mac-dialog-decisions.mjs']));
 const legacy=startNode(legacyRoot,'run',{MAC_DIALOG_HELPER_CMD:JSON.stringify([native])});
 await waitUntil(()=>metadata().pid===legacy.child.pid);
 assert.equal(metadata().version,undefined);
 assert.equal((await startNode(nextRoot,'ensure').done).code,0);
 const status=JSON.parse((await startNode(nextRoot,'status').done).stdout);
 assert.equal(status.running,true);assert.equal(status.version,undefined);
 assert.equal(status.restart_requested,true);assert.equal(legacy.child.exitCode,null);
 logs.push('PASS 旧 de8f4bc6 は稼働版不明・切替待ちのまま保持し、強制停止しない');
 legacy.child.kill('SIGTERM');await legacy.done;
 fs.writeFileSync(path.join(out,'summary.json'),JSON.stringify({ok:true,checks:['real signed bundled app','missing app fails without swiftc','tampered app fails','parent answer closes both','child answer closes both','simultaneous answer wins once','pending state defers handoff; empty state restarts new version','real legacy daemon remains running with unknown version and pending switch','rollback clears stale handoff and keeps old daemon','headless ensure starts through callerless completion none','handoff rechecks actual registration before switching'],logs},null,2));
 console.log('PASS signed app and two real responders; evidence: '+out);
} finally {
 for(const d of daemons) await d.stop();
 for(const pid of isolatedPids) { try { process.kill(pid,'SIGTERM'); } catch {} }
 for(const c of children) c.kill('SIGTERM');
 fs.writeFileSync(path.join(out,'integration.log'),logs.join('\n'));
 fs.rmSync(tmp,{recursive:true,force:true});
}
