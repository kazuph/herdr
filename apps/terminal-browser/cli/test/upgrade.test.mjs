import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";

const moduleUrl = new URL("../src/upgrade.ts", import.meta.url).href;
const source = `import { upgradeCommand } from ${JSON.stringify(moduleUrl)}; process.exitCode = await upgradeCommand();`;

test("upgrade reports a missing herdr without calling upstream or closing browsers", () => {
  const empty = fs.mkdtempSync(path.join(os.tmpdir(), "tb-no-herdr-"));
  try {
    const result = spawnSync(process.execPath, ["--experimental-strip-types", "--input-type=module", "-e", source], { env: { ...process.env, PATH: empty }, encoding: "utf8" });
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /herdr tb install could not start/);
  } finally { fs.rmSync(empty, { recursive: true }); }
});

// This optional integration test is registered only in the explicit remote
// check command that supplies the newly built real Herdr, never a fake CLI.
if (process.env.HERDR_TB_TEST_REAL_BIN) {
  test("upgrade invokes the real herdr tb install command", () => {
    const result = spawnSync(process.execPath, ["--experimental-strip-types", "--input-type=module", "-e", source], { env: { ...process.env, PATH: path.dirname(process.env.HERDR_TB_TEST_REAL_BIN) }, encoding: "utf8" });
    assert.equal(result.status, 1);
    assert.match(result.stderr, /tb installation supports macOS Apple silicon/);
  });
}

const { upgradeTarget } = await import(moduleUrl);
for (const runtime of ['herdr', 'ikesu', 'gtx']) {
  test(`upgrade targets ${runtime} and prefers its BIN_PATH`, () => {
    const env = { [`${runtime.toUpperCase()}_PANE_ID`]: 'p1' };
    assert.deepEqual(upgradeTarget(env), { runtime, bin: runtime, args: ["tb", "install"] });
    env[`${runtime.toUpperCase()}_BIN_PATH`] = `/installed/${runtime}`;
    assert.deepEqual(upgradeTarget(env), { runtime, bin: `/installed/${runtime}`, args: ["tb", "install"] });
  });
}
test('upgrade defaults to herdr only without a runtime', () => {
  assert.deepEqual(upgradeTarget({}), { runtime: 'herdr', bin: 'herdr', args: ["tb", "install"] });
});
