// e2e: real herdr server (isolated session) + real daemon + fake dialog helper.
//
//   node test/e2e.mjs [--out <dir>]
//
// Spawns `herdr server` against throwaway XDG dirs, runs mac-dialog-decisions.mjs
// with the dialog helper swapped for a scripted fake (MAC_DIALOG_HELPER_CMD)
// that polls a plan file for what to "press", and asserts:
//   - two pending decisions show one dialog, with the queue count on it
//   - answering via the dialog resolves the decision with responder mac-dialog
//   - the next queued decision opens right after
//   - a free-text (allow_text) answer carries the text
//   - resolving via `herdr decision answer` kills the open dialog child

import { execFile, spawn } from "node:child_process";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { waitUntil } from "./fakes.mjs";

const APP_ROOT = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const REPO_ROOT = path.resolve(APP_ROOT, "..", "..");
const DEBUG_HERDR = path.join(REPO_ROOT, "target", "debug", "herdr");
// Prefer a freshly built binary; fall back to the installed herdr.
const HERDR =
  process.env.HERDR_BIN ||
  (fs.existsSync(DEBUG_HERDR) ? DEBUG_HERDR : "herdr");
const DAEMON = path.join(APP_ROOT, "mac-dialog-decisions.mjs");
const FAKE_DIALOG = path.join(APP_ROOT, "test", "fake-dialog-cmd.mjs");

const outIndex = process.argv.indexOf("--out");
const OUT =
  outIndex >= 0
    ? process.argv[outIndex + 1]
    : path.join(REPO_ROOT, ".artifacts", "feat-mac-dialog-decisions", "e2e");
fs.mkdirSync(OUT, { recursive: true });

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "mac-dialog-e2e-"));
const configHome = path.join(tmp, "config");
const runtimeDir = path.join(tmp, "runtime");
const stateDir = path.join(tmp, "state");
const fakeDir = path.join(tmp, "fake-dialog");
const planPath = path.join(tmp, "plan.json");
const socketPath = path.join(runtimeDir, "herdr.sock");
fs.mkdirSync(path.join(configHome, "herdr"), { recursive: true });
fs.mkdirSync(runtimeDir, { recursive: true });
fs.mkdirSync(stateDir, { recursive: true });
fs.mkdirSync(fakeDir, { recursive: true });
fs.writeFileSync(path.join(configHome, "herdr", "config.toml"), "onboarding = false\n");

const herdrEnv = {
  XDG_CONFIG_HOME: configHome,
  XDG_RUNTIME_DIR: runtimeDir,
  HERDR_SOCKET_PATH: socketPath,
  HERDR_SOCKET_PATH_EXPLICIT: "1",
  SHELL: "/bin/sh",
};

const serverLog = fs.createWriteStream(path.join(OUT, "server.log"));
const daemonLog = fs.createWriteStream(path.join(OUT, "daemon.log"));

function spawnLogged(argv, env, out) {
  const child = spawn(argv[0], argv.slice(1), {
    env: { ...process.env, ...env },
    stdio: ["ignore", "pipe", "pipe"],
  });
  child.stdout.on("data", (c) => out.write(c));
  child.stderr.on("data", (c) => out.write(c));
  return child;
}

function herdr(args) {
  return new Promise((resolve) => {
    execFile(
      HERDR,
      args,
      { env: { ...process.env, ...herdrEnv }, encoding: "utf8", timeout: 15_000 },
      (error, stdout, stderr) => {
        resolve({
          code: error?.code ?? 0,
          stdout: String(stdout),
          stderr: String(stderr),
        });
      },
    );
  });
}

function spawnAsk(args) {
  const child = execFile(
    HERDR,
    ["decision", "ask", ...args],
    { env: { ...process.env, ...herdrEnv }, encoding: "utf8" },
  );
  let stdout = "";
  let stderr = "";
  child.stdout.on("data", (c) => (stdout += c));
  child.stderr.on("data", (c) => (stderr += c));
  const done = new Promise((resolve) =>
    child.on("exit", (code) => resolve({ code, stdout, stderr })),
  );
  return { child, done };
}

