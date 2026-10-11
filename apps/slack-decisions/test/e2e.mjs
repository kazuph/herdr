// e2e: real herdr server (isolated session) + real daemon + fake Slack.
//
//   node test/e2e.mjs [--out <dir>]
//
// Spawns `herdr server` against throwaway XDG dirs, runs slack-decisions.mjs
// against it with Slack pointed at a local http+ws fake, creates a decision
// with `herdr decision ask`, answers it by injecting a number reaction over
// the socket-mode websocket, and asserts the decision resolves with the
// picked option and the Slack post is updated to 決定済み.

import { execFile, spawn } from "node:child_process";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { FakeSlack, FakeWsServer, waitUntil } from "./fakes.mjs";

const APP_ROOT = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const REPO_ROOT = path.resolve(APP_ROOT, "..", "..");
const HERDR = path.join(REPO_ROOT, "target", "debug", "herdr");
const DAEMON = path.join(APP_ROOT, "slack-decisions.mjs");

const outIndex = process.argv.indexOf("--out");
const OUT =
  outIndex >= 0
    ? process.argv[outIndex + 1]
    : path.join(REPO_ROOT, ".artifacts", "feat-slack-decisions", "e2e");
fs.mkdirSync(OUT, { recursive: true });

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "slack-decisions-e2e-"));
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
    process.stdout.write(`${ok ? "PASS" : "FAIL"} ${name}${detail ? ` — ${detail}` : ""}\n`);
  };

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

  // 2. fake Slack (web api + socket mode endpoint)
  const fake = await new FakeSlack().start();
  const ws = await new FakeWsServer().start();
  fake.wsUrl = ws.url;
  cleanup.push(() => fake.stop());
  cleanup.push(() => ws.stop());

  // 3. the real daemon, glasses absent (no saberante socket) -> immediate post
  const daemonEnv = {
    ...herdrEnv,
    HERDR_BIN_PATH: HERDR,
    SLACK_BOT_TOKEN: "xoxb-e2e",
    SLACK_CHANNEL: "C-e2e",
    SLACK_APP_TOKEN: "xapp-e2e",
    SLACK_DECISIONS_API_BASE: fake.base,
    SLACK_DECISIONS_STATE_DIR: stateDir,
    SLACK_DECISIONS_SABERA_SOCK: path.join(tmp, "no-sabera.sock"),
    SLACK_DECISIONS_NO_KEYCHAIN: "1",
    SLACK_DECISIONS_NO_PASS: "1",
    SLACK_DECISIONS_POLL_INTERVAL_MS: "200",
    HOME: tmp, // keep the real slack-ask env files out of this run
  };
  const daemon = spawnLogged(
    [process.execPath, DAEMON, "run"],
    daemonEnv,
    daemonLog,
  );
  cleanup.push(() => daemon.kill("SIGTERM"));
  await waitUntil(() => fake.socketUrls > 0);
  await ws.waitForClient();
  step("daemon socket-mode connection", true, `opens=${fake.socketUrls}`);

  // 4. create a real pending decision (blocks until answered)
  let askOut = "";
  let askCode = null;
  const ask = execFile(
    HERDR,
    [
      "decision",
      "ask",
      "--kind",
      "ask",
      "--title",
      "e2e: Slack から承認して",
      "--body",
      "feat/slack-decisions の動作確認",
      "--option",
      "approve:承認する",
      "--option",
      "reject:却下する",
      "--allow-text",
      "--timeout",
      "120",
    ],
    { env: { ...process.env, ...herdrEnv }, encoding: "utf8" },
    (error, stdout, stderr) => {
      askOut = `${stdout}\n${stderr}`;
      askCode = error?.code ?? 0;
    },
  );
  cleanup.push(() => ask.kill("SIGTERM"));

  // 5. fake slack receives the post
  await waitUntil(() => fake.posts.length > 0);
  const post = fake.posts[0];
  fs.writeFileSync(
    path.join(OUT, "post.json"),
    JSON.stringify(post, null, 2),
  );
  step("decision posted to fake slack", true, `ts=${post.ts}`);

  // 6. human reacts with :one: over the socket-mode websocket
  ws.send({
    envelope_id: "e2e-env-1",
    type: "events_api",
    payload: {
      type: "event_callback",
      event: {
        type: "reaction_added",
        user: "UHUMAN",
        reaction: "one",
        item: { type: "message", channel: "C-e2e", ts: post.ts },
      },
    },
  });

  // 7. decision ask resolves with the picked option
  await waitUntil(() => askCode !== null, { timeout: 15_000 });
  fs.writeFileSync(path.join(OUT, "ask-output.txt"), askOut);
  const answered =
    askOut.includes('"approve"') || askOut.includes("opt") || askOut.includes("answered");
  step("herdr decision ask resolved", askCode === 0, askOut.trim().slice(0, 200));
  step("answer recorded as approve via slack", answered && askOut.includes('"slack"'), askOut.trim().slice(0, 200));

  // 8. the slack post is updated to 決定済み
  await waitUntil(() => fake.updates.some((u) => u.ts === post.ts));
  const update = fake.updates.find((u) => u.ts === post.ts);
  const updatedText = JSON.stringify(update.blocks);
  step(
    "slack post updated to resolved",
    /決定済み/.test(updatedText) && /slack/.test(updatedText),
    updatedText.slice(0, 200),
  );

  fs.writeFileSync(
    path.join(OUT, "slack-state.json"),
    JSON.stringify(
      {
        posts: fake.posts,
        updates: fake.updates,
        threadReplies: Object.fromEntries(fake.threadReplies),
        calls: fake.calls.map((c) => c.method),
      },
      null,
      2,
    ),
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
