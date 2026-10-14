#!/usr/bin/env node
// herdr plugin daemon: show pending decisions as a front-most macOS dialog.
//
// A `herdr decision watch --all-machines` subscription feeds `created` /
// `resolved` / `machine_up` / `machine_down` lines. Each pending decision is
// presented one at a time in a native dialog (dialog.js, driven through
// osascript -l JavaScript) that shows the full decision text and returns the
// picked option(s) and/or free text as a JSON line on stdout. The daemon turns
// that into `herdr decision answer ... --responder mac-dialog`. When the
// decision resolves anywhere else, the open dialog is killed so a stale answer
// is never sent.

import { execFile, execFileSync, spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import process from "node:process";
import readline from "node:readline";
import { fileURLToPath } from "node:url";

const PLUGIN_ID = "kazuph.mac-dialog-decisions";
const RUN_LABEL = "mac-dialog-decisions";
const WATCH_RETRY_MS = 2_000;
const DAEMON_PID_FILE = "daemon.json";
const RESPONDER = "mac-dialog";

function log(...args) {
  const at = new Date().toTimeString().slice(0, 8);
  process.stderr.write(`[mac-dialog-decisions ${at}] ${args.join(" ")}\n`);
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function homePath(...parts) {
  return path.join(os.homedir(), ...parts);
}

function loadConfig(env = process.env) {
  const configDir =
    env.HERDR_PLUGIN_CONFIG_DIR ||
    homePath(".config", "herdr", "plugins", "config", PLUGIN_ID);
  const configPath =
    env.MAC_DIALOG_CONFIG || path.join(configDir, "config.json");
  let file = {};
  try {
    file = JSON.parse(fs.readFileSync(configPath, "utf8"));
  } catch {
    file = {};
  }
  const stateDir =
    env.MAC_DIALOG_STATE_DIR ||
    env.HERDR_PLUGIN_STATE_DIR ||
    homePath(".local", "state", "herdr", "plugins", PLUGIN_ID);
  const pickNumber = (envValue, fileValue, fallback) => {
    const parsed = Number(envValue ?? fileValue);
    return Number.isFinite(parsed) && parsed >= 0 ? parsed : fallback;
  };
  let helperCmd = null;
  if (env.MAC_DIALOG_HELPER_CMD) {
    try {
      const parsed = JSON.parse(env.MAC_DIALOG_HELPER_CMD);
      if (Array.isArray(parsed) && parsed.length > 0) helperCmd = parsed;
    } catch {
      log(`MAC_DIALOG_HELPER_CMD の JSON が読めません: ${env.MAC_DIALOG_HELPER_CMD}`);
    }
  }
  if (!helperCmd && Array.isArray(file.helper_command) && file.helper_command.length > 0) {
    helperCmd = file.helper_command.map(String);
  }
  return {
    enabled:
      (env.MAC_DIALOG_ENABLED ?? file.enabled ?? true) !== false &&
      env.MAC_DIALOG_ENABLED !== "0",
    herdrBin: env.HERDR_BIN_PATH || file.herdr_bin || "herdr",
    helperCmd, // null = built-in osascript dialog.js
    // Brief delay before a queued decision takes the screen, so a burst of
    // simultaneous decisions reports the true queue size on the first dialog.
    presentDelayMs: pickNumber(
      env.MAC_DIALOG_PRESENT_DELAY_MS,
      file.present_delay_ms,
      250,
    ),
    stateDir,
    configDir,
    configPath,
  };
}

// The daemon outlives the server that launched it: a Homebrew install records
// HERDR_BIN_PATH as a versioned Cellar path that `brew upgrade` deletes, so
// every herdr invocation resolves the binary afresh — the configured path
// while it exists, then PATH, then the usual prefixes.
function resolveHerdrBin(config, logFn = () => {}) {
  const configured = config.herdrBin;
  if (configured && configured !== "herdr" && fs.existsSync(configured)) {
    return configured;
  }
  const candidates = [
    ...(process.env.PATH || "")
      .split(path.delimiter)
      .filter(Boolean)
      .map((dir) => path.join(dir, "herdr")),
    "/opt/homebrew/bin/herdr",
    path.join(os.homedir(), ".local", "bin", "herdr"),
  ];
  for (const candidate of candidates) {
    if (fs.existsSync(candidate)) {
      if (configured && configured !== "herdr" && !fs.existsSync(configured)) {
        logFn(
          `HERDR_BIN_PATH (${configured}) が存在しないため ${candidate} を使います`,
        );
      }
      return candidate;
    }
  }
  return null;
}

function execFileText(file, args, options = {}) {
  return new Promise((resolve) => {
    execFile(
      file,
      args,
      { encoding: "utf8", timeout: options.timeoutMs ?? 30_000, ...options },
      (error, stdout, stderr) => {
        resolve({
          code:
            error && typeof error.code === "number" ? error.code : error ? 1 : 0,
          stdout: String(stdout || ""),
          stderr: String(stderr || (error ? error.message : "")),
        });
      },
    );
  });
}

// --- dialog spec (display text) ------------------------------------------------

function fmtDeadline(decision, now = Date.now()) {
  if (!decision.expires_unix_ms) return "期限なし";
  const at = new Date(decision.expires_unix_ms);
  const remainSec = Math.max(
    0,
    Math.round((decision.expires_unix_ms - now) / 1000),
  );
  const hh = String(at.getHours()).padStart(2, "0");
  const mm = String(at.getMinutes()).padStart(2, "0");
  const remain =
    remainSec >= 120 ? `${Math.round(remainSec / 60)}分` : `${remainSec}秒`;
  return `${hh}:${mm}（残り約${remain}）`;
}

function optionDescription(option) {
  return typeof option?.description === "string" ? option.description.trim() : "";
}

function optionPreview(option) {
  return typeof option?.preview === "string" ? option.preview.trimEnd() : "";
}

// マシン › スペース › タブ › ペイン（エージェント）— only the parts that exist.
function originBreadcrumb(decision, machine) {
  const origin = decision.origin || {};
  const parts = [];
  const machineLabel = machine?.label || machine?.id || origin.machine;
  if (machineLabel) parts.push(machineLabel);
  if (origin.workspace_label) parts.push(origin.workspace_label);
  if (origin.tab_label) parts.push(origin.tab_label);
  const pane = origin.pane_label
    ? origin.agent
      ? `${origin.pane_label}（${origin.agent}）`
      : origin.pane_label
    : origin.agent || "";
  if (pane) parts.push(pane);
  return parts.join(" › ");
}

// The complete dialog text. Nothing is truncated: every option keeps its
// label, description and preview (or the "プレビューあり（画面で確認）" marker
// when herdr could not capture it), and the whole block lives in a scrollable
// text view.
function buildDialogText(decision, machine, queueRemaining = 0) {
  const lines = [];
  const head = ["【判断待ち】"];
  if (decision.question_label) head.push(decision.question_label);
  lines.push(head.join(" "));
  lines.push(`■ ${decision.title || decision.decision_id}`);
  if (decision.body) {
    lines.push("", String(decision.body));
  }

  const origin = decision.origin || {};
  const breadcrumb = originBreadcrumb(decision, machine);
  const detail = [];
  if (origin.pane_id) detail.push(`ペインID: ${origin.pane_id}`);
  if (origin.cwd) detail.push(`ディレクトリ: ${origin.cwd}`);
  if (origin.branch) detail.push(`ブランチ: ${origin.branch}`);
  if (origin.command) detail.push(`コマンド: ${origin.command}`);
  if (origin.reason) detail.push(`理由: ${origin.reason}`);
  if (origin.machine) detail.push(`マシン: ${origin.machine}`);
  if (breadcrumb || detail.length > 0) {
    lines.push("", "── 出どころ ──");
    if (breadcrumb) lines.push(breadcrumb);
    lines.push(...detail);
  }

  const options = Array.isArray(decision.options) ? decision.options : [];
  if (options.length > 0) {
    lines.push("", "── 選択肢 ──");
    options.forEach((option, index) => {
      const label = option.label || option.id;
      lines.push(`${index + 1}. ${label}`);
      const description = optionDescription(option);
      if (description) {
        lines.push(...description.split("\n").map((l) => `   ${l}`));
      }
      const preview = optionPreview(option);
      if (preview) {
        lines.push(...preview.split("\n").map((l) => `   ${l}`));
      } else if (option?.preview_unavailable) {
        lines.push("   プレビューあり（画面で確認）");
      }
    });
    if (decision.multi_select === true) {
      lines.push("", "※ 複数選択できます。チェックしてから「この選択で回答」。");
    }
    if (decision.allow_text) {
      lines.push("※ 下の自由入力に書いた文も一緒に送られます（選択肢なしでも可）。");
    }
  } else if (decision.allow_text) {
    lines.push("", "※ 下の自由入力に書いて「送信」してください。");
  }

  lines.push("", `期限: ${fmtDeadline(decision)}`);
  if (queueRemaining > 0) {
    lines.push(`ほかに ${queueRemaining} 件の判断待ちがあります。`);
  }
  lines.push(`判断待ちID: ${decision.decision_id}`);
  return lines.join("\n");
}

function buildDialogSpec(decision, machine, queueRemaining = 0) {
  const titleBits = ["herdr 判断待ち"];
  if (decision.question_label) titleBits.push(decision.question_label);
  if (queueRemaining > 0) titleBits.push(`（ほか${queueRemaining}件）`);
  return {
    window_title: titleBits.join(" "),
    text: buildDialogText(decision, machine, queueRemaining),
    options: (Array.isArray(decision.options) ? decision.options : []).map(
      (option, index) => ({
        id: option.id,
        title: `${index + 1}. ${String(option.label || option.id).replace(/\s+/g, " ")}`,
      }),
    ),
    multi_select: decision.multi_select === true,
    allow_text: decision.allow_text === true,
  };
}

// --- daemon ---------------------------------------------------------------------

function decisionKey(machineId, decisionId) {
  return `${machineId} ${decisionId}`;
}

function decisionIdOf(decision) {
  return String(decision?.decision_id || "");
}

export class MacDialogDecisionsDaemon {
  constructor({
    config,
    spawnWatch, // () => {lines: AsyncIterable<object>, close()}
    runAnswer, // (argv: string[]) => Promise<{code, stdout, stderr}>
    spawnDialog, // (specPath, spec) => {lines: AsyncIterable<string>, kill()}
    specFilePath, // (key) => path to write the spec JSON
    log: logFn = log,
  }) {
    this.config = config;
    this.spawnWatch = spawnWatch;
    this.runAnswer = runAnswer;
    this.spawnDialog = spawnDialog;
    this.specFilePath = specFilePath;
    this.log = logFn;
    this.entries = new Map(); // key -> entry
    this.queue = []; // keys waiting for the dialog
    this.showing = null; // key currently on screen
    this.child = null; // dialog child handle while showing
    this.machinesDown = new Set();
    this.stopped = false;
    this._pumpTimer = null;
    this._watchHandle = null;
    this._watchDone = null;
  }

  async start() {
    this._watchDone = this.watchLoop();
  }

  async stop() {
    this.stopped = true;
    if (this._pumpTimer) clearTimeout(this._pumpTimer);
    this._watchHandle?.close?.();
    this._closeDialog();
    await Promise.allSettled([this._watchDone]);
  }

  // A short delay before taking the screen lets a burst of simultaneous
  // decisions coalesce so the first dialog reports the true queue size.
  schedulePump() {
    if (this.stopped) return;
    // reset on each new event so a burst coalesces into one presentation
    clearTimeout(this._pumpTimer);
    this._pumpTimer = setTimeout(() => {
      this._pumpTimer = null;
      this.pump();
    }, this.config.presentDelayMs ?? 0);
  }

  // ---- watch stream ---------------------------------------------------------

  async watchLoop() {
    while (!this.stopped) {
      const watch = await this.spawnWatch();
      this._watchHandle = watch;
      try {
        for await (const line of watch.lines) {
          if (this.stopped) break;
          try {
            this.onWatchLine(line);
          } catch (error) {
            this.log(`watch 行の処理に失敗: ${error.message}`);
          }
        }
      } catch (error) {
        this.log(`decision watch が切れました: ${error.message}`);
      }
      if (this.stopped) break;
      this.log("decision watch を再接続します");
      await sleep(WATCH_RETRY_MS);
    }
  }

  onWatchLine(line) {
    const event = line?.event;
    const machine = line?.machine || {};
    const mid = String(machine.id || "local");
    const decision = line?.decision || {};
    const did = decisionIdOf(decision);

    if (event === "machine_up") {
      this.machinesDown.delete(mid);
      return;
    }
    if (event === "machine_down") {
      this.onMachineDown(mid, line?.reason || "");
      return;
    }
    if (event === "created" && did) {
      this.onCreated(mid, machine, decision);
      return;
    }
    if (event === "resolved" && did) {
      this.onResolved(mid, decision);
    }
  }

  onCreated(mid, machine, decision) {
    const key = decisionKey(mid, decisionIdOf(decision));
    const existing = this.entries.get(key);
    if (existing) {
      // Reconnect replay, or a repeat created for a decision whose dialog
      // crashed. A dismissed dialog stays dismissed for this daemon's lifetime.
      existing.machineDown = false;
      existing.decision = decision;
      if (
        !existing.showing &&
        !existing.queued &&
        !existing.dismissed &&
        !existing.resolved
      ) {
        existing.queued = true;
        this.queue.push(key);
        this.schedulePump();
      }
      return;
    }
    if (decision.status && decision.status !== "pending") return;

    const entry = {
      key,
      machineId: mid,
      machine,
      decision,
      queued: false,
      showing: false,
      dismissed: false,
      resolved: false,
      machineDown: false,
      answerPending: false,
    };
    this.entries.set(key, entry);
    if (this.machinesDown.has(mid)) {
      this.log(`${mid} は切断中: ${decision.decision_id} のダイアログは出しません`);
      entry.machineDown = true;
      return;
    }
    entry.queued = true;
    this.queue.push(key);
    this.log(`判断待ちをキューに追加: ${decision.decision_id} (queue=${this.queue.length})`);
    this.schedulePump();
  }

  onResolved(mid, decision) {
    const key = decisionKey(mid, decisionIdOf(decision));
    const entry = this.entries.get(key);
    if (entry) {
      entry.resolved = true;
      entry.queued = false;
      this.queue = this.queue.filter((k) => k !== key);
      if (entry.showing) this._closeDialog();
      this.entries.delete(key);
    }
    this.log(`決定済み: ${decision.decision_id} (${decision.status})`);
  }

  onMachineDown(mid, reason) {
    this.machinesDown.add(mid);
    for (const entry of [...this.entries.values()]) {
      if (entry.machineId !== mid) continue;
      entry.machineDown = true;
      entry.queued = false;
      this.queue = this.queue.filter((k) => k !== entry.key);
      if (entry.showing) this._closeDialog();
      this.entries.delete(entry.key);
    }
    this.log(`マシン切断: ${mid} (${reason})`);
  }

  // ---- dialog presentation ----------------------------------------------------

  pump() {
    if (this.showing || this.stopped) return;
    while (this.queue.length > 0) {
      const key = this.queue.shift();
      const entry = this.entries.get(key);
      if (!entry || entry.resolved || entry.dismissed || entry.machineDown) {
        continue;
      }
      entry.queued = false;
      this._present(entry);
      return;
    }
  }

  _present(entry) {
    const spec = buildDialogSpec(
      entry.decision,
      entry.machine,
      this.queue.length,
    );
    let specPath = "";
    try {
      specPath = this.specFilePath(entry.key);
      spec.result_path = `${specPath}.result.jsonl`;
      fs.mkdirSync(path.dirname(specPath), { recursive: true });
      fs.writeFileSync(specPath, JSON.stringify(spec), { mode: 0o600 });
    } catch (error) {
      this.log(`spec ファイルの書き込みに失敗 (${entry.decision.decision_id}): ${error.message}`);
      return;
    }
    entry.showing = true;
    this.showing = entry.key;
    this.log(`ダイアログを表示: ${entry.decision.decision_id}`);
    const child = this.spawnDialog(specPath, spec);
    child.entry = entry;
    child.specPath = specPath;
    child.handled = false;
    this.child = child;
    this._consumeChild(child);
  }

  async _consumeChild(child) {
    try {
      for await (const line of child.lines) {
        if (child.handled || this.stopped) continue;
        let message;
        try {
          message = JSON.parse(line);
        } catch {
          this.log(`ダイアログの JSON でない行を捨てました: ${String(line).slice(0, 120)}`);
          continue;
        }
        if (message?.event === "shown") continue;
        if (typeof message?.result !== "string") continue;
        child.handled = true;
        await this._handleResult(child, message);
      }
    } catch (error) {
      this.log(`ダイアログプロセスの読み取りに失敗: ${error.message}`);
    }
    // exited
    const entry = child.entry;
    try {
      fs.unlinkSync(child.specPath);
      fs.unlinkSync(`${child.specPath}.result.jsonl`);
    } catch {}
    entry.showing = false;
    if (this.showing === entry.key) this.showing = null;
    this.child = null;
    this.schedulePump();
  }

  async _handleResult(child, message) {
    const entry = child.entry;
    if (message.result === "answer") {
      const ids = Array.isArray(message.option_ids)
        ? message.option_ids.filter((id) => typeof id === "string" && id)
        : [];
      const text = typeof message.text === "string" ? message.text.trim() : "";
      if (!entry.resolved && (ids.length > 0 || text)) {
        await this.submitAnswer(entry, { optionIds: ids, text });
      }
      return;
    }
    if (message.result === "dismiss" || message.result === "closed") {
      entry.dismissed = true;
      this.log(`ダイアログを閉じました（あとで）: ${entry.decision.decision_id}`);
    }
  }

  _closeDialog() {
    if (!this.child) return;
    this.log(`ダイアログを閉じます: ${this.child.entry.decision.decision_id}`);
    try {
      this.child.kill();
    } catch {}
  }

  // ---- answering ---------------------------------------------------------------

  async submitAnswer(entry, { optionIds = [], text = "" }) {
    if (entry.resolved || entry.answerPending) return;
    entry.answerPending = true;
    const did = entry.decision.decision_id;
    const args = [
      "decision",
      "answer",
      did,
      ...optionIds,
      "--responder",
      RESPONDER,
      "--machine",
      entry.machineId,
    ];
    if (text) args.push("--text", text);
    this.log(
      `回答を送信: ${did} option=${optionIds.join(",") || "-"}${text ? " +text" : ""}`,
    );
    const result = await this.runAnswer(args);
    if (result.code === 0) return; // the resolved event closes/removes the entry
    entry.answerPending = false;
    const combined = `${result.stdout}\n${result.stderr}`;
    if (combined.includes("decision_already_resolved")) {
      this.log(`ほかの窓口で決定済みでした: ${did}`);
      return;
    }
    const detail =
      combined.trim().split("\n").pop()?.slice(0, 300) || "unknown";
    this.log(`回答の送信に失敗 (${did}): ${detail}`);
  }
}

// --- process plumbing -------------------------------------------------------------

function pluginRootDir() {
  return path.dirname(fileURLToPath(import.meta.url));
}

function spawnDecisionWatch(config, logFn) {
  const bin = resolveHerdrBin(config, logFn);
  if (!bin) {
    return {
      lines: (async function* () {
        throw new Error(
          `herdr 実行ファイルが見つかりません (HERDR_BIN_PATH=${config.herdrBin})。` +
            "PATH・/opt/homebrew/bin・~/.local/bin を確認してください",
        );
      })(),
      close() {},
    };
  }
  const child = spawn(bin, ["decision", "watch", "--all-machines"], {
    stdio: ["ignore", "pipe", "pipe"],
  });
  child.stderr.on("data", (chunk) => {
    for (const line of String(chunk).split("\n")) {
      if (line.trim()) logFn(`watch stderr: ${line.trim()}`);
    }
  });
  const rl = readline.createInterface({ input: child.stdout });
  let resolveExit;
  const exited = new Promise((resolve) => {
    resolveExit = resolve;
    child.on("exit", resolve);
  });
  return {
    lines: (async function* () {
      try {
        for await (const raw of rl) {
          if (!raw.trim()) continue;
          try {
            yield JSON.parse(raw);
          } catch {
            logFn(`watch の JSON でない行を捨てました: ${raw.slice(0, 120)}`);
          }
        }
        const code = await exited;
        throw new Error(`decision watch exited (code ${code})`);
      } finally {
        rl.close();
        try {
          child.kill();
        } catch {}
      }
    })(),
    close() {
      rl.close();
      try {
        child.kill();
      } catch {}
      resolveExit?.(0);
    },
  };
}

// Spawns a dialog helper as a plain child process (custom MAC_DIALOG_HELPER_CMD
// in tests, or `osascript -l JavaScript dialog.js` as a fallback). The helper
// prints JSON lines on stdout; the first {"result": ...} line is the user's
// answer and {"event":"shown"} is a heartbeat that the window hit the screen.
function spawnHelperDialog(config, specPath, logFn = () => {}) {
  const argv = config.helperCmd || [
    "osascript",
    "-l",
    "JavaScript",
    path.join(pluginRootDir(), "dialog.js"),
  ];
  const child = spawn(argv[0], [...argv.slice(1), specPath], {
    stdio: ["ignore", "pipe", "pipe"],
  });
  child.on("error", (error) =>
    logFn(`dialog spawn に失敗: ${error.message} (argv=${argv.join(" ")})`),
  );
  child.on("exit", (code, signal) =>
    logFn(`dialog が終了: code=${code} signal=${signal}`),
  );
  child.stderr.on("data", (chunk) => {
    for (const line of String(chunk).split("\n")) {
      if (line.trim()) logFn(`dialog stderr: ${line.trim()}`);
    }
  });
  const rl = readline.createInterface({ input: child.stdout });
  let resolveExit;
  const exited = new Promise((resolve) => {
    resolveExit = resolve;
    child.on("exit", resolve);
  });
  return {
    lines: (async function* () {
      try {
        for await (const raw of rl) {
          if (!raw.trim()) continue;
          yield raw;
        }
      } finally {
        rl.close();
      }
    })(),
    exited,
    kill() {
      try {
        child.kill("SIGTERM");
      } catch {}
      resolveExit?.(0);
    },
  };
}

// The osacompile applet that hosts dialog.js. A bundled .app launched through
// `open` is registered with LaunchServices, so unlike a bare osascript child
// it is allowed to become the active application — which is what makes the
// dialog steal focus and accept keyboard input.
function appletPath(config) {
  return path.join(config.stateDir, "herdr-mac-dialog.app");
}

function ensureApplet(config, logFn = () => {}) {
  const appPath = appletPath(config);
  const source = path.join(pluginRootDir(), "dialog.js");
  try {
    const srcMtime = fs.statSync(source).mtimeMs;
    const appMtime = fs.existsSync(path.join(appPath, "Contents", "MacOS", "applet"))
      ? fs.statSync(path.join(appPath, "Contents", "MacOS", "applet")).mtimeMs
      : 0;
    if (appMtime >= srcMtime) return appPath;
    fs.mkdirSync(config.stateDir, { recursive: true });
    // osacompile only produces a bundle when the output name ends in .app
    const tmpApp = `${appPath}.tmp-${process.pid}.app`;
    const res = spawnSync("osacompile", ["-l", "JavaScript", "-o", tmpApp, source], {
      encoding: "utf8",
      timeout: 30_000,
    });
    if (res.error || res.status !== 0) {
      logFn(
        `アプレットのビルドに失敗 (osascript にフォールバック): ${res.stderr || res.error?.message || res.status}`,
      );
      return null;
    }
    fs.rmSync(appPath, { recursive: true, force: true });
    fs.renameSync(tmpApp, appPath);
    logFn("アプレットをビルドしました: " + appPath);
    return appPath;
  } catch (error) {
    logFn(`アプレットのビルドに失敗 (osascript にフォールバック): ${error.message}`);
    return null;
  }
}

// Spawns the dialog as a bundled applet via `open`. stdout is not available
// through LaunchServices, so dialog.js appends the same JSON lines to
// spec.result_path instead; this polls that file. kill() signals the applet
// by pid (learned from the "shown" event) — needed when a decision is
// resolved through another channel.
function spawnAppletDialog(config, specPath, spec, logFn = () => {}) {
  const app = ensureApplet(config, logFn);
  if (!app) return spawnHelperDialog(config, specPath, logFn);
  const resultPath = spec.result_path;
  try {
    fs.rmSync(resultPath, { force: true });
  } catch {}
  const child = spawn("open", ["-n", "-W", app, "--args", specPath], {
    stdio: ["ignore", "pipe", "pipe"],
  });
  child.on("error", (error) =>
    logFn(`open に失敗: ${error.message}`),
  );
  child.stderr.on("data", (chunk) => {
    for (const line of String(chunk).split("\n")) {
      if (line.trim()) logFn(`open stderr: ${line.trim()}`);
    }
  });
  let exitedFlag = false;
  let resolveExit;
  const exited = new Promise((resolve) => {
    resolveExit = resolve;
    child.on("exit", (code, signal) => {
      exitedFlag = true;
      logFn(`applet が終了: code=${code} signal=${signal}`);
      resolve();
    });
  });
  let appletPid = null;
  const appletExe = `${app}/Contents/MacOS/applet`;
  const findAppletPid = () => {
    if (appletPid) return appletPid;
    try {
      const out = execFileSync("pgrep", ["-f", appletExe], {
        encoding: "utf8",
      });
      appletPid = Number(out.split("\n")[0].trim()) || null;
    } catch {}
    return appletPid;
  };
  return {
    lines: (async function* () {
      let offset = 0;
      let remainder = "";
      // outlive the `open` child slightly: the applet may write its final
      // result just before `open -W` returns
      for (let i = 0; ; i++) {
        let stat = null;
        try {
          stat = fs.statSync(resultPath);
        } catch {}
        if (stat && stat.size > offset) {
          const fd = fs.openSync(resultPath, "r");
          const buf = Buffer.alloc(stat.size - offset);
          fs.readSync(fd, buf, 0, buf.length, offset);
          fs.closeSync(fd);
          offset = stat.size;
          const chunk = remainder + buf.toString("utf8");
          const parts = chunk.split("\n");
          remainder = parts.pop() ?? "";
          for (const line of parts) {
            if (!line.trim()) continue;
            try {
              const message = JSON.parse(line);
              if (message?.event === "shown" && message.pid) {
                appletPid = Number(message.pid);
              }
            } catch {}
            yield line;
          }
        } else if (exitedFlag) {
          // applet quit and no new bytes — done after one final drain
          if (i > 10) break;
        }
        await sleep(200);
      }
    })(),
    exited,
    kill() {
      const pid = findAppletPid();
      if (pid) {
        try {
          process.kill(pid, "SIGTERM");
        } catch {}
      }
      try {
        child.kill("SIGTERM");
      } catch {}
      resolveExit?.(0);
    },
  };
}

function createRunAnswer(config, logFn = () => {}) {
  return (args) => {
    const bin = resolveHerdrBin(config, logFn);
    if (!bin) {
      return Promise.resolve({
        code: 127,
        stdout: "",
        stderr:
          `herdr 実行ファイルが見つかりません (HERDR_BIN_PATH=${config.herdrBin})。` +
          "PATH・/opt/homebrew/bin・~/.local/bin を確認してください",
      });
    }
    return execFileText(bin, args, { timeoutMs: 60_000 });
  };
}

// --- singleton ensure ----------------------------------------------------------------

function pidAlive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

function pidLooksOurs(pid) {
  const out = (() => {
    try {
      return execFileSync("ps", ["-o", "command=", "-p", String(pid)], {
        encoding: "utf8",
        stdio: ["ignore", "pipe", "ignore"],
      }).trim();
    } catch {
      return "";
    }
  })();
  return out.includes("mac-dialog-decisions.mjs");
}

function daemonPidPath(stateDir) {
  return path.join(stateDir, DAEMON_PID_FILE);
}

function readDaemonPid(stateDir) {
  try {
    const parsed = JSON.parse(fs.readFileSync(daemonPidPath(stateDir), "utf8"));
    return Number(parsed.pid) || 0;
  } catch {
    return 0;
  }
}

function daemonRunning(stateDir) {
  const pid = readDaemonPid(stateDir);
  return pid > 0 && pidAlive(pid) && pidLooksOurs(pid);
}

function writeDaemonPid(stateDir) {
  fs.mkdirSync(stateDir, { recursive: true });
  fs.writeFileSync(
    daemonPidPath(stateDir),
    JSON.stringify({ pid: process.pid, started: Date.now() }),
    { mode: 0o600 },
  );
}

function shellQuote(value) {
  return `'${String(value).replace(/'/g, "'\\''")}'`;
}

async function ensureDaemon(config, env) {
  fs.mkdirSync(config.stateDir, { recursive: true });
  const lockDir = path.join(config.stateDir, "ensure.lock");
  // mkdir is atomic: it serializes concurrent event-hook invocations.
  for (;;) {
    try {
      fs.mkdirSync(lockDir);
      break;
    } catch {
      let age = Number.MAX_SAFE_INTEGER;
      try {
        age = Date.now() - fs.statSync(lockDir).mtimeMs;
      } catch {}
      if (age > 60_000) {
        fs.rmSync(lockDir, { recursive: true, force: true });
        continue;
      }
      await sleep(200);
      if (age > 10_000) return; // another ensure is mid-flight
    }
  }
  try {
    if (daemonRunning(config.stateDir)) {
      log(`デーモンは起動済み (pid ${readDaemonPid(config.stateDir)})`);
      return;
    }
    let startedAt = 0;
    try {
      startedAt = JSON.parse(
        fs.readFileSync(path.join(config.stateDir, "starting.json"), "utf8"),
      ).started;
    } catch {}
    if (Date.now() - startedAt < 30_000) {
      log("起動中のデーモンがあるため何もしません");
      return;
    }

    const pluginRoot = pluginRootDir();
    const exportsList = [
      ["HERDR_PLUGIN_CONFIG_DIR", config.configDir],
      ["HERDR_PLUGIN_STATE_DIR", config.stateDir],
      ["HERDR_SOCKET_PATH", env.HERDR_SOCKET_PATH || ""],
      ["HERDR_BIN_PATH", resolveHerdrBin(config, log) || config.herdrBin],
      ["PATH", env.PATH || ""],
    ]
      .filter(([, value]) => value)
      .map(([key, value]) => `${key}=${shellQuote(value)}`)
      .join(" ");
    const inner = `export ${exportsList}; exec node ${shellQuote(path.join(pluginRoot, "mac-dialog-decisions.mjs"))} run`;
    const daemonBin = resolveHerdrBin(config, log);
    if (!daemonBin) {
      log(
        `herdr 実行ファイルが見つかりません (HERDR_BIN_PATH=${config.herdrBin})。` +
          "デーモンを起動できません",
      );
      return;
    }
    const result = await execFileText(
      daemonBin,
      [
        "run",
        "--label",
        RUN_LABEL,
        "--cwd",
        pluginRoot,
        "--completion",
        "none",
        "--",
        "sh",
        "-c",
        inner,
      ],
      { timeoutMs: 15_000 },
    );
    if (result.code !== 0) {
      log(`herdr run に失敗: ${(result.stderr || result.stdout).trim()}`);
      return;
    }
    fs.writeFileSync(
      path.join(config.stateDir, "starting.json"),
      JSON.stringify({ started: Date.now() }),
      { mode: 0o600 },
    );
    log(`デーモンを herdr run ジョブとして起動しました (label=${RUN_LABEL})`);
  } finally {
    fs.rmSync(lockDir, { recursive: true, force: true });
  }
}

// --- entry point ----------------------------------------------------------------------

async function gateCheck(config) {
  if (process.platform !== "darwin") {
    log("macOS 以外では動きません (platforms=[\"macos\"])");
    return false;
  }
  if (!config.enabled) {
    log("設定で無効化されています (enabled=false)");
    return false;
  }
  return true;
}

async function main() {
  const mode = process.argv[2] || "run";
  const config = loadConfig(process.env);

  if (!(await gateCheck(config))) return;

  if (mode === "ensure") {
    await ensureDaemon(config, process.env);
    return;
  }

  if (mode !== "run") {
    process.stderr.write("usage: mac-dialog-decisions.mjs [run|ensure]\n");
    process.exit(2);
  }

  if (daemonRunning(config.stateDir)) {
    log(
      `すでにデーモンが動いています (pid ${readDaemonPid(config.stateDir)})。終了します。`,
    );
    return;
  }

  // Clean up dialog spec files and orphaned applets left by an earlier daemon.
  const specDir = path.join(config.stateDir, "dialogs");
  try {
    fs.rmSync(specDir, { recursive: true, force: true });
  } catch {}
  try {
    // only this daemon's applet: the executable path lives under stateDir
    const orphans = execFileSync(
      "pgrep",
      ["-f", `${appletPath(config)}/Contents/MacOS/applet`],
      { encoding: "utf8" },
    )
      .split("\n")
      .map((s) => Number(s.trim()))
      .filter(Boolean);
    for (const pid of orphans) {
      try {
        process.kill(pid, "SIGTERM");
      } catch {}
    }
  } catch {}

  writeDaemonPid(config.stateDir);
  const daemon = new MacDialogDecisionsDaemon({
    config,
    spawnWatch: () => Promise.resolve(spawnDecisionWatch(config, log)),
    runAnswer: createRunAnswer(config, log),
    spawnDialog: (specPath, spec) =>
      config.helperCmd
        ? spawnHelperDialog(config, specPath, log)
        : spawnAppletDialog(config, specPath, spec, log),
    specFilePath: (key) =>
      path.join(specDir, `${key.replace(/[^A-Za-z0-9_.-]/g, "_")}.json`),
    log,
  });
  await daemon.start();
  log("watch 開始 (herdr decision watch --all-machines)");

  const shutdown = () => {
    void daemon.stop().then(() => process.exit(0));
  };
  process.on("SIGINT", shutdown);
  process.on("SIGTERM", shutdown);
}

const invokedAsScript = (() => {
  const entry = process.argv[1];
  if (!entry) return false;
  try {
    return (
      fs.realpathSync(entry) === fs.realpathSync(fileURLToPath(import.meta.url))
    );
  } catch {
    return false;
  }
})();

if (invokedAsScript) {
  main().catch((error) => {
    log(`fatal: ${error?.stack || error}`);
    process.exit(1);
  });
}

export {
  buildDialogSpec,
  buildDialogText,
  createRunAnswer,
  decisionKey,
  fmtDeadline,
  loadConfig,
  originBreadcrumb,
  resolveHerdrBin,
  spawnHelperDialog,
  spawnAppletDialog,
};
