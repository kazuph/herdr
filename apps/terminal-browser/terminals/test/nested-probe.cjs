const fs = require('node:fs');
const path = require('node:path');
const { pathToFileURL } = require('node:url');
const diagnostics = require('node:diagnostics_channel');
const appRoot = process.env.TB_TEST_APP_ROOT || path.resolve(__dirname, '../..');
const { detect, currentMultiplexer } = require(path.join(appRoot, 'terminals/dist/index.js'));
const [output, selected, orphan] = process.argv.slice(2);
const commands = [];
diagnostics.channel('child_process').subscribe(({ process: child }) => child.once('spawn', () => commands.push({ bin: child.spawnfile, args: child.spawnargs.slice(1) })));
async function run() {
  if (orphan && process.ppid !== 1) {
    setTimeout(run, 10);
    return;
  }
  const result = {};
  try {
    const { upgradeCommand, upgradeTarget } = await import(pathToFileURL(path.join(appRoot, 'cli/src/upgrade.ts')).href);
    if (orphan) {
      try { currentMultiplexer(process.env); } catch (e) { result.selectionError = e.message; }
      result.target = upgradeTarget();
      result.exit = await upgradeCommand();
    } else {
      const good = detect();
      result.name = good.name;
      result.pane = await good.getCurrentPane({ tty: null, cwd: process.cwd() });
      result.herdr = process.env.HERDR_PANE_ID;
      result.ikesu = process.env.IKESU_PANE_ID;
      result.target = upgradeTarget();
      result.exit = await upgradeCommand();
      process.env[`${selected.toUpperCase()}_PANE_ID`] = 'p999999999';
      try { await detect().getCurrentPane({ tty: null, cwd: process.cwd() }); } catch (e) { result.rejected = /Cannot verify/.test(e.message); }
      const installedBefore = commands.filter(c => c.args.join(' ') === 'tb install').length;
      try { await upgradeCommand(); } catch (e) { result.upgradeRejected = /Cannot verify/.test(e.message); }
      result.invalidPaneSpawnedInstall = commands.filter(c => c.args.join(' ') === 'tb install').length !== installedBefore;
    }
  } catch (e) { result.error = e.stack; }
  result.commands = commands;
  fs.writeFileSync(output, JSON.stringify(result));
}
run();
