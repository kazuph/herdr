#!/usr/bin/env node
// herdr plugin daemon: mirror pending decisions to Slack when the Sabera
// glasses are not answering, and turn Slack answers (number reactions or
// thread replies) into `herdr decision answer --responder slack`.
//
// Slack sends only ever happen on kazuph's machine, and only when the
// slack-ask credentials resolve. On any other machine, or without
// credentials, this exits quietly after logging the reason.

import { execFile, execFileSync, spawn } from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import process from "node:process";
import readline from "node:readline";
import { fileURLToPath } from "node:url";

const PLUGIN_ID = "kazuph.slack-decisions";
const RUN_LABEL = "slack-decisions";
const DEFAULT_ALLOWED_USER = "kazuph";
const DEFAULT_POST_DELAY_MS = 180_000; // glasses connected: wait before posting
const DEFAULT_POLL_INTERVAL_MS = 5_000;
const SABERA_TIMEOUT_MS = 3_000;
const WATCH_RETRY_MS = 2_000;
const SOCKET_RECONNECT_MS = 1_000;
const RECONCILE_DELAY_MS = 3_000;
const STALE_POST_MAX_AGE_MS = 7 * 24 * 60 * 60 * 1000;
const POST_RETRY_MS = 60_000;
const NUMBER_EMOJIS = [
  "one", "two", "three", "four", "five", "six", "seven", "eight", "nine",
];
const SLACK_GET_METHODS = new Set([
  "conversations.replies",
  "conversations.history",
  "conversations.info",
  "reactions.get",
  "users.info",
]);
const DAEMON_PID_FILE = "daemon.json";
const POSTS_FILE = "posts.json";

