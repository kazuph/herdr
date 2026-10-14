// Live verification: real herdr server (isolated) + real daemon + the REAL
// osascript dialog.js. Drives the dialog through Accessibility APIs (no mouse
// capture) and captures screenshots with screencapture.
//
//   node test/live-verify.mjs [--out <dir>]
//
// Covers the acceptance scenario: rich decision (descriptions, previews,
// multi-select, free text, long body) shown in a front-most dialog; answer via
// AX checkbox/button presses recorded with responder=mac-dialog; an external
// `decision answer` closes the open dialog; two pending decisions queue.

import { execFile, execFileSync, spawn } from "node:child_process";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { waitUntil } from "./fakes.mjs";

const APP_ROOT = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const REPO_ROOT = path.resolve(APP_ROOT, "..", "..");
const DEBUG_HERDR = path.join(REPO_ROOT, "target", "debug", "herdr");
const HERDR =
  process.env.HERDR_BIN ||
  (fs.existsSync(DEBUG_HERDR) ? DEBUG_HERDR : "herdr");
const DAEMON = path.join(APP_ROOT, "mac-dialog-decisions.mjs");

const outIndex = process.argv.indexOf("--out");
const OUT =
  outIndex >= 0
    ? process.argv[outIndex + 1]
    : path.join(REPO_ROOT, ".artifacts", "feat-mac-dialog-decisions", "live");
fs.mkdirSync(OUT, { recursive: true });
fs.mkdirSync(path.join(OUT, "screens"), { recursive: true });

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "mac-dialog-live-"));
const configHome = path.join(tmp, "config");
const runtimeDir = path.join(tmp, "runtime");
const stateDir = path.join(tmp, "state");
const socketPath = path.join(runtimeDir, "herdr.sock");
fs.mkdirSync(path.join(configHome, "herdr"), { recursive: true });
fs.mkdirSync(runtimeDir, { recursive: true });
fs.mkdirSync(stateDir, { recursive: true });
fs.writeFileSync(path.join(configHome, "herdr", "config.toml"), "onboarding = false\n");

const herdrEnv = {
  XDG_CONFIG_HOME: configHome,
  XDG_RUNTIME_DIR: runtimeDir,
  HERDR_SOCKET_PATH: socketPath,
  HERDR_SOCKET_PATH_EXPLICIT: "1",
  SHELL: "/bin/sh",
};

