import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { pathToFileURL } from "node:url";

test("agent-browser receives only the bundled empty config", async () => {
  const distRoot = path.resolve("/tmp/terminal-browser-security-test");
  const socketDir = fs.mkdtempSync(path.join(os.tmpdir(), "terminal-browser-sockets-"));
  process.env.TERMINAL_BROWSER_DIST_ROOT = distRoot;
  const moduleUrl = pathToFileURL(path.resolve("dist/action.js"));
  moduleUrl.searchParams.set("test", String(Date.now()));
  const { agentBrowserConfigPath, agentBrowserEnvironment } = await import(moduleUrl.href);

  const expected = path.join(distRoot, "agent-browser", "config.json");
  assert.equal(agentBrowserConfigPath(), expected);
  assert.equal(agentBrowserEnvironment(socketDir).AGENT_BROWSER_CONFIG, expected);
});