function log(...args) {
  const at = new Date().toTimeString().slice(0, 8);
  process.stderr.write(`[slack-decisions ${at}] ${args.join(" ")}\n`);
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function homePath(...parts) {
  return path.join(os.homedir(), ...parts);
}

function parseEnvFile(filePath) {
  let content;
  try {
    content = fs.readFileSync(filePath, "utf8");
  } catch {
    return {};
  }
  const env = {};
  for (const rawLine of content.split("\n")) {
    const line = rawLine.trim();
    if (!line || line.startsWith("#") || !line.includes("=")) continue;
    const [key, ...rest] = line.split("=");
    env[key.trim()] = rest.join("=").trim().replace(/^["']|["']$/g, "");
  }
  return env;
}

function defaultCredentialEnvFiles() {
  // Same files slack-ask's get-credentials.sh reads.
  return [
    homePath("dotfiles", "claude", "hooks", ".env"),
    homePath(".claude", "hooks", ".env"),
    homePath(".config", "claude-slack", "credentials"),
  ];
}

function execFileTextSync(file, args) {
  try {
    return execFileSync(file, args, {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    }).trim();
  } catch {
    return "";
  }
}

function keychainValue(service) {
  // Same store slack-ask reads: security -a claude-slack -s <key>.
  return execFileTextSync(
    "security",
    ["find-generic-password", "-w", "-a", "claude-slack", "-s", service],
  );
}

function passValue(entry) {
  return execFileTextSync("pass", ["show", entry]).split("\n")[0].trim();
}

function loadConfig(env = process.env) {
  const configDir =
    env.HERDR_PLUGIN_CONFIG_DIR ||
    homePath(".config", "herdr", "plugins", "config", PLUGIN_ID);
  const configPath =
    env.SLACK_DECISIONS_CONFIG || path.join(configDir, "config.json");
  let file = {};
  try {
    file = JSON.parse(fs.readFileSync(configPath, "utf8"));
  } catch {
    file = {};
  }
  const pickNumber = (envValue, fileValue, fallback) => {
    const parsed = Number(envValue ?? fileValue);
    return Number.isFinite(parsed) && parsed >= 0 ? parsed : fallback;
  };
  const stateDir =
    env.SLACK_DECISIONS_STATE_DIR ||
    env.HERDR_PLUGIN_STATE_DIR ||
    homePath(".local", "state", "herdr", "plugins", PLUGIN_ID);
  return {
    enabled:
      (env.SLACK_DECISIONS_ENABLED ?? file.enabled ?? true) !== false &&
      env.SLACK_DECISIONS_ENABLED !== "0",
    allowedOsUser:
      env.SLACK_DECISIONS_ALLOWED_USER ||
      file.allowed_os_user ||
      DEFAULT_ALLOWED_USER,
    osUser: env.SLACK_DECISIONS_OS_USER || "",
    postDelayMs: pickNumber(
      env.SLACK_DECISIONS_POST_DELAY_MS,
      file.post_delay_ms,
      DEFAULT_POST_DELAY_MS,
    ),
    pollIntervalMs: pickNumber(
      env.SLACK_DECISIONS_POLL_INTERVAL_MS,
      file.poll_interval_ms,
      DEFAULT_POLL_INTERVAL_MS,
    ),
    saberaSocket:
      env.SLACK_DECISIONS_SABERA_SOCK ||
      file.sabera_socket ||
      homePath(".sabera-relay.sock"),
    apiBase:
      env.SLACK_DECISIONS_API_BASE ||
      file.slack_api_base ||
      "https://slack.com/api",
    slackChannel: env.SLACK_DECISIONS_CHANNEL || file.slack_channel || "",
    mention: env.SLACK_DECISIONS_MENTION ?? file.mention ?? "",
    socketMode:
      (env.SLACK_DECISIONS_SOCKET_MODE ?? file.socket_mode ?? true) !== false &&
      env.SLACK_DECISIONS_SOCKET_MODE !== "0",
    credentialEnvFiles:
      Array.isArray(file.credentials_env_files) &&
      file.credentials_env_files.length > 0
        ? file.credentials_env_files.map((p) => p.replace(/^~/, os.homedir()))
        : defaultCredentialEnvFiles(),
    noKeychain:
      env.SLACK_DECISIONS_NO_KEYCHAIN === "1" || file.no_keychain === true,
    noPass: env.SLACK_DECISIONS_NO_PASS === "1" || file.no_pass === true,
    herdrBin: env.HERDR_BIN_PATH || file.herdr_bin || "herdr",
    stateDir,
    configDir,
    configPath,
  };
}

function loadCredentials(config, env = process.env) {
  let token = env.SLACK_BOT_TOKEN || "";
  let channel = env.SLACK_CHANNEL || env.SLACK_CHANNEL_ID || "";
  let appToken = env.SLACK_APP_TOKEN || "";
  let mention = config.mention || "";

  for (const filePath of config.credentialEnvFiles) {
    const parsed = parseEnvFile(filePath);
    token ||= parsed.SLACK_BOT_TOKEN || "";
    channel ||= parsed.SLACK_CHANNEL || parsed.SLACK_CHANNEL_ID || "";
    appToken ||= parsed.SLACK_APP_TOKEN || "";
    mention ||=
      parsed.SLACK_APPROVAL_MENTION || parsed.SLACK_DECISIONS_MENTION || "";
  }

  if (!config.noKeychain) {
    token ||= keychainValue("SLACK_BOT_TOKEN");
    channel ||=
      keychainValue("SLACK_CHANNEL") || keychainValue("SLACK_CHANNEL_ID");
    appToken ||= keychainValue("SLACK_APP_TOKEN");
  }
  if (!config.noPass) {
    token ||= passValue("claude/slack-bot-token");
    channel ||= passValue("claude/slack-channel");
    appToken ||= passValue("claude/slack-app-token");
  }

  if (config.slackChannel) channel = config.slackChannel;
  return { token, channel, appToken, mention };
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

// --- Slack API client -------------------------------------------------------

class SlackClient {
  constructor({ token, appToken, channel, apiBase }) {
    this.token = token;
    this.appToken = appToken;
    this.channel = channel;
    this.apiBase = (apiBase || "https://slack.com/api").replace(/\/+$/, "");
    this.mention = "";
  }

  async api(method, params = {}) {
    const url = new URL(`${this.apiBase}/${method}`);
    const headers = { Authorization: `Bearer ${this.token}` };
    const init = { signal: AbortSignal.timeout(30_000) };
    if (SLACK_GET_METHODS.has(method)) {
      for (const [key, value] of Object.entries(params)) {
        url.searchParams.set(key, String(value));
      }
      init.method = "GET";
      init.headers = headers;
    } else {
      init.method = "POST";
      headers["Content-Type"] = "application/json; charset=utf-8";
      init.headers = headers;
      init.body = JSON.stringify(params);
    }
    const response = await fetch(url.toString(), init);
    const json = await response.json();
    if (!json.ok) {
      const error = new Error(json.error || "unknown_slack_error");
      error.payload = json;
      throw error;
    }
    return json;
  }

  authTest() {
    return this.api("auth.test", {});
  }

  postMessage({ text, blocks }) {
    return this.api("chat.postMessage", { channel: this.channel, text, blocks });
  }

  updateMessage(ts, { text, blocks }) {
    return this.api("chat.update", { channel: this.channel, ts, text, blocks });
  }

  postThreadReply(threadTs, text) {
    return this.api("chat.postMessage", {
      channel: this.channel,
      thread_ts: threadTs,
      text,
    });
  }

  async addReactions(ts, names) {
    for (const name of names) {
      try {
        await this.api("reactions.add", {
          channel: this.channel,
          timestamp: ts,
          name,
        });
      } catch {
        // already reacted / propagation lag — safe to ignore
      }
    }
  }

  async message(ts) {
    const result = await this.api("conversations.history", {
      channel: this.channel,
      latest: ts,
      inclusive: true,
      limit: 1,
    });
    return result.messages?.[0] || null;
  }

  async replies(threadTs) {
    const result = await this.api("conversations.replies", {
      channel: this.channel,
      ts: threadTs,
      inclusive: true,
      limit: 100,
    });
    return result.messages || [];
  }

  async openSocketUrl() {
    const response = await fetch(`${this.apiBase}/apps.connections.open`, {
      method: "POST",
      headers: { Authorization: `Bearer ${this.appToken}` },
      signal: AbortSignal.timeout(30_000),
    });
    const json = await response.json();
    if (!json.ok || !json.url) {
      throw new Error(json.error || "socket_mode_open_failed");
    }
    return json.url;
  }
}

// --- Sabera relay (unix-socket WebSocket) -----------------------------------
// The saberante hub (`sabera.py serve`) owns ~/.sabera-relay.sock. A local
// request client sends {"type":"hello"} and the hub replies {"type":"link",
// ..., "glasses": "connected"|null}. Only the glasses field matters here.

function wsFrameText(text) {
  const payload = Buffer.from(text, "utf8");
  const mask = crypto.randomBytes(4);
  let header;
  if (payload.length < 126) {
    header = Buffer.from([0x81, 0x80 | payload.length]);
  } else if (payload.length < 65536) {
    header = Buffer.alloc(4);
    header[0] = 0x81;
    header[1] = 0x80 | 126;
    header.writeUInt16BE(payload.length, 2);
  } else {
    header = Buffer.alloc(10);
    header[0] = 0x81;
    header[1] = 0x80 | 127;
    header.writeBigUInt64BE(BigInt(payload.length), 2);
  }
  const masked = Buffer.alloc(payload.length);
  for (let i = 0; i < payload.length; i += 1) {
    masked[i] = payload[i] ^ mask[i % 4];
  }
  return Buffer.concat([header, mask, masked]);
}

// Incremental parser for server->client (unmasked) WebSocket frames.
function createWsParser(onText, onClose) {
  let buffer = Buffer.alloc(0);
  let fragments = [];
  return (chunk) => {
    buffer = Buffer.concat([buffer, chunk]);
    for (;;) {
      if (buffer.length < 2) return;
      const fin = (buffer[0] & 0x80) !== 0;
      const opcode = buffer[0] & 0x0f;
      const masked = (buffer[1] & 0x80) !== 0;
      let length = buffer[1] & 0x7f;
      let offset = 2;
      if (length === 126) {
        if (buffer.length < 4) return;
        length = buffer.readUInt16BE(2);
        offset = 4;
      } else if (length === 127) {
        if (buffer.length < 10) return;
        length = Number(buffer.readBigUInt64BE(2));
        offset = 10;
      }
      const maskLen = masked ? 4 : 0;
      if (buffer.length < offset + maskLen + length) return;
      let payload = buffer.subarray(offset + maskLen, offset + maskLen + length);
      if (masked) {
        const mask = buffer.subarray(offset, offset + 4);
        const unmasked = Buffer.alloc(payload.length);
        for (let i = 0; i < payload.length; i += 1) {
          unmasked[i] = payload[i] ^ mask[i % 4];
        }
        payload = unmasked;
      }
      buffer = buffer.subarray(offset + maskLen + length);
      if (opcode === 0x8) {
        onClose?.();
        return;
      }
      if (opcode === 0x9) continue; // ping: no reply needed for a 3s probe
      if (opcode === 0x1 || opcode === 0x0) {
        fragments.push(payload);
        if (fin) {
          const text = Buffer.concat(fragments).toString("utf8");
          fragments = [];
          onText(text);
        }
      }
    }
  };
}

function wsUnixRequest(sockPath, sendObj, timeoutMs = SABERA_TIMEOUT_MS) {
  return new Promise((resolve) => {
    let settled = false;
    const socket = net.connect({ path: sockPath });
    const timer = setTimeout(() => done(null), timeoutMs);
    const done = (value) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      socket.destroy();
      resolve(value);
    };
    let handshakeDone = false;
    let handshake = Buffer.alloc(0);
    const parser = createWsParser(
      (text) => {
        try {
          done(JSON.parse(text));
        } catch {
          done(null);
        }
      },
      () => done(null),
    );
    socket.on("connect", () => {
      const key = crypto.randomBytes(16).toString("base64");
      socket.write(
        "GET / HTTP/1.1\r\n" +
          "Host: localhost\r\n" +
          "Upgrade: websocket\r\n" +
          "Connection: Upgrade\r\n" +
          `Sec-WebSocket-Key: ${key}\r\n` +
          "Sec-WebSocket-Version: 13\r\n\r\n",
      );
    });
    socket.on("data", (chunk) => {
      if (!handshakeDone) {
        handshake = Buffer.concat([handshake, chunk]);
        const marker = handshake.indexOf("\r\n\r\n");
        if (marker === -1) return;
        const head = handshake.subarray(0, marker).toString("utf8");
        if (!/^HTTP\/1\.1 101/i.test(head)) {
          done(null);
          return;
        }
        handshakeDone = true;
        socket.write(wsFrameText(JSON.stringify(sendObj)));
        const rest = handshake.subarray(marker + 4);
        if (rest.length > 0) parser(rest);
        return;
      }
      parser(chunk);
    });
    socket.on("error", () => done(null));
    socket.on("close", () => done(null));
  });
}

// "connected" / other status string, or null when the hub socket is missing
// or does not answer — treated as "glasses not connected".
async function saberaGlassesState(sockPath, timeoutMs = SABERA_TIMEOUT_MS) {
  try {
    fs.statSync(sockPath);
  } catch {
    return null;
  }
  const reply = await wsUnixRequest(sockPath, { type: "hello" }, timeoutMs);
  if (!reply || typeof reply !== "object") return null;
  return typeof reply.glasses === "string" ? reply.glasses : null;
}

// --- Slack message rendering -------------------------------------------------

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

function originLines(decision) {
  const origin = decision.origin || {};
  const lines = [];
  if (origin.pane_id) lines.push(`*ペイン* \`${origin.pane_id}\``);
  if (origin.agent) lines.push(`*エージェント* \`${origin.agent}\``);
  if (origin.cwd) lines.push(`*ディレクトリ* \`${origin.cwd}\``);
  if (origin.branch) lines.push(`*ブランチ* \`${origin.branch}\``);
  if (origin.command) lines.push(`*コマンド* \`${origin.command}\``);
  if (origin.reason) lines.push(`*理由* ${origin.reason}`);
  if (origin.machine) lines.push(`*出どころマシン* ${origin.machine}`);
  return lines;
}

const STATUS_TEXT = {
  pending: ":hourglass_flowing_sand: 回答待ち",
  answered: ":white_check_mark: 決定済み",
  expired: ":hourglass: 期限切れ",
  cancelled: ":no_entry_sign: 取り消し",
  machine_down: ":warning: マシン切断",
};

function buildDecisionBlocks({
  decision,
  machine,
  status = "pending",
  mention = "",
  now = Date.now(),
}) {
  const statusText = STATUS_TEXT[status] || status;
  const machineLabel = machine?.label || machine?.id || "local";
  const head = mention
    ? `【${statusText}】判断待ち | ${machineLabel} | ${mention}`
    : `【${statusText}】判断待ち | ${machineLabel}`;
  const blocks = [
    { type: "context", elements: [{ type: "mrkdwn", text: head }] },
    {
      type: "section",
      text: { type: "mrkdwn", text: `*${decision.title || decision.decision_id}*` },
    },
  ];
  if (decision.body) {
    blocks.push({
      type: "section",
      text: { type: "mrkdwn", text: decision.body },
    });
  }
  const origins = originLines(decision);
  if (origins.length > 0) {
    blocks.push({
      type: "section",
      text: { type: "mrkdwn", text: origins.join("\n") },
    });
  }

  if (status === "pending") {
    const options = Array.isArray(decision.options) ? decision.options : [];
    if (options.length > 0) {
      blocks.push({ type: "divider" });
      const lines = options
        .map((option, index) => {
          const emoji = NUMBER_EMOJIS[index] || `${index + 1}番`;
          return `:${emoji}: ${option.label || option.id}`;
        })
        .join("\n");
      blocks.push({ type: "section", text: { type: "mrkdwn", text: lines } });
      const hint = decision.allow_text
        ? "数字リアクションで選択 or スレッド返信（文も答えに付きます）"
        : "数字リアクションで選択 or スレッドに番号で返信";
      blocks.push({
        type: "context",
        elements: [
          {
            type: "mrkdwn",
            text: `${hint}  |  ⏱️ 期限: ${fmtDeadline(decision, now)}  |  ${decision.decision_id}`,
          },
        ],
      });
    } else {
      blocks.push({
        type: "context",
        elements: [
          {
            type: "mrkdwn",
            text: `スレッドに返信してください  |  ⏱️ 期限: ${fmtDeadline(decision, now)}  |  ${decision.decision_id}`,
          },
        ],
      });
    }
    return blocks;
  }

  const answer = decision.answer || {};
  const option = (decision.options || []).find((o) => o.id === answer.option_id);
  const what = option?.label || answer.option_id || answer.text || "";
  const who = answer.responder || "";
  let resultText;
  if (status === "answered") {
    resultText = `*結果* ${what || "（空）"}${who ? `\n*回答者* ${who}` : ""}`;
  } else if (status === "expired") {
    resultText = "期限切れになりました";
  } else if (status === "cancelled") {
    resultText = "取り消されました";
  } else if (status === "machine_down") {
    resultText = "マシンとの接続が切れました（回答を送れません）";
  } else {
    resultText = `状態: ${status}`;
  }
  blocks.push({ type: "divider" });
  blocks.push({ type: "section", text: { type: "mrkdwn", text: resultText } });
  return blocks;
}

function fallbackText({ decision, machine }) {
  const options = (decision.options || [])
    .map((option, index) => `${index + 1}. ${option.label || option.id}`)
    .join("\n");
  return [
    `[${machine?.label || "local"}] ${decision.title || decision.decision_id}`,
    decision.body || "",
    options,
  ]
    .filter(Boolean)
    .join("\n");
}

// --- Daemon -------------------------------------------------------------------

function decisionKey(machineId, decisionId) {
  return `${machineId} ${decisionId}`;
}

function decisionIdOf(decision) {
  return String(decision?.decision_id || "");
}

export class SlackDecisionsDaemon {
  constructor({
    config,
    slack,
    botUserId = "",
    spawnWatch, // () => {lines: AsyncIterable<object>, close()}
    runAnswer, // (argv: string[]) => Promise<{code, stdout, stderr}>
    getDecision, // (machineId, decisionId) => Promise<decision|null>
    glassesState, // () => Promise<string|null>
    log: logFn = log,
    postsFilePath = null,
  }) {
    this.config = config;
    this.slack = slack;
    this.botUserId = botUserId;
    this.spawnWatch = spawnWatch;
    this.runAnswer = runAnswer;
    this.getDecision = getDecision;
    this.glassesState = glassesState;
    this.log = logFn;
    this.postsFilePath = postsFilePath;
    this.entries = new Map(); // key -> entry
    this.machinesDown = new Set();
    this.posts = new Map(); // key -> {channel, ts, machineId, decisionId}
    this.stopped = false;
    this.pollTimer = null;
    this.socketAbort = new AbortController();
    this._watchHandle = null;
    this._watchDone = null;
    this._socketDone = null;
  }

  loadPostsFile() {
    if (!this.postsFilePath) return;
    try {
      const raw = JSON.parse(fs.readFileSync(this.postsFilePath, "utf8"));
      const now = Date.now();
      for (const [key, value] of Object.entries(raw || {})) {
        if (!value || typeof value.ts !== "string" || !value.ts) continue;
        if (now - (value.saved_at || 0) > STALE_POST_MAX_AGE_MS) continue;
        this.posts.set(key, {
          channel: value.channel,
          ts: value.ts,
          machineId: value.machine_id,
          decisionId: value.decision_id,
        });
      }
      if (this.posts.size > 0) {
        this.log(`Slack 投稿記録を復元: ${this.posts.size} 件`);
      }
    } catch {
      // no posts file yet
    }
  }

  savePostsFile() {
    if (!this.postsFilePath) return;
    try {
      fs.mkdirSync(path.dirname(this.postsFilePath), { recursive: true });
      const out = {};
      for (const [key, value] of this.posts) {
        out[key] = {
          channel: value.channel,
          ts: value.ts,
          machine_id: value.machineId,
          decision_id: value.decisionId,
          saved_at: Date.now(),
        };
      }
      fs.writeFileSync(this.postsFilePath, JSON.stringify(out, null, 2), {
        mode: 0o600,
      });
    } catch (error) {
      this.log(`posts ファイルの保存に失敗: ${error.message}`);
    }
  }

  async start() {
    this.loadPostsFile();
    this._watchDone = this.watchLoop();
    this._schedulePoll();
    if (this.config.socketMode && this.slack.appToken) {
      this._socketDone = this.socketLoop();
    } else if (!this.slack.appToken) {
      this.log(
        "SLACK_APP_TOKEN なし: Socket Mode なしで履歴ポーリングだけで回答を拾います",
      );
    }
  }

  async stop() {
    this.stopped = true;
    if (this.pollTimer) clearTimeout(this.pollTimer);
    for (const entry of this.entries.values()) {
      if (entry.postTimer) clearTimeout(entry.postTimer);
    }
    this.socketAbort.abort();
    this._watchHandle?.close?.();
    await Promise.allSettled([this._watchDone, this._socketDone]);
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
      void this.onMachineUp(mid);
      return;
    }
    if (event === "machine_down") {
      this.onMachineDown(mid, line?.reason || "");
      return;
    }
    if (event === "created" && did) {
      void this.onCreated(mid, machine, decision);
      return;
    }
    if (event === "resolved" && did) {
      void this.onResolved(mid, decision);
    }
  }

  async onCreated(mid, machine, decision) {
    const key = decisionKey(mid, decisionIdOf(decision));
    const existing = this.entries.get(key);
    if (existing) {
      // Reconnect replay. If it was marked machine-down, re-arm it now.
      if (existing.machineDown) {
        existing.machineDown = false;
        existing.decision = decision;
        if (existing.post) await this.renderPending(existing);
      }
      return;
    }
    if (decision.status && decision.status !== "pending") return;

    const entry = {
      key,
      machineId: mid,
      machine,
      decision,
      post: this.posts.get(key)
        ? { channel: this.posts.get(key).channel, ts: this.posts.get(key).ts }
        : null,
      machineDown: false,
      answerPending: false,
      resolved: false,
      postTimer: null,
      seenReplies: new Set(),
      seenReactions: new Set(),
    };
    this.entries.set(key, entry);
    if (entry.post) {
      this.log(`再起動後に追跡を再開: ${decision.decision_id}`);
      return;
    }
    if (this.machinesDown.has(mid)) {
      this.log(`${mid} は切断中: ${decision.decision_id} は投稿しません`);
      return;
    }

    const glasses = await this.glassesState();
    if (glasses === "connected") {
      this.log(
        `グラス接続中: ${decision.decision_id} は ${Math.round(this.config.postDelayMs / 1000)}秒たっても未回答なら投稿`,
      );
      entry.postTimer = setTimeout(() => {
        entry.postTimer = null;
        void this.postDecision(entry);
      }, this.config.postDelayMs);
    } else {
      this.log(`グラス未接続: ${decision.decision_id} をすぐ投稿します`);
      await this.postDecision(entry);
    }
  }

  async postDecision(entry) {
    if (entry.resolved || entry.post || this.stopped) return;
    if (this.machinesDown.has(entry.machineId)) return;
    if (entry.decision.status && entry.decision.status !== "pending") return;
    const { decision, machine } = entry;
    const blocks = buildDecisionBlocks({
      decision,
      machine,
      status: "pending",
      mention: this.slack.mention || "",
    });
    try {
      const posted = await this.slack.postMessage({
        text: fallbackText({ decision, machine }),
        blocks,
      });
      entry.post = { channel: this.slack.channel, ts: posted.ts };
      this.posts.set(entry.key, {
        channel: this.slack.channel,
        ts: posted.ts,
        machineId: entry.machineId,
        decisionId: decision.decision_id,
      });
      this.savePostsFile();
      const emojis = NUMBER_EMOJIS.slice(0, (decision.options || []).length);
      if (emojis.length > 0) {
        await this.slack.addReactions(posted.ts, emojis);
      }
      this.log(`Slack へ投稿: ${decision.decision_id} (${posted.ts})`);
    } catch (error) {
      this.log(`Slack 投稿に失敗 (${decision.decision_id}): ${error.message}`);
      entry.postTimer = setTimeout(() => {
        entry.postTimer = null;
        void this.postDecision(entry);
      }, POST_RETRY_MS);
    }
  }

  async renderPending(entry) {
    if (!entry.post || entry.resolved) return;
    try {
      await this.slack.updateMessage(entry.post.ts, {
        text: fallbackText(entry),
        blocks: buildDecisionBlocks({
          decision: entry.decision,
          machine: entry.machine,
          status: "pending",
          mention: this.slack.mention || "",
        }),
      });
    } catch (error) {
      this.log(`投稿の再表示に失敗 (${entry.decision.decision_id}): ${error.message}`);
    }
  }

  async updatePostStatus(entry, status, decision) {
    if (!entry.post) return;
    try {
      await this.slack.updateMessage(entry.post.ts, {
        text: `${(STATUS_TEXT[status] || status).replace(/:[a-z_0-9]+:/g, "").trim()}: ${decision.title || decision.decision_id}`,
        blocks: buildDecisionBlocks({
          decision,
          machine: entry.machine,
          status,
          mention: this.slack.mention || "",
        }),
      });
    } catch (error) {
      this.log(`投稿の更新に失敗 (${decision.decision_id}): ${error.message}`);
    }
  }

  async onResolved(mid, decision) {
    const key = decisionKey(mid, decisionIdOf(decision));
    const entry =
      this.entries.get(key) || {
        key,
        machineId: mid,
        machine: { id: mid, label: mid },
        decision,
        post: this.posts.get(key) || null,
        resolved: false,
        postTimer: null,
      };
    entry.resolved = true;
    if (entry.postTimer) clearTimeout(entry.postTimer);
    if (entry.post) {
      await this.updatePostStatus(entry, decision.status || "answered", decision);
      this.posts.delete(key);
      this.savePostsFile();
    }
    this.entries.delete(key);
    this.log(`決定済み: ${decision.decision_id} (${decision.status})`);
  }

  onMachineDown(mid, reason) {
    this.machinesDown.add(mid);
    for (const entry of [...this.entries.values()]) {
      if (entry.machineId !== mid) continue;
      if (!entry.post) {
        if (entry.postTimer) clearTimeout(entry.postTimer);
        this.entries.delete(entry.key);
        continue; // unposted items of a down machine are never posted
      }
      entry.machineDown = true;
      void this.updatePostStatus(entry, "machine_down", entry.decision);
    }
    this.log(`マシン切断: ${mid} (${reason})`);
  }

  async onMachineUp(mid) {
    const wasDown = this.machinesDown.delete(mid);
    if (wasDown) this.log(`マシン復帰: ${mid}`);
    // Decisions resolved while the machine was down (or while this daemon was
    // down) are reconciled once the created replay has had a moment to arrive.
    setTimeout(() => void this.reconcileMachine(mid), RECONCILE_DELAY_MS);
  }

  async reconcileMachine(mid) {
    const stale = [...this.entries.values()].filter(
      (entry) => entry.machineId === mid && entry.machineDown,
    );
    for (const [key, post] of this.posts) {
      if (post.machineId === mid && !this.entries.has(key)) {
        stale.push({
          key,
          machineId: mid,
          machine: { id: mid, label: mid },
          decision: { decision_id: post.decisionId },
          post: { channel: post.channel, ts: post.ts },
          resolved: false,
          postTimer: null,
        });
      }
    }
    for (const entry of stale) {
      const did = entry.decision?.decision_id || entry.post?.decisionId;
      if (!did) continue;
      try {
        const current = await this.getDecision(mid, did);
        if (!current || current.status !== "pending") {
          await this.onResolved(
            mid,
            current || {
              decision_id: did,
              title: entry.decision?.title || did,
              options: entry.decision?.options || [],
              status: "cancelled",
            },
          );
        } else {
          entry.machineDown = false;
          entry.decision = current;
          if (!this.entries.has(entry.key)) this.entries.set(entry.key, entry);
          await this.renderPending(entry);
        }
      } catch (error) {
        this.log(`decision get に失敗 (${mid}/${did}): ${error.message}`);
      }
    }
  }

  // ---- answering ------------------------------------------------------------

  pendingEntryByTs(ts) {
    for (const entry of this.entries.values()) {
      if (entry.post && entry.post.ts === ts && !entry.resolved) return entry;
    }
    return null;
  }

  async submitAnswer(entry, { optionId = null, text = null, via }) {
    if (entry.resolved || entry.answerPending) return;
    entry.answerPending = true;
    const did = entry.decision.decision_id;
    const args = [
      "decision",
      "answer",
      did,
      optionId ?? "",
      "--responder",
      "slack",
      "--machine",
      entry.machineId,
    ];
    if (text) args.push("--text", text);
    this.log(`回答を送信: ${did} option=${optionId ?? "-"} via=${via}`);
    const result = await this.runAnswer(args);
    if (result.code === 0) return; // the resolved event updates the post
    entry.answerPending = false;
    const combined = `${result.stdout}\n${result.stderr}`;
    if (combined.includes("decision_already_resolved")) {
      await this.postThreadNote(entry, "ほかの窓口で決定済みでした。");
      return;
    }
    const detail =
      combined.trim().split("\n").pop()?.slice(0, 300) || "unknown";
    this.log(`回答の送信に失敗 (${did}): ${detail}`);
    await this.postThreadNote(entry, `回答を送れませんでした: ${detail}`);
  }

  async postThreadNote(entry, text) {
    if (!entry.post) return;
    try {
      await this.slack.postThreadReply(entry.post.ts, text);
    } catch (error) {
      this.log(`スレッドへの書き込みに失敗: ${error.message}`);
    }
  }

  // A human reply in the decision thread: a bare number picks that option;
  // anything else is free text (only valid for allow_text decisions).
  async handleThreadReply(entry, text, user = "") {
    const trimmed = (text || "").normalize("NFKC").trim();
    if (!trimmed) return;
    const options = entry.decision.options || [];
    if (/^\d+$/.test(trimmed)) {
      const index = Number(trimmed) - 1;
      if (index >= 0 && index < options.length) {
        await this.submitAnswer(entry, {
          optionId: options[index].id,
          via: `slack reply by ${user || "?"}`,
        });
        return;
      }
    }
    if (entry.decision.allow_text) {
      await this.submitAnswer(entry, {
        text: trimmed,
        via: `slack reply by ${user || "?"}`,
      });
      return;
    }
    await this.postThreadNote(
      entry,
      "この判断待ちは自由記述を受け付けていません。数字のリアクションか、番号の返信で選んでください。",
    );
  }

  async handleReaction(entry, reactionName, user = "") {
    const index = NUMBER_EMOJIS.indexOf(reactionName);
    if (index === -1) return;
    const options = entry.decision.options || [];
    if (index >= options.length) return;
    await this.submitAnswer(entry, {
      optionId: options[index].id,
      via: `slack reaction :${reactionName}: by ${user || "?"}`,
    });
  }

  // ---- Socket Mode ------------------------------------------------------------

  async socketLoop() {
    while (!this.stopped) {
      let url;
      try {
        url = await this.slack.openSocketUrl();
      } catch (error) {
        this.log(`Socket Mode URL の取得に失敗: ${error.message}`);
        await sleep(SOCKET_RECONNECT_MS);
        continue;
      }
      await this.runSocket(url);
      if (!this.stopped) await sleep(SOCKET_RECONNECT_MS);
    }
  }

  runSocket(url) {
    return new Promise((resolve) => {
      let settled = false;
      const finish = () => {
        if (settled) return;
        settled = true;
        this.socketAbort.signal.removeEventListener("abort", onAbort);
        resolve();
      };
      const onAbort = () => {
        try {
          ws.close();
        } catch {}
        finish();
      };
      const ws = new WebSocket(url);
      this.socketAbort.signal.addEventListener("abort", onAbort, { once: true });
      ws.addEventListener("message", (event) => {
        let envelope;
        try {
          envelope = JSON.parse(String(event.data));
        } catch {
          return;
        }
        if (envelope?.envelope_id) {
          try {
            ws.send(JSON.stringify({ envelope_id: envelope.envelope_id }));
          } catch {}
        }
        if (envelope?.type === "disconnect") {
          try {
            ws.close();
          } catch {}
          finish();
          return;
        }
        if (envelope?.payload) void this.handleSocketPayload(envelope.payload);
      });
      ws.addEventListener("close", finish);
      ws.addEventListener("error", () => {
        try {
          ws.close();
        } catch {}
        finish();
      });
    });
  }

  async handleSocketPayload(payload) {
    const event = payload?.type === "event_callback" ? payload.event : payload;
    if (event?.type === "reaction_added") {
      if (this.botUserId && event.user === this.botUserId) return;
      if (event.item?.type !== "message" || !event.item?.ts) return;
      const entry = this.pendingEntryByTs(event.item.ts);
      if (!entry) return;
      await this.handleReaction(entry, event.reaction, event.user || "");
      return;
    }
    if (event?.type === "message") {
      if (event.subtype && event.subtype !== "file_share") return;
      if (event.bot_id || (this.botUserId && event.user === this.botUserId)) {
        return;
      }
      const threadTs = event.thread_ts;
      if (!threadTs) return;
      const entry = this.pendingEntryByTs(threadTs);
      if (!entry) return;
      if (event.ts && event.ts === entry.post?.ts) return;
      await this.handleThreadReply(entry, event.text || "", event.user || "");
    }
  }

  // ---- history polling fallback ----------------------------------------------
  // Socket Mode events can land on another consumer's connection (ai_guard
  // opens its own). Missed answers are recovered by reading channel history.

  _schedulePoll() {
    if (this.stopped) return;
    this.pollTimer = setTimeout(async () => {
      try {
        await this.pollOnce();
      } catch (error) {
        this.log(`履歴ポーリングに失敗: ${error.message}`);
      }
      this._schedulePoll();
    }, this.config.pollIntervalMs);
  }

  async pollOnce() {
    for (const entry of this.entries.values()) {
      if (!entry.post || entry.resolved || entry.answerPending || entry.machineDown) {
        continue;
      }
      await this.pollEntryReactions(entry);
      await this.pollEntryReplies(entry);
    }
  }

  async pollEntryReactions(entry) {
    let message;
    try {
      message = await this.slack.message(entry.post.ts);
    } catch {
      return;
    }
    for (const reaction of message?.reactions || []) {
      const index = NUMBER_EMOJIS.indexOf(reaction.name);
      if (index === -1) continue;
      const users = (reaction.users || []).filter((u) => u !== this.botUserId);
      if (users.length === 0) continue;
      const marker = `${reaction.name}:${users[0]}`;
      if (entry.seenReactions.has(marker)) continue;
      entry.seenReactions.add(marker);
      const options = entry.decision.options || [];
      if (index < options.length) {
        await this.submitAnswer(entry, {
          optionId: options[index].id,
          via: `slack reaction :${reaction.name}: (history)`,
        });
      }
      return;
    }
  }

  async pollEntryReplies(entry) {
    let messages;
    try {
      messages = await this.slack.replies(entry.post.ts);
    } catch {
      return;
    }
    const postTs = entry.post.ts;
    const fresh = messages
      .filter((message) => message.ts && Number(message.ts) > Number(postTs))
      .filter((message) => !entry.seenReplies.has(message.ts))
      .filter(
        (message) =>
          message.user !== this.botUserId &&
          !message.bot_id &&
          message.subtype !== "bot_message" &&
          (!message.subtype || message.subtype === "file_share"),
      )
      .sort((a, b) => Number(a.ts) - Number(b.ts));
    for (const message of fresh) {
      entry.seenReplies.add(message.ts);
      await this.handleThreadReply(entry, message.text || "", message.user || "");
      if (entry.answerPending || entry.resolved) return;
    }
  }
}

// --- process plumbing -----------------------------------------------------------

function spawnDecisionWatch(config, logFn) {
  const child = spawn(
    config.herdrBin,
    ["decision", "watch", "--all-machines"],
    { stdio: ["ignore", "pipe", "pipe"] },
  );
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

function createRunAnswer(config) {
  return (args) => execFileText(config.herdrBin, args, { timeoutMs: 60_000 });
}

function createGetDecision(config) {
  return async (machineId, decisionId) => {
    const result = await execFileText(
      config.herdrBin,
      ["decision", "get", decisionId, "--machine", machineId],
      { timeoutMs: 60_000 },
    );
    if (result.code !== 0) {
      throw new Error(result.stderr.trim() || `exit ${result.code}`);
    }
    const parsed = JSON.parse(result.stdout);
    return parsed?.result?.decision || null;
  };
}

// --- singleton ensure -----------------------------------------------------------

function pidAlive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

function pidLooksOurs(pid) {
  const out = execFileTextSync("ps", ["-o", "command=", "-p", String(pid)]);
  return out.includes("slack-decisions.mjs");
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

    const pluginRoot = path.dirname(fileURLToPath(import.meta.url));
    const exportsList = [
      ["HERDR_PLUGIN_CONFIG_DIR", config.configDir],
      ["HERDR_PLUGIN_STATE_DIR", config.stateDir],
      ["HERDR_SOCKET_PATH", env.HERDR_SOCKET_PATH || ""],
      ["HERDR_BIN_PATH", config.herdrBin],
      ["PATH", env.PATH || ""],
    ]
      .filter(([, value]) => value)
      .map(([key, value]) => `${key}=${shellQuote(value)}`)
      .join(" ");
    const inner = `export ${exportsList}; exec node ${shellQuote(path.join(pluginRoot, "slack-decisions.mjs"))} run`;
    const result = await execFileText(
      config.herdrBin,
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

// --- entry point -----------------------------------------------------------------

async function gateCheck(config) {
  if (!config.enabled) {
    log("設定で無効化されています (enabled=false)");
    return false;
  }
  const user = config.osUser || os.userInfo().username;
  if (user !== config.allowedOsUser) {
    log(
      `このマシンでは Slack 送信をしません (whoami=${user}; 許可: ${config.allowedOsUser})`,
    );
    return false;
  }
  return true;
}

async function main() {
  const mode = process.argv[2] || "run";
  const config = loadConfig(process.env);

  if (mode === "check-sabera") {
    const state = await saberaGlassesState(config.saberaSocket);
    process.stdout.write(
      `${JSON.stringify({ socket: config.saberaSocket, glasses: state })}\n`,
    );
    return;
  }

  if (!(await gateCheck(config))) return;

  if (mode === "ensure") {
    await ensureDaemon(config, process.env);
    return;
  }

  if (mode !== "run") {
    process.stderr.write("usage: slack-decisions.mjs [run|ensure|check-sabera]\n");
    process.exit(2);
  }

  const creds = loadCredentials(config);
  if (!creds.token || !creds.channel) {
    log("Slack 資格情報が読めません（トークンまたはチャンネルなし）。停止します。");
    return;
  }
  if (daemonRunning(config.stateDir)) {
    log(
      `すでにデーモンが動いています (pid ${readDaemonPid(config.stateDir)})。終了します。`,
    );
    return;
  }

  const slack = new SlackClient({
    token: creds.token,
    appToken: creds.appToken,
    channel: creds.channel,
    apiBase: config.apiBase,
  });
  slack.mention = creds.mention;

  let botUserId = "";
  try {
    const auth = await slack.authTest();
    botUserId = auth.user_id || "";
    log(`Slack 接続OK (bot=${botUserId}, channel=${creds.channel})`);
  } catch (error) {
    log(`Slack auth.test に失敗: ${error.message}。停止します。`);
    return;
  }

  writeDaemonPid(config.stateDir);
  const daemon = new SlackDecisionsDaemon({
    config,
    slack,
    botUserId,
    spawnWatch: () => Promise.resolve(spawnDecisionWatch(config, log)),
    runAnswer: createRunAnswer(config),
    getDecision: createGetDecision(config),
    glassesState: () => saberaGlassesState(config.saberaSocket),
    log,
    postsFilePath: path.join(config.stateDir, POSTS_FILE),
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
    return fs.realpathSync(entry) === fs.realpathSync(fileURLToPath(import.meta.url));
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
  buildDecisionBlocks,
  decisionKey,
  fallbackText,
  fmtDeadline,
  loadConfig,
  loadCredentials,
  parseEnvFile,
  saberaGlassesState,
  SlackClient,
  wsUnixRequest,
};