const evidence = { steps: [] };
function step(name, ok, detail = "") {
  evidence.steps.push({ name, ok, detail });
  process.stdout.write(`${ok ? "PASS" : "FAIL"} ${name}${detail ? ` — ${detail}` : ""}\n`);
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function spawnLogged(argv, env, logName) {
  const log = fs.createWriteStream(path.join(OUT, logName));
  const child = spawn(argv[0], argv.slice(1), {
    env: { ...process.env, ...env },
    stdio: ["ignore", "pipe", "pipe"],
  });
  child.stdout.on("data", (c) => log.write(c));
  child.stderr.on("data", (c) => log.write(c));
  return child;
}

function herdr(args) {
  return new Promise((resolve) => {
    execFile(
      HERDR,
      args,
      { env: { ...process.env, ...herdrEnv }, encoding: "utf8", timeout: 15_000 },
      (error, stdout, stderr) =>
        resolve({
          code: error?.code ?? 0,
          stdout: String(stdout),
          stderr: String(stderr),
        }),
    );
  });
}

// Raw JSON request on the isolated unix socket. The response is a single JSON
// object; it is complete as soon as it parses.
function api(method, params) {
  return new Promise((resolve, reject) => {
    const sock = net.connect(socketPath);
    let buf = "";
    sock.on("connect", () =>
      sock.write(JSON.stringify({ id: "live-verify", method, params }) + "\n"),
    );
    sock.on("data", (chunk) => {
      buf += chunk;
      for (const line of buf.split("\n")) {
        if (!line.trim()) continue;
        try {
          const parsed = JSON.parse(line);
          sock.destroy();
          resolve(parsed);
          return;
        } catch {
          // keep waiting for more data
        }
      }
    });
    sock.on("error", reject);
    setTimeout(() => {
      sock.destroy();
      reject(new Error("api timeout"));
    }, 10_000);
  });
}

// --- Accessibility helpers (Swift AXUIElement; never touches the mouse) ------

const AX_HELPER_SRC = path.join(APP_ROOT, "test", "ax-helper.swift");
const AX_HELPER_BIN = path.join(tmp, "ax-helper");

function dialogPids() {
  // the real dialog is an osacompile applet under the daemon's state dir
  try {
    return execFileSync(
      "pgrep",
      ["-f", "herdr-mac-dialog.app/Contents/MacOS/applet"],
      { encoding: "utf8" },
    )
      .split("\n")
      .map((s) => Number(s.trim()))
      .filter(Boolean);
  } catch {
    return [];
  }
}

function axBin() {
  if (!fs.existsSync(AX_HELPER_BIN)) {
    execFileSync("swiftc", ["-O", "-o", AX_HELPER_BIN, AX_HELPER_SRC], {
      stdio: ["ignore", "pipe", "pipe"],
    });
  }
  return AX_HELPER_BIN;
}

function ax(...args) {
  return execFileSync(axBin(), args.map(String), {
    encoding: "utf8",
    timeout: 20_000,
  }).trim();
}

function axState(pid) {
  try {
    return JSON.parse(ax("probe", pid));
  } catch (error) {
    return { error: String(error).slice(0, 200) };
  }
}

function axPress(pid, titleMatch) {
  try {
    return JSON.parse(ax("press", pid, titleMatch));
  } catch {
    return { pressed: false };
  }
}

function axSetText(pid, text) {
  try {
    return JSON.parse(ax("settext", pid, text));
  } catch {
    return { set: false };
  }
}

function screenshotWindow(pid, name) {
  let bounds;
  try {
    bounds = JSON.parse(ax("bounds", pid));
  } catch {
    return "";
  }
  if (typeof bounds?.x !== "number") return "";
  const file = path.join(OUT, "screens", `${name}.png`);
  execFileSync("screencapture", [
    "-x",
    `-R${bounds.x},${bounds.y},${bounds.w},${bounds.h}`,
    file,
  ]);
  return file;
}

function specShown() {
  const dir = path.join(stateDir, "dialogs");
  try {
    const file = fs.readdirSync(dir).find((f) => f.endsWith(".json"));
    return file
      ? JSON.parse(fs.readFileSync(path.join(dir, file), "utf8"))
      : null;
  } catch {
    return null;
  }
}

const cleanup = [];
async function main() {
  // kill stale dialogs left from earlier verification runs
  for (const pid of dialogPids()) {
    try {
      process.kill(pid, "SIGTERM");
    } catch {}
  }
  await sleep(300);

  // 1. isolated herdr server
  const server = spawnLogged([HERDR, "server"], herdrEnv, "server.log");
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

  // 2. real daemon (default osascript helper)
  const daemon = spawnLogged(
    [process.execPath, DAEMON, "run"],
    {
      ...herdrEnv,
      HERDR_BIN_PATH: HERDR,
      MAC_DIALOG_STATE_DIR: stateDir,
      MAC_DIALOG_CONFIG: path.join(tmp, "config.json"),
      MAC_DIALOG_PRESENT_DELAY_MS: "800",
      HOME: tmp,
    },
    "daemon.log",
  );
  cleanup.push(() => daemon.kill("SIGTERM"));
  await sleep(400);

  const longBody =
    "【前提】複数ファイルの再構成を行います。\n" +
    Array.from(
      { length: 40 },
      (_, i) => `手順${i + 1}: src/module-${i}.ts を整理して移動します。`,
    ).join("\n");

  // 3. rich decision: descriptions, previews, unavailable preview,
  //    multi-select, free text, long body, full origin
  const rich = await api("decision.create", {
    kind: "ask",
    title: "ライブ検証: 大規模リファクタを進めてよい？",
    body: longBody,
    allow_text: true,
    multi_select: true,
    timeout_ms: 600000,
    origin: {
      pane_id: "p9999",
      agent: "devin",
      cwd: "/Users/kazuph/src/github.com/kazuph/herdr",
      branch: "feat/mac-dialog-decisions",
      command: "node scripts/refactor.mjs --all",
      reason: "live verification",
      machine: "kazu-mba-m5",
      workspace_label: "ライブ検証スペース",
      tab_label: "検証タブ",
      pane_label: "ライブペイン",
    },
    options: [
      {
        id: "scope-small",
        label: "小さい範囲だけ進める",
        description: "apps/ 配下だけを対象にする",
        preview: "- apps/mac-dialog-decisions/\n- apps/slack-decisions/",
      },
      {
        id: "scope-all",
        label: "全部まとめて進める",
        description: "src/ も含めて一括で実施する",
        preview_unavailable: true,
      },
      {
        id: "stop",
        label: "やめる",
        description: "今回は何もしない",
      },
    ],
  });
  const richId = rich?.result?.decision?.decision_id;
  step("rich decision created", Boolean(richId), String(richId));

  // 4. dialog shows, front-most, all content readable
  await waitUntil(() => dialogPids().length === 1, { timeout: 20_000 });
  await waitUntil(
    () => {
      const pid = dialogPids()[0];
      const w = pid && axState(pid).windows?.[0];
      // AX attributes can lag the window appearing; wait until populated
      return w && w.title?.includes("herdr 判断待ち") && w.size?.[0] > 0;
    },
    { timeout: 15_000 },
  );
  const richPid = dialogPids()[0];
  // activation can lag the window appearing; wait until it wins the front
  const frontmostOk = await waitUntil(
    () => axState(richPid).frontmost === true,
    { timeout: 10_000 },
  ).then(() => true).catch(() => false);
  const state1 = axState(richPid);
  step(
    "rich dialog shown front-most",
    frontmostOk && state1.windows[0].title.includes("herdr 判断待ち"),
    `frontmost=${frontmostOk} title=${state1.windows[0].title}`,
  );
  const shot1 = screenshotWindow(richPid, "01-rich-multi");
  step("screenshot captured", Boolean(shot1) && fs.existsSync(shot1), shot1);

  // note: question_label is set only by the in-app AskUserQuestion flow and
  // cannot be injected through decision.create; it is covered by unit tests
  const specText = specShown()?.text || "";
  const specOk =
    specText.includes("【判断待ち】") &&
    specText.includes("大規模リファクタ") &&
    specText.includes(longBody) &&
    specText.includes("ライブ検証スペース › 検証タブ › ライブペイン（devin）") &&
    specText.includes("ペインID: p9999") &&
    specText.includes("ブランチ: feat/mac-dialog-decisions") &&
    specText.includes("小さい範囲だけ進める") &&
    specText.includes("apps/ 配下だけを対象にする") &&
    specText.includes("- apps/mac-dialog-decisions/") &&
    specText.includes("プレビューあり（画面で確認）") &&
    specText.includes("期限:");
  step("spec text carries every field verbatim", specOk);

  // 5. answer via AX: check two boxes, type free text, press この選択で回答
  axPress(richPid, "1. 小さい範囲だけ進める");
  axPress(richPid, "2. 全部まとめて進める");
  axSetText(richPid, "まず様子見でお願いします");
  const pressRes = axPress(richPid, "この選択で回答");
  step("multi-select + text answer pressed via AX", pressRes.pressed === true);
  await waitUntil(async () => {
    const got = await herdr(["decision", "get", richId]);
    return got.code === 0 && got.stdout.includes('"answered"');
  }, { timeout: 15_000 });
  const richGet = JSON.parse(
    (await herdr(["decision", "get", richId])).stdout,
  ).result.decision;
  step(
    "multi answer recorded (responder=mac-dialog, ids+text)",
    richGet.answer?.responder === "mac-dialog" &&
      JSON.stringify(richGet.answer.option_ids) ===
        JSON.stringify(["scope-small", "scope-all"]) &&
      richGet.answer.text === "まず様子見でお願いします",
    JSON.stringify(richGet.answer || {}),
  );

  // 6. single-select decision answered by pressing its option button
  const single = await api("decision.create", {
    kind: "command_guard",
    title: "ライブ検証: rm -rf build/ を実行してよい？",
    body: "guard: danger_words に一致",
    timeout_ms: 600000,
    origin: { agent: "claude", command: "rm -rf build/", reason: "danger_words" },
    options: [
      { id: "approve", label: "承認する", description: "このコマンドを実行" },
      { id: "reject", label: "却下する" },
    ],
  });
  const singleId = single?.result?.decision?.decision_id;
  await waitUntil(() => dialogPids().length === 1, { timeout: 15_000 });
  await waitUntil(
    () => {
      const w = axState(dialogPids()[0]).windows?.[0];
      return w && w.title?.includes("herdr 判断待ち") && w.size?.[0] > 0;
    },
    { timeout: 15_000 },
  );
  const singlePid = dialogPids()[0];
  await sleep(500);
  screenshotWindow(singlePid, "02-single");
  const press2 = axPress(singlePid, "1. 承認する");
  step("single-select option pressed via AX", press2.pressed === true);
  await waitUntil(async () => {
    const got = await herdr(["decision", "get", singleId]);
    return got.stdout.includes('"answered"');
  }, { timeout: 15_000 });
  const singleGet = JSON.parse(
    (await herdr(["decision", "get", singleId])).stdout,
  ).result.decision;
  step(
    "single answer recorded (responder=mac-dialog)",
    singleGet.answer?.responder === "mac-dialog" &&
      singleGet.answer?.option_id === "approve",
    JSON.stringify(singleGet.answer || {}),
  );

  // 7. external answer closes the open dialog
  const ext = await api("decision.create", {
    kind: "ask",
    title: "ライブ検証: 別経路で答える",
    timeout_ms: 600000,
    options: [{ id: "ok", label: "OK" }],
  });
  const extId = ext?.result?.decision?.decision_id;
  await waitUntil(() => dialogPids().length === 1, { timeout: 15_000 });
  await waitUntil(
    () => {
      const w = axState(dialogPids()[0]).windows?.[0];
      return w && w.title?.includes("herdr 判断待ち") && w.size?.[0] > 0;
    },
    { timeout: 15_000 },
  );
  await sleep(400);
  const extAns = await herdr([
    "decision", "answer", extId, "ok", "--responder", "cli",
  ]);
  step("answered externally via CLI", extAns.code === 0, extAns.stderr.trim());
  await waitUntil(() => dialogPids().length === 0, { timeout: 15_000 });
  step("dialog closed on external answer", true);

  // 8. two pending decisions present one at a time
  const q1 = await api("decision.create", {
    kind: "ask",
    title: "ライブ検証: キュー一つ目",
    timeout_ms: 600000,
    options: [{ id: "a", label: "A" }],
  });
  const q2 = await api("decision.create", {
    kind: "ask",
    title: "ライブ検証: キュー二つ目",
    timeout_ms: 600000,
    options: [{ id: "b", label: "B" }],
  });
  const q1Id = q1?.result?.decision?.decision_id;
  const q2Id = q2?.result?.decision?.decision_id;
  await waitUntil(() => dialogPids().length === 1, { timeout: 15_000 });
  await waitUntil(
    () => {
      const w = axState(dialogPids()[0]).windows?.[0];
      return w && w.title?.includes("herdr 判断待ち") && w.size?.[0] > 0;
    },
    { timeout: 15_000 },
  );
  await sleep(300);
  step(
    "two pending decisions show exactly one dialog",
    dialogPids().length === 1,
    `pids=${dialogPids().length}`,
  );
  const queueSpec = specShown();
  const queueTitle = queueSpec?.window_title || "";
  step("queue count on the dialog title", queueTitle.includes("ほか1件"), queueTitle);
  screenshotWindow(dialogPids()[0], "03-queued");
  const shownId = /判断待ちID: (\S+)/.exec(queueSpec?.text || "")?.[1] || q1Id;
  await herdr(["decision", "answer", shownId, "a", "--responder", "cli"]);
  await waitUntil(
    () => specShown()?.text.includes("キュー二つ目"),
    { timeout: 15_000 },
  );
  step("queued decision shown after first resolved", true, specShown()?.window_title || "");
  // clean the second up
  await herdr(["decision", "cancel", q2Id]);
  const closedAll = await waitUntil(() => dialogPids().length === 0, {
    timeout: 20_000,
  })
    .then(() => true)
    .catch(() => false);
  step(
    "queued dialog closed after cancel",
    closedAll,
    `pids=${JSON.stringify(dialogPids())}`,
  );

  fs.writeFileSync(path.join(OUT, "summary.json"), JSON.stringify(evidence, null, 2));
  const failed = evidence.steps.filter((s) => !s.ok);
  process.stdout.write(
    `\n${failed.length === 0 ? "LIVE OK" : "LIVE FAILED"} — evidence in ${OUT}\n`,
  );
  for (const fn of cleanup.reverse()) {
    try { fn(); } catch {}
  }
  process.exit(failed.length === 0 ? 0 : 1);
}

main().catch((error) => {
  process.stderr.write(`live-verify failed: ${error?.stack || error}\n`);
  fs.writeFileSync(path.join(OUT, "summary.json"), JSON.stringify(evidence, null, 2));
  for (const fn of cleanup.reverse()) {
    try { fn(); } catch {}
  }
  process.exit(1);
});
