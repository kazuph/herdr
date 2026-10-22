#!/usr/bin/env node
// herdr plugin daemon: show pending decisions as normal macOS windows.
//
// A `herdr decision watch --all-machines` subscription feeds `created` /
// `resolved` / `machine_up` / `machine_down` lines. Each pending decision is
// presented together in one native AppKit app (dialog.swift) that shows
// where it comes from, the question, every option with its description and
// preview, and a free-text field, and returns the picked option(s) and/or
// free text as a JSON line on stdout. The daemon turns
// that into `herdr decision answer ... --responder mac-dialog`. When the
// decision resolves anywhere else, only its keyed window closes so a stale answer
// is never sent.

import { execFile, execFileSync, spawn, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import process from "node:process";
import readline from "node:readline";
import { fileURLToPath } from "node:url";

const MAC_DIALOG_SIGNATURE_REQUIREMENT = '=anchor apple generic and identifier "dev.herdr.mac-dialog-decisions" and certificate leaf[subject.OU] = "67M8FDFZ28" and certificate leaf[subject.CN] = "Developer ID Application: KAZUHIRO HOMMA (67M8FDFZ28)"';
const PLUGIN_ID = "kazuph.mac-dialog-decisions";
const RUN_LABEL = "mac-dialog-decisions";
const WATCH_RETRY_MS = 2_000;
const DAEMON_PID_FILE = "daemon.json";
const RESPONDER = "mac-dialog";
const DEFAULT_LATER_MS = 10 * 60_000;
// Resending an answer a saved machine could not take yet (Tailscale SSH
// re-approval): every 15 s for up to 30 minutes, until the decision resolves.
const ANSWER_RETRY_MS = 15_000;
const ANSWER_RETRY_LIMIT = 120;

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
    // あとで: how long a dismissed dialog stays hidden before it comes back.
    laterMs: pickNumber(
      env.MAC_DIALOG_LATER_MS,
      file.later_minutes !== undefined ? Number(file.later_minutes) * 60_000 : undefined,
      DEFAULT_LATER_MS,
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

function durationLabel(ms) {
  if (ms >= 60_000) return `${Math.round(ms / 60_000)}分`;
  return `${Math.max(1, Math.round(ms / 1000))}秒`;
}

function sourceLabel(decision) {
  const origin = decision.origin || {};
  if (origin.agent) return `🤖 ${origin.agent}`;
  const kinds = {
    command_guard: "🛡 危険コマンドの承認",
    tool_permission: "🔐 ツールの許可",
    agent_prompt: "💬 エージェントの質問",
    ask: "❓ 確認",
  };
  return kinds[decision.kind] || "";
}

// プロジェクト（スペース） › タブ › ペイン — what the user scans first.
function locationLabel(decision) {
  const origin = decision.origin || {};
  const parts = [];
  if (origin.workspace_label) parts.push(origin.workspace_label);
  else if (origin.cwd) parts.push(path.basename(origin.cwd));
  if (origin.tab_label) parts.push(origin.tab_label);
  if (origin.pane_label) parts.push(origin.pane_label);
  return parts.join(" › ");
}

function projectName(decision) {
  const origin = decision.origin || {};
  if (origin.workspace_label) return origin.workspace_label;
  if (origin.cwd) return path.basename(origin.cwd);
  return "";
}

function buildDialogSpec(decision, machine, queueRemaining = 0, options = {}) {
  const origin = decision.origin || {};
  const details = [];
  if (origin.command) {
    details.push({ label: "コマンド", value: String(origin.command), monospace: true });
  }
  if (origin.cwd) details.push({ label: "場所", value: String(origin.cwd), monospace: true });
  if (origin.branch) details.push({ label: "ブランチ", value: String(origin.branch), monospace: true });
  if (origin.reason && origin.reason !== "screen_prompt") {
    details.push({ label: "理由", value: String(origin.reason) });
  }
  if (origin.pane_id) details.push({ label: "ペイン", value: String(origin.pane_id), monospace: true });

  const machineId = machine?.id || origin.machine || "local";
  const remote = machineId !== "local";
  const project = projectName(decision);
  return {
    window_title: project ? `herdr 判断待ち — ${project}` : "herdr 判断待ち",
    source: sourceLabel(decision),
    location: locationLabel(decision),
    machine: remote ? String(machine?.label || machineId) : "",
    question_label: decision.question_label || "",
    title: String(decision.title || decision.decision_id || ""),
    body: (() => {
      const body = String(decision.body || "");
      const progress = String(decision.question_label || "");
      if (!/^質問 \d+\/\d+(?: |$)/u.test(progress)) return body;
      const at = body.indexOf(progress);
      if (at < 0 || (at > 0 && !/\s/u.test(body[at - 1])) ||
          (at + progress.length < body.length && !/\s/u.test(body[at + progress.length]))) return body;
      return (body.slice(0, at) + body.slice(at + progress.length)).trim();
    })(),
    details,
    options: (Array.isArray(decision.options) ? decision.options : []).map(
      (option) => ({
        id: String(option.id),
        label: String(option.label || option.id),
        description: optionDescription(option),
        preview: optionPreview(option),
        preview_unavailable: option?.preview_unavailable === true,
      }),
    ),
    multi_select: decision.multi_select === true,
    allow_text: decision.allow_text === true,
    deadline: decision.expires_unix_ms ? `期限 ${fmtDeadline(decision)}` : "",
    queue_remaining: queueRemaining,
    later_label: durationLabel(options.laterMs ?? DEFAULT_LATER_MS),
    daemon_pid: options.daemonPid ?? process.pid,
    decision_id: String(decision.decision_id || ""),
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
    onStateChange = () => {},
  }) {
    this.config = config;
    this.spawnWatch = spawnWatch;
    this.runAnswer = runAnswer;
    this.spawnDialog = spawnDialog;
    this.specFilePath = specFilePath;
    this.log = logFn;
    this.onStateChange = onStateChange;
    this.entries = new Map(); // key -> entry
    this.queue = []; // keys waiting for the dialog
    this.showing = new Map(); // machine + decision -> open window
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
    for (const entry of this.entries.values()) {
      clearTimeout(entry.snoozeTimer);
      clearTimeout(entry.answerRetryTimer);
      this._closeDialog(entry);
    }
    this.child?.kill();
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
    this._onWatchLine(line);
    this.onStateChange();
  }

  _onWatchLine(line) {
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
      // crashed. A dialog put off with あとで comes back on its own timer.
      existing.machineDown = false;
      existing.decision = decision;
      if (
        !existing.showing &&
        !existing.queued &&
        !existing.dismissed &&
        !existing.answerPending &&
        !existing.answerRetryTimer &&
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
      clearTimeout(entry.snoozeTimer);
      clearTimeout(entry.answerRetryTimer);
      this.queue = this.queue.filter((k) => k !== key);
      if (entry.showing) this._closeDialog(entry);
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
      clearTimeout(entry.snoozeTimer);
      this.queue = this.queue.filter((k) => k !== entry.key);
      if (entry.showing) this._closeDialog(entry);
      // A submitted answer still owns its text and retry timer while offline.
      if (!entry.answerPending && !entry.answerRetryTimer) this.entries.delete(entry.key);
    }
    this.log(`マシン切断: ${mid} (${reason})`);
  }

  // ---- dialog presentation ----------------------------------------------------

  pump() {
    if (this.stopped) return;
    while (this.queue.length > 0) {
      const key = this.queue.shift();
      const entry = this.entries.get(key);
      if (!entry || entry.resolved || entry.dismissed || entry.machineDown) {
        continue;
      }
      entry.queued = false;
      this._present(entry);
    }
  }

  _present(entry) {
    const spec = buildDialogSpec(entry.decision, entry.machine, this.queue.length, {
      laterMs: this.config.laterMs,
    });
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
    if (!this.child) {
      const child = this.spawnDialog();
      this.child = child;
      this._consumeChild(child);
    }
    entry.specPath = specPath;
    entry.showing = true;
    this.showing.set(entry.key, entry);
    this.child.send({ command: "open", key: entry.key, spec_path: specPath });
    this.onStateChange();
    this.log(`ダイアログを表示: ${entry.decision.decision_id}`);
  }

  async _consumeChild(child) {
    try {
      for await (const line of child.lines) {
        if (this.stopped || this.child !== child) continue;
        let message;
        try {
          message = JSON.parse(line);
        } catch {
          this.log(`ダイアログの JSON でない行を捨てました: ${String(line).slice(0, 120)}`);
          continue;
        }
        const entry = this.showing.get(message.key);
        if (!entry || typeof message.result !== "string") continue;
        this._closeDialog(entry);
        if (message.result === "answer") {
          const ids = Array.isArray(message.option_ids)
            ? message.option_ids.filter((id) => typeof id === "string" && id) : [];
          const text = typeof message.text === "string" ? message.text.trim() : "";
          if (!entry.resolved && (ids.length > 0 || text)) {
            // Do not block other windows on a saved machine's SSH timeout.
            void this.submitAnswer(entry, { optionIds: ids, text });
          }
        } else if (["later", "dismiss", "closed"].includes(message.result)) {
          this.snooze(entry);
        }
      }
    } catch (error) {
      this.log(`ダイアログプロセスの読み取りに失敗: ${error.message}`);
    }
    if (this.child !== child) return;
    this.child = null;
    for (const entry of [...this.showing.values()]) {
      this._closeDialog(entry);
      if (!this.stopped && !entry.resolved && !entry.dismissed && !entry.answerPending) {
        entry.queued = true;
        this.queue.push(entry.key);
      }
    }
    this.schedulePump();
  }

  // あとで: hide now, show again after laterMs if nobody has answered by then.
  snooze(entry) {
    entry.dismissed = true;
    const laterMs = this.config.laterMs ?? DEFAULT_LATER_MS;
    this.log(
      `あとで: ${entry.decision.decision_id} を ${Math.round(laterMs / 60_000)} 分後にもう一度出します`,
    );
    clearTimeout(entry.snoozeTimer);
    entry.snoozeTimer = setTimeout(() => {
      entry.snoozeTimer = null;
      if (this.stopped || entry.resolved || entry.machineDown) return;
      if (this.entries.get(entry.key) !== entry) return;
      entry.dismissed = false;
      if (!entry.queued && !entry.showing) {
        entry.queued = true;
        this.queue.push(entry.key);
        this.schedulePump();
      }
    }, laterMs);
    entry.snoozeTimer.unref?.();
  }

  _closeDialog(entry) {
    if (!entry.showing) return;
    this.showing.delete(entry.key);
    entry.showing = false;
    this.onStateChange();
    this.child?.send({ command: "close", key: entry.key });
    for (const file of [entry.specPath, `${entry.specPath}.result.jsonl`]) {
      try { fs.unlinkSync(file); } catch {}
    }
  }

  // ---- answering ---------------------------------------------------------------

  async submitAnswer(entry, { optionIds = [], text = "" }) {
    if (this.stopped || entry.resolved || entry.answerPending) return;
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
    if (this.stopped || entry.resolved) return;
    if (result.code === 0) return; // the resolved event closes/removes the entry
    entry.answerPending = false;
    const combined = `${result.stdout}\n${result.stderr}`;
    if (combined.includes("decision_already_resolved")) {
      this.log(`ほかの窓口で決定済みでした: ${did}`);
      return;
    }
    const approvalUrl = combined.match(/https:\/\/login\.tailscale\.com\/\S+/)?.[0];
    if (approvalUrl) entry.answerApprovalUrl = approvalUrl;
    if (entry.answerApprovalUrl && !entry.resolved) {
      // The saved machine wants a Tailscale SSH re-approval for a new
      // connection: keep the answer and resend it until it lands or the
      // decision resolves elsewhere.
      entry.answerRetries = (entry.answerRetries || 0) + 1;
      this.log(`回答を送れません (${did}): Tailscale の承認待ち ${entry.answerApprovalUrl}（${entry.answerRetries}回目）`);
      if (entry.answerRetries < ANSWER_RETRY_LIMIT) {
        clearTimeout(entry.answerRetryTimer);
        entry.answerRetryTimer = setTimeout(() => {
          if (!entry.resolved) void this.submitAnswer(entry, { optionIds, text });
        }, this.config.answerRetryMs ?? ANSWER_RETRY_MS);
        entry.answerRetryTimer.unref?.();
      }
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

// Runs the dialog as a plain child process: the compiled native panel, or a
// MAC_DIALOG_HELPER_CMD stand-in in tests. It prints JSON lines on stdout; the
// first {"result": ...} line is the user's answer and {"event":"shown"} marks
// that the window is on screen.
function spawnHelperDialog(config, logFn = () => {}) {
  const argv = config.helperCmd;
  const child = spawn(argv[0], [...argv.slice(1), "--stream"], {
    stdio: ["pipe", "pipe", "pipe"],
  });
  child.stdin.on("error", (error) => logFn(`dialog stdin: ${error.message}`));
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
    send(message) {
      if (!child.stdin.destroyed) child.stdin.write(`${JSON.stringify(message)}\n`);
    },
    kill() {
      try {
        child.kill("SIGTERM");
      } catch {}
      resolveExit?.(0);
    },
  };
}

// The native panel (dialog.swift) is compiled once per source version into
// the plugin state dir and run directly, so its stdout carries the result and
// a signal closes it. swiftc must use Xcode's default toolchain: a
// DEVELOPER_DIR pointing at CommandLineTools makes the SDK and compiler
// disagree and the build fails.
function nativeDialogPath(config) {
  const source = path.join(pluginRootDir(), "dialog.swift");
  const hash = createHash("sha256")
    .update(fs.readFileSync(source))
    .digest("hex")
    .slice(0, 12);
  return path.join(config.stateDir, "bin", `herdr-decision-dialog-${hash}.app`, "Contents", "MacOS", "herdr-decision-dialog");
}

function ensureNativeDialog(config, logFn = () => {}, root = pluginRootDir()) {
  if (fs.existsSync(path.join(root, "VERSION"))) {
    const binary = path.join(root, "herdr-decision-dialog.app", "Contents", "MacOS", "herdr-decision-dialog");
    if ((!fs.existsSync(path.join(root, "PLATFORM")) || fs.readFileSync(path.join(root, "PLATFORM"), "utf8").trim() !== "darwin-arm64") || !fs.existsSync(binary)) {
      logFn("配布版の同梱アプリが欠けているか、対応するマシン用ではありません。herdr mac-dialog install で入れ直してください");
      return null;
    }
    const verified = spawnSync("/usr/bin/codesign", ["--verify", "--deep", "--strict", "--test-requirement", MAC_DIALOG_SIGNATURE_REQUIREMENT, path.join(root, "herdr-decision-dialog.app")], { encoding: "utf8" });
    if (verified.error || verified.status !== 0) {
      logFn(`同梱アプリの署名が無効、改変済み、または開発者 ID が一致しません: ${verified.stderr || verified.error?.message || verified.status}`);
      return null;
    }
    logFn(`署名済み配布版の同梱アプリを使用: ${binary}`);
    return binary;
  }
  logFn("開発版: リポジトリの dialog.swift を使用します");
  const source = path.join(root, "dialog.swift");
  const binary = nativeDialogPath(config);
  if (fs.existsSync(binary)) return binary;
  fs.mkdirSync(path.dirname(binary), { recursive: true });
  const tmp = `${binary}.tmp-${process.pid}`;
  const env = { ...process.env };
  delete env.DEVELOPER_DIR;
  logFn("ダイアログをビルドします (swiftc、初回は1〜2分かかります)");
  const res = spawnSync("swiftc", ["-O", source, "-o", tmp], {
    encoding: "utf8",
    env,
    timeout: 600_000,
  });
  if (res.error || res.status !== 0) {
    logFn(
      `ダイアログのビルドに失敗: ${(res.stderr || res.error?.message || String(res.status)).slice(0, 2000)}`,
    );
    try {
      fs.rmSync(tmp, { force: true });
    } catch {}
    return null;
  }
  fs.renameSync(tmp, binary);
  fs.writeFileSync(path.join(path.dirname(path.dirname(binary)), "Info.plist"), `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>herdr-decision-dialog</string>
<key>CFBundleIdentifier</key><string>dev.herdr.mac-dialog-decisions</string>
<key>CFBundleName</key><string>herdr 判断待ち</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>`);
  // older builds of the panel are no longer needed
  const bundle = path.dirname(path.dirname(path.dirname(binary)));
  for (const name of fs.readdirSync(path.dirname(bundle))) {
    const full = path.join(path.dirname(bundle), name);
    if (full !== bundle && name.startsWith("herdr-decision-dialog-")) {
      try {
        fs.rmSync(full, { recursive: true, force: true });
      } catch {}
    }
  }
  logFn(`ダイアログをビルドしました: ${binary}`);
  return binary;
}

function killOrphanDialogs(config) {
  try {
    const orphans = execFileSync(
      "pgrep",
      ["-f", path.join(config.stateDir, "bin", "herdr-decision-dialog-")],
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

function pluginVersion(root = pluginRootDir()) {
  try { return fs.readFileSync(path.join(root, "VERSION"), "utf8").trim(); }
  catch { return "development"; }
}
function writeDaemonPid(stateDir, fields = {}) {
  fs.mkdirSync(stateDir, { recursive: true });
  const temporary = `${daemonPidPath(stateDir)}.${process.pid}.tmp`;
  fs.writeFileSync(temporary, JSON.stringify({ pid: process.pid, plugin_root: pluginRootDir(), version: pluginVersion(), ...fields }), { mode: 0o600 });
  fs.renameSync(temporary, daemonPidPath(stateDir));
}
function daemonStatus(config) {
  let state = {};
  try { state = JSON.parse(fs.readFileSync(daemonPidPath(config.stateDir), "utf8")); } catch {}
  return { ...state, running: daemonRunning(config.stateDir), restart_requested: fs.existsSync(path.join(config.stateDir, "restart-request.json")) };
}
function requestRestart(config) {
  const file = path.join(config.stateDir, "restart-request.json");
  const temporary = `${file}.${process.pid}.tmp`;
  fs.writeFileSync(temporary, JSON.stringify({ plugin_root: pluginRootDir() }), { mode: 0o600 });
  fs.renameSync(temporary, file);
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
      const state = daemonStatus(config);
      if (state.plugin_root !== pluginRootDir()) {
        requestRestart(config);
        log(`登録版 ${pluginVersion()} と稼働版 ${state.version || "不明（旧常駐役）"} が異なります。書きかけ・あとで・回答再送を保ち、安全に切り替えられるまで待ちます`);
      } else {
        fs.rmSync(path.join(config.stateDir, "restart-request.json"), { force: true });
        log(`デーモンは起動済み (pid ${state.pid}, version ${state.version})`);
      }
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
      throw new Error(`herdr run に失敗: ${(result.stderr || result.stdout).trim()}`);
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

  if (mode === "status") {
    process.stdout.write(JSON.stringify(daemonStatus(config)) + "\n");
    return;
  }
  if (mode === "ensure") {
    if (!config.helperCmd && !ensureNativeDialog(config, log)) { process.exitCode = 1; return; }
    await ensureDaemon(config, process.env);
    return;
  }

  if (mode !== "run") {
    process.stderr.write("usage: mac-dialog-decisions.mjs [run|ensure|status]\n");
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
  killOrphanDialogs(config);

  writeDaemonPid(config.stateDir);
  const nativeDialog = config.helperCmd ? null : ensureNativeDialog(config, log);
  if (!config.helperCmd && !nativeDialog) {
    log("ダイアログを作れないため終了します（上のビルドエラーを確認してください）");
    process.exit(1);
  }
  const daemon = new MacDialogDecisionsDaemon({
    config,
    spawnWatch: () => Promise.resolve(spawnDecisionWatch(config, log)),
    runAnswer: createRunAnswer(config, log),
    spawnDialog: () =>
      spawnHelperDialog(
        { ...config, helperCmd: config.helperCmd || [nativeDialog] },
        log,
      ),
    specFilePath: (key) =>
      path.join(specDir, `${key.replace(/[^A-Za-z0-9_.-]/g, "_")}.json`),
    log,
    onStateChange: () => {
      writeDaemonPid(config.stateDir, { open_windows: daemon.showing.size, pending_entries: daemon.entries.size });
      void maybeHandoff();
    },
  });
  let handingOff = false;
  const restartPath = path.join(config.stateDir, "restart-request.json");
  async function maybeHandoff() {
    // A hidden あとで or an in-flight answer still owns state. Keep the
    // current process until it has nothing to lose, including retry timers.
    if (handingOff || daemon.stopped || daemon.entries.size !== 0 || daemon.showing.size !== 0) return;
    let request;
    try { request = JSON.parse(fs.readFileSync(restartPath, "utf8")); } catch { return; }
    if (typeof request.plugin_root !== "string" || request.plugin_root === pluginRootDir()) return;
    const target = path.join(request.plugin_root, "mac-dialog-decisions.mjs");
    if (!fs.existsSync(target)) { log("新版の常駐役が欠けています。現在の版を保持します"); return; }
    if (!config.helperCmd && !ensureNativeDialog(config, log, request.plugin_root)) {
      log("新版のアプリ検証に失敗しました。現在の版を保持します"); return;
    }
    const herdrBin = resolveHerdrBin(config, log);
    if (!herdrBin) return;
    const listed = await execFileText(herdrBin, ["plugin", "list", "--json", "--plugin", PLUGIN_ID]);
    let registered;
    try { registered = JSON.parse(listed.stdout).result.plugins.find(plugin => plugin.plugin_id === PLUGIN_ID); } catch { return; }
    if (listed.code !== 0) return;
    // ensure may have cancelled or replaced the request while the registry
    // was being read. Never apply that superseded request.
    let currentRequest;
    try { currentRequest = JSON.parse(fs.readFileSync(restartPath, "utf8")); } catch { return; }
    if (currentRequest.plugin_root !== request.plugin_root) return;
    if (registered?.plugin_root !== request.plugin_root) {
      fs.rmSync(restartPath, { force: true });
      log("登録先と一致しない古い切替要求を取り消しました");
      return;
    }
    if (handingOff || daemon.stopped || daemon.entries.size !== 0 || daemon.showing.size !== 0) return;
    handingOff = true;
    await daemon.stop();
    restartWatcher.close();
    if (readDaemonPid(config.stateDir) === process.pid) fs.rmSync(daemonPidPath(config.stateDir), { force: true });
    fs.rmSync(path.join(config.stateDir, "starting.json"), { force: true });
    fs.rmSync(restartPath, { force: true });
    log(`安全な切替: ${pluginRootDir()} → ${request.plugin_root}`);
    const result = await execFileText(process.execPath, [target, "ensure"], { env: process.env });
    if (result.stdout) process.stdout.write(result.stdout);
    if (result.stderr) process.stderr.write(result.stderr);
    process.exit(result.code);
  }
  const restartWatcher = fs.watch(config.stateDir, (_, filename) => {
    if (filename === "restart-request.json") void maybeHandoff();
  });
  await daemon.start();
  writeDaemonPid(config.stateDir, { open_windows: 0, pending_entries: 0 });
  void maybeHandoff();
  log("watch 開始 (herdr decision watch --all-machines)");

  const shutdown = () => {
    restartWatcher.close();
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
  createRunAnswer,
  decisionKey,
  fmtDeadline,
  loadConfig,
  locationLabel,
  resolveHerdrBin,
  spawnHelperDialog,
  ensureNativeDialog,
};