function setPlan(plan) {
  fs.writeFileSync(planPath, JSON.stringify(plan));
}

function specFiles() {
  const file = path.join(fakeDir, "specs.jsonl");
  try {
    return fs
      .readFileSync(file, "utf8")
      .split("\n")
      .filter(Boolean)
      .map((l) => JSON.parse(l));
  } catch {
    return [];
  }
}

function aliveDialogs() {
  return fs.readdirSync(fakeDir).filter((f) => f.startsWith("alive-")).length;
}

function specDecisionId(spec) {
  return /判断待ちID: (\S+)/.exec(spec.text)?.[1] || "";
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

const cleanup = [];
function finish(code) {
  for (const fn of cleanup.reverse()) {
    try {
      fn();
    } catch {}
  }
  process.exit(code);
}

async function main() {
  const evidence = { steps: [] };
  const step = (name, ok, detail) => {
    evidence.steps.push({ name, ok, detail });
    process.stdout.write(
      `${ok ? "PASS" : "FAIL"} ${name}${detail ? ` — ${detail}` : ""}\n`,
    );
  };
  setPlan({});

  // 1. isolated herdr server
  const server = spawnLogged([HERDR, "server"], herdrEnv, serverLog);
  cleanup.push(() => server.kill("SIGTERM"));
  await waitUntil(
    () =>
      new Promise((resolve) => {
        const sock = net.connect(socketPath);
        sock.once("connect", () => {
          sock.destroy();
          resolve(true);
        });
        sock.once("error", () => resolve(false));
      }),
  );
  step("isolated herdr server up", true, socketPath);

  // 2. two decisions pending before the daemon ever starts
  const askA = spawnAsk([
    "--kind", "ask",
    "--title", "e2e-q1: 一つ目",
    "--body", "順番待ちの確認",
    "--option", "go:進む",
    "--option", "stop:止める",
    "--timeout", "120",
  ]);
  const askB = spawnAsk([
    "--kind", "ask",
    "--title", "e2e-q2: 二つ目",
    "--option", "go:進む",
    "--timeout", "120",
  ]);
  cleanup.push(() => askA.child.kill("SIGTERM"));
  cleanup.push(() => askB.child.kill("SIGTERM"));
  await waitUntil(async () => {
    const list = await herdr(["decision", "list", "--status", "pending"]);
    const decisions = JSON.parse(list.stdout)?.result?.decisions || [];
    return decisions.length === 2;
  });
  step("two decisions pending before daemon start", true);

  // 3. daemon with the fake dialog helper: replays both, shows only the first
  const daemonEnv = {
    ...herdrEnv,
    HERDR_BIN_PATH: HERDR,
    MAC_DIALOG_STATE_DIR: stateDir,
    MAC_DIALOG_HELPER_CMD: JSON.stringify([process.execPath, FAKE_DIALOG]),
    MAC_DIALOG_FAKE_PLAN: planPath,
    MAC_DIALOG_FAKE_DIR: fakeDir,
    MAC_DIALOG_PRESENT_DELAY_MS: "200",
    HOME: tmp,
  };
  const daemon = spawnLogged(
    [process.execPath, DAEMON, "run"],
    daemonEnv,
    daemonLog,
  );
  cleanup.push(() => daemon.kill("SIGTERM"));
  await waitUntil(() => specFiles().length === 1, { timeout: 15_000 });
  await sleep(400); // prove no second dialog pops
  step(
    "exactly one dialog shown for two pending decisions",
    specFiles().length === 1 && aliveDialogs() === 1,
    `specs=${specFiles().length} alive=${aliveDialogs()}`,
  );
  const firstSpec = specFiles()[0].spec;
  step(
    "queue count shown on the first dialog",
    firstSpec.window_title.includes("ほか1件") &&
      firstSpec.text.includes("ほかに 1 件の判断待ち"),
    firstSpec.window_title,
  );
  const firstId = specDecisionId(firstSpec);
  step("first spec carries the decision id", Boolean(firstId), firstId);

  // 4. "press" the dialog: plan entry answers with an option id
  setPlan({ [firstSpec.window_title]: { action: "answer", option_ids: ["go"] } });
  const firstDone =
    firstSpec.text.includes("e2e-q1") ? askA.done : askB.done;
  const firstResult = await firstDone;
  fs.writeFileSync(path.join(OUT, "ask-first.txt"), firstResult.stdout + firstResult.stderr);
  step(
    "dialog answer resolved first decision (responder=mac-dialog)",
    firstResult.code === 0 &&
      firstResult.stdout.includes('"go"') &&
      firstResult.stdout.includes('"mac-dialog"'),
    firstResult.stdout.trim().slice(0, 240),
  );
  await waitUntil(() => specFiles().length === 2, { timeout: 15_000 });
  const secondSpec = specFiles()[1].spec;
  const secondId = specDecisionId(secondSpec);
  step(
    "queued decision shown after the first resolved",
    Boolean(secondId) && secondId !== firstId,
    secondSpec.window_title,
  );

  // 5. free text decision answered with --text only
  const askT = spawnAsk([
    "--kind", "ask",
    "--title", "e2e-text: 自由入力で答えて",
    "--option", "ok:了解",
    "--allow-text",
    "--timeout", "120",
  ]);
  cleanup.push(() => askT.child.kill("SIGTERM"));
  const secondDone = secondSpec.text.includes("e2e-q1") ? askA.done : askB.done;
  // the queued second decision is still open; send its answer via CLI so the
  // text decision can take the screen
  const ext = await herdr([
    "decision", "answer", secondId, "go", "--responder", "cli",
  ]);
  step("answered queued decision via CLI", ext.code === 0, ext.stderr.trim());
  await secondDone;
  await waitUntil(() => specFiles().length === 3, { timeout: 15_000 });
  const textSpec = specFiles()[2].spec;
  step(
    "free-text decision dialog shown",
    textSpec.allow_text === true && /e2e-text/.test(textSpec.text),
    textSpec.window_title,
  );
  setPlan({
    [textSpec.window_title]: {
      action: "answer",
      option_ids: [],
      text: "様子見します",
    },
  });
  const textResult = await askT.done;
  fs.writeFileSync(path.join(OUT, "ask-text.txt"), textResult.stdout + textResult.stderr);
  step(
    "free-text answer reached the decision",
    textResult.code === 0 &&
      textResult.stdout.includes("様子見します") &&
      textResult.stdout.includes('"mac-dialog"'),
    textResult.stdout.trim().slice(0, 240),
  );

  // 6. answering through another channel closes the open dialog
  const askE = spawnAsk([
    "--kind", "ask",
    "--title", "e2e-ext: ほかで答える",
    "--option", "yes:はい",
    "--timeout", "120",
  ]);
  cleanup.push(() => askE.child.kill("SIGTERM"));
  await waitUntil(() => specFiles().length === 4, { timeout: 15_000 });
  await waitUntil(() => aliveDialogs() === 1);
  const extSpec = specFiles()[3].spec;
  const extId = specDecisionId(extSpec);
  const extAnswer = await herdr([
    "decision", "answer", extId, "yes", "--responder", "cli",
  ]);
  step("answered the open dialog's decision via CLI", extAnswer.code === 0);
  await waitUntil(() => aliveDialogs() === 0, { timeout: 15_000 });
  step("open dialog closed after external answer", true);
  await askE.done;

  fs.writeFileSync(
    path.join(OUT, "specs.json"),
    JSON.stringify(specFiles().map((s) => s.spec), null, 2),
  );
  fs.writeFileSync(path.join(OUT, "summary.json"), JSON.stringify(evidence, null, 2));

  const failed = evidence.steps.filter((s) => !s.ok);
  process.stdout.write(
    `\n${failed.length === 0 ? "E2E OK" : "E2E FAILED"} — evidence in ${OUT}\n`,
  );
  finish(failed.length === 0 ? 0 : 1);
}

main().catch((error) => {
  process.stderr.write(`e2e failed: ${error?.stack || error}\n`);
  finish(1);
});
