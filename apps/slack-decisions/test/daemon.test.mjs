// Fake-Slack tests for the slack-decisions daemon. No real Slack requests are
// made here: SlackClient.apiBase is pointed at a local http server, Socket
// Mode is pointed at a local ws server, and the Sabera check is either a stub
// function or a fake unix-socket ws server.

import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFile } from "node:child_process";
import { test } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import {
  saberaGlassesState,
  SlackClient,
  SlackDecisionsDaemon,
} from "../slack-decisions.mjs";
import {
  FakeSlack,
  FakeWsServer,
  startFakeSabera,
  waitUntil,
} from "./fakes.mjs";

const PLUGIN_ROOT = path.dirname(fileURLToPath(import.meta.url));
const APP_ROOT = path.dirname(PLUGIN_ROOT);
const DAEMON_PATH = path.join(APP_ROOT, "slack-decisions.mjs");


function tmpdir(name) {
  return fs.mkdtempSync(path.join(os.tmpdir(), `slack-decisions-${name}-`));
}

function sampleDecision(overrides = {}) {
  return {
    decision_id: "dec-test-1",
    kind: "pane.propose_kill",
    title: "pane p1136 を終了してよい？",
    body: "git status で完了確認済み",
    options: [
      { id: "opt-approve", label: "承認" },
      { id: "opt-reject", label: "却下" },
    ],
    allow_text: false,
    status: "pending",
    created_unix_ms: Date.now(),
    expires_unix_ms: Date.now() + 3600_000,
    origin: {
      pane_id: "p1136",
      agent: "claude",
      cwd: "/tmp/repo",
      command: "just test",
      reason: "no output for 600s",
    },
    ...overrides,
  };
}

function makeDaemon({
  slack,
  config: configOverrides = {},
  glasses = null,
  runAnswer = async () => ({ code: 0, stdout: "{}", stderr: "" }),
  getDecision = async () => null,
  postsFilePath = null,
} = {}) {
  const answers = [];
  const daemon = new SlackDecisionsDaemon({
    config: {
      postDelayMs: 80,
      pollIntervalMs: 40,
      socketMode: false,
      saberaSocket: "/tmp/definitely-not-there.sock",
      stateDir: tmpdir("state"),
      ...configOverrides,
    },
    slack,
    botUserId: "UBOT",
    spawnWatch: async () => {
      throw new Error("tests drive onWatchLine directly");
    },
    runAnswer: async (argv) => {
      answers.push(argv);
      return runAnswer(argv);
    },
    getDecision,
    glassesState: async () => glasses,
    log: () => {},
    postsFilePath,
  });
  daemon.testAnswers = answers;
  return daemon;
}

function feed(daemon, event) {
  daemon.onWatchLine(event);
}

async function feedAndSettle(daemon, event, ms = 60) {
  daemon.onWatchLine(event);
  await new Promise((r) => setTimeout(r, ms));
}

const LOCAL = { id: "local", label: "Local" };

// --- tests -------------------------------------------------------------------

test("created + glasses absent -> posts immediately with full context", async () => {
  const fake = await new FakeSlack().start();
  const slack = new SlackClient({
    token: "t",
    channel: "C1",
    apiBase: fake.base,
  });
  const daemon = makeDaemon({ slack, glasses: null });
  try {
    await feedAndSettle(daemon, {
      event: "created",
      machine: LOCAL,
      decision: sampleDecision(),
    });
    assert.equal(fake.posts.length, 1);
    const post = fake.posts[0];
    assert.equal(post.channel, "C1");
    const text = JSON.stringify(post.blocks);
    assert.match(text, /pane p1136 を終了してよい/);
    assert.match(text, /Local/);
    assert.match(text, /p1136/); // pane
    assert.match(text, /claude/); // agent
    assert.match(text, /\/tmp\/repo/); // cwd
    assert.match(text, /just test/); // command
    assert.match(text, /no output for 600s/); // reason
    assert.match(text, /:one: 承認/);
    assert.match(text, /:two: 却下/);
    assert.match(text, /期限/);
    // number reactions were added for each option
    const names = fake.reactionAddCalls.map((c) => c.name);
    assert.deepEqual(names, ["one", "two"]);
  } finally {
    await daemon.stop();
    await fake.stop();
  }
});

test("glasses connected -> waits the configured delay, posts if still pending", async () => {
  const fake = await new FakeSlack().start();
  const slack = new SlackClient({ token: "t", channel: "C1", apiBase: fake.base });
  const daemon = makeDaemon({ slack, glasses: "connected", config: { postDelayMs: 120 } });
  try {
    feed(daemon, { event: "created", machine: LOCAL, decision: sampleDecision() });
    await new Promise((r) => setTimeout(r, 50));
    assert.equal(fake.posts.length, 0, "must not post before the delay");
    await waitUntil(() => fake.posts.length > 0);
    assert.equal(fake.posts.length, 1);
  } finally {
    await daemon.stop();
    await fake.stop();
  }
});

test("glasses connected -> resolved during delay is never posted", async () => {
  const fake = await new FakeSlack().start();
  const slack = new SlackClient({ token: "t", channel: "C1", apiBase: fake.base });
  const daemon = makeDaemon({ slack, glasses: "connected", config: { postDelayMs: 100 } });
  try {
    feed(daemon, { event: "created", machine: LOCAL, decision: sampleDecision() });
    await new Promise((r) => setTimeout(r, 30));
    feed(daemon, {
      event: "resolved",
      machine: LOCAL,
      decision: { ...sampleDecision(), status: "answered" },
    });
    await new Promise((r) => setTimeout(r, 200));
    assert.equal(fake.posts.length, 0);
  } finally {
    await daemon.stop();
    await fake.stop();
  }
});

test("duplicated created (watch reconnect) posts only once", async () => {
  const fake = await new FakeSlack().start();
  const slack = new SlackClient({ token: "t", channel: "C1", apiBase: fake.base });
  const daemon = makeDaemon({ slack, glasses: null });
  try {
    const event = { event: "created", machine: LOCAL, decision: sampleDecision() };
    feed(daemon, event);
    feed(daemon, event);
    feed(daemon, event);
    await new Promise((r) => setTimeout(r, 80));
    assert.equal(fake.posts.length, 1);
  } finally {
    await daemon.stop();
    await fake.stop();
  }
});

test("number reaction -> herdr decision answer <id> <option> --machine --responder slack", async () => {
  const fake = await new FakeSlack().start();
  const slack = new SlackClient({ token: "t", channel: "C1", apiBase: fake.base });
  const daemon = makeDaemon({ slack, glasses: null });
  try {
    await feedAndSettle(daemon, {
      event: "created",
      machine: { id: "darwin-mini", label: "darwin mini" },
      decision: sampleDecision(),
    });
    const ts = fake.posts[0].ts;
    await daemon.handleSocketPayload({
      type: "event_callback",
      event: {
        type: "reaction_added",
        user: "UHUMAN",
        reaction: "two",
        item: { type: "message", channel: "C1", ts },
      },
    });
    assert.equal(daemon.testAnswers.length, 1);
    assert.deepEqual(daemon.testAnswers[0], [
      "decision",
      "answer",
      "dec-test-1",
      "opt-reject",
      "--responder",
      "slack",
      "--machine",
      "darwin-mini",
    ]);
  } finally {
    await daemon.stop();
    await fake.stop();
  }
});

test("thread reply '2' answers with option 2", async () => {
  const fake = await new FakeSlack().start();
  const slack = new SlackClient({ token: "t", channel: "C1", apiBase: fake.base });
  const daemon = makeDaemon({ slack, glasses: null });
  try {
    await feedAndSettle(daemon, {
      event: "created",
      machine: LOCAL,
      decision: sampleDecision(),
    });
    const ts = fake.posts[0].ts;
    await daemon.handleSocketPayload({
      type: "event_callback",
      event: {
        type: "message",
        user: "UHUMAN",
        text: "２",
        ts: "1000.9001",
        thread_ts: ts,
        channel: "C1",
      },
    });
    assert.equal(daemon.testAnswers.length, 1);
    assert.equal(daemon.testAnswers[0][3], "opt-reject");
  } finally {
    await daemon.stop();
    await fake.stop();
  }
});

test("allow_text thread reply forwards --text", async () => {
  const fake = await new FakeSlack().start();
  const slack = new SlackClient({ token: "t", channel: "C1", apiBase: fake.base });
  const daemon = makeDaemon({ slack, glasses: null });
  try {
    await feedAndSettle(daemon, {
      event: "created",
      machine: LOCAL,
      decision: sampleDecision({
        decision_id: "dec-text",
        allow_text: true,
        options: [{ id: "opt-ok", label: "OK" }],
      }),
    });
    const ts = fake.posts[0].ts;
    await daemon.handleSocketPayload({
      type: "event_callback",
      event: {
        type: "message",
        user: "UHUMAN",
        text: "条件付きで承認、先に dry-run",
        ts: "1000.9002",
        thread_ts: ts,
        channel: "C1",
      },
    });
    assert.equal(daemon.testAnswers.length, 1);
    const argv = daemon.testAnswers[0];
    assert.equal(argv[3], "");
    assert.deepEqual(argv.slice(4), [
      "--responder",
      "slack",
      "--machine",
      "local",
      "--text",
      "条件付きで承認、先に dry-run",
    ]);
  } finally {
    await daemon.stop();
    await fake.stop();
  }
});

test("free text on a non-allow_text decision is refused in thread", async () => {
  const fake = await new FakeSlack().start();
  const slack = new SlackClient({ token: "t", channel: "C1", apiBase: fake.base });
  const daemon = makeDaemon({ slack, glasses: null });
  try {
    await feedAndSettle(daemon, {
      event: "created",
      machine: LOCAL,
      decision: sampleDecision(),
    });
    const ts = fake.posts[0].ts;
    await daemon.handleSocketPayload({
      type: "event_callback",
      event: {
        type: "message",
        user: "UHUMAN",
        text: "たぶんOK",
        ts: "1000.9003",
        thread_ts: ts,
        channel: "C1",
      },
    });
    assert.equal(daemon.testAnswers.length, 0);
    const notes = (fake.threadReplies.get(ts) || []).filter((m) => m.bot_id);
    assert.equal(notes.length, 1);
    assert.match(notes[0].text, /自由記述/);
  } finally {
    await daemon.stop();
    await fake.stop();
  }
});

test("resolved answered -> post updated with who and what", async () => {
  const fake = await new FakeSlack().start();
  const slack = new SlackClient({ token: "t", channel: "C1", apiBase: fake.base });
  const daemon = makeDaemon({ slack, glasses: null });
  try {
    await feedAndSettle(daemon, {
      event: "created",
      machine: LOCAL,
      decision: sampleDecision(),
    });
    const ts = fake.posts[0].ts;
    await feedAndSettle(daemon, {
      event: "resolved",
      machine: LOCAL,
      decision: {
        ...sampleDecision(),
        status: "answered",
        answer: {
          option_id: "opt-approve",
          responder: "tui",
          answered_unix_ms: Date.now(),
        },
      },
    });
    const update = fake.updates.find((u) => u.ts === ts);
    assert.ok(update, "chat.update was called for the post");
    const text = JSON.stringify(update.blocks);
    assert.match(text, /決定済み/);
    assert.match(text, /承認/);
    assert.match(text, /tui/);
    assert.equal(daemon.entries.size, 0);
  } finally {
    await daemon.stop();
    await fake.stop();
  }
});

test("resolved expired / cancelled also update the post", async () => {
  const fake = await new FakeSlack().start();
  const slack = new SlackClient({ token: "t", channel: "C1", apiBase: fake.base });
  const daemon = makeDaemon({ slack, glasses: null });
  try {
    for (const [id, status, match] of [
      ["dec-exp", "expired", /期限切れ/],
      ["dec-can", "cancelled", /取り消し/],
    ]) {
      await feedAndSettle(daemon, {
        event: "created",
        machine: LOCAL,
        decision: sampleDecision({ decision_id: id }),
      });
      const ts = fake.posts.at(-1).ts;
      await feedAndSettle(daemon, {
        event: "resolved",
        machine: LOCAL,
        decision: { ...sampleDecision({ decision_id: id }), status },
      });
      const update = fake.updates.find((u) => u.ts === ts);
      assert.ok(update);
      assert.match(JSON.stringify(update.blocks), match);
    }
  } finally {
    await daemon.stop();
    await fake.stop();
  }
});

test("machine_down before post -> never posts; after post -> marked down", async () => {
  const fake = await new FakeSlack().start();
  const slack = new SlackClient({ token: "t", channel: "C1", apiBase: fake.base });
  const daemon = makeDaemon({ slack, glasses: "connected", config: { postDelayMs: 80 } });
  try {
    feed(daemon, {
      event: "machine_down",
      machine: { id: "darwin-mini" },
      reason: "ssh dead",
    });
    feed(daemon, {
      event: "created",
      machine: { id: "darwin-mini", label: "darwin mini" },
      decision: sampleDecision({ decision_id: "dec-down" }),
    });
    await new Promise((r) => setTimeout(r, 160));
    assert.equal(fake.posts.length, 0, "down machine's pending is not posted");

    // posted decision of a machine that then goes down
    await feedAndSettle(daemon, {
      event: "created",
      machine: LOCAL,
      decision: sampleDecision({ decision_id: "dec-up" }),
    }, 300);
    // local machine is not down: it posts via delay timer
    await waitUntil(() => fake.posts.length === 1);
    const ts = fake.posts[0].ts;
    feed(daemon, { event: "machine_down", machine: LOCAL, reason: "lost" });
    await waitUntil(() => fake.updates.some((u) => u.ts === ts));
    assert.match(JSON.stringify(fake.updates.at(-1).blocks), /マシン切断/);
  } finally {
    await daemon.stop();
    await fake.stop();
  }
});

test("decision_already_resolved -> thread note, no retry loop", async () => {
  const fake = await new FakeSlack().start();
  const slack = new SlackClient({ token: "t", channel: "C1", apiBase: fake.base });
  const daemon = makeDaemon({
    slack,
    glasses: null,
    runAnswer: async () => ({
      code: 1,
      stdout: "",
      stderr: "error: decision_already_resolved",
    }),
  });
  try {
    await feedAndSettle(daemon, {
      event: "created",
      machine: LOCAL,
      decision: sampleDecision(),
    });
    const ts = fake.posts[0].ts;
    await daemon.handleSocketPayload({
      type: "event_callback",
      event: {
        type: "reaction_added",
        user: "UHUMAN",
        reaction: "one",
        item: { type: "message", ts },
      },
    });
    assert.equal(daemon.testAnswers.length, 1);
    const notes = (fake.threadReplies.get(ts) || []).filter((m) => m.bot_id);
    assert.equal(notes.length, 1);
    assert.match(notes[0].text, /決定済み/);
  } finally {
    await daemon.stop();
    await fake.stop();
  }
});

test("history poll recovers a reaction that Socket Mode missed", async () => {
  const fake = await new FakeSlack().start();
  const slack = new SlackClient({ token: "t", channel: "C1", apiBase: fake.base });
  const daemon = makeDaemon({ slack, glasses: null });
  try {
    await feedAndSettle(daemon, {
      event: "created",
      machine: LOCAL,
      decision: sampleDecision(),
    });
    const ts = fake.posts[0].ts;
    fake.userReact(ts, "one");
    await daemon.pollOnce();
    assert.equal(daemon.testAnswers.length, 1);
    assert.equal(daemon.testAnswers[0][3], "opt-approve");
  } finally {
    await daemon.stop();
    await fake.stop();
  }
});

test("history poll recovers a thread reply that Socket Mode missed", async () => {
  const fake = await new FakeSlack().start();
  const slack = new SlackClient({ token: "t", channel: "C1", apiBase: fake.base });
  const daemon = makeDaemon({ slack, glasses: null });
  try {
    await feedAndSettle(daemon, {
      event: "created",
      machine: LOCAL,
      decision: sampleDecision({ allow_text: true, decision_id: "dec-poll" }),
    });
    const ts = fake.posts[0].ts;
    fake.userReply(ts, "ポーリング経由の回答");
    await daemon.pollOnce();
    assert.equal(daemon.testAnswers.length, 1);
    const argv = daemon.testAnswers[0];
    assert.equal(argv.at(-2), "--text");
    assert.equal(argv.at(-1), "ポーリング経由の回答");
  } finally {
    await daemon.stop();
    await fake.stop();
  }
});

test("socket mode end-to-end: envelope over real ws drives an answer", async () => {
  const fake = await new FakeSlack().start();
  const ws = await new FakeWsServer().start();
  fake.wsUrl = ws.url;
  const slack = new SlackClient({
    token: "t",
    appToken: "xapp-fake",
    channel: "C1",
    apiBase: fake.base,
  });
  const daemon = makeDaemon({ slack, glasses: null, config: { socketMode: true } });
  try {
    daemon.socketLoop().catch(() => {});
    await ws.waitForClient();
    assert.equal(fake.socketUrls, 1, "exactly one socket-mode connection");
    await feedAndSettle(daemon, {
      event: "created",
      machine: LOCAL,
      decision: sampleDecision(),
    });
    const ts = fake.posts[0].ts;
    ws.send({
      envelope_id: "env-1",
      type: "events_api",
      payload: {
        type: "event_callback",
        event: {
          type: "reaction_added",
          user: "UHUMAN",
          reaction: "one",
          item: { type: "message", channel: "C1", ts },
        },
      },
    });
    await waitUntil(() => daemon.testAnswers.length > 0);
    assert.equal(daemon.testAnswers[0][3], "opt-approve");
    await waitUntil(() =>
      ws.received.some((m) => JSON.parse(m).envelope_id === "env-1"),
    );
  } finally {
    await daemon.stop();
    await ws.stop();
    await fake.stop();
  }
});

test("posts file restore -> restart resumes answering without reposting", async () => {
  const dir = tmpdir("restore");
  const postsFile = path.join(dir, "posts.json");
  const fake = await new FakeSlack().start();
  const slack = new SlackClient({ token: "t", channel: "C1", apiBase: fake.base });
  const first = makeDaemon({ slack, glasses: null, postsFilePath: postsFile });
  try {
    await feedAndSettle(first, {
      event: "created",
      machine: LOCAL,
      decision: sampleDecision(),
    });
    assert.equal(fake.posts.length, 1);
    await first.stop();

    const second = makeDaemon({ slack, glasses: null, postsFilePath: postsFile });
    second.loadPostsFile();
    await feedAndSettle(second, {
      event: "created",
      machine: LOCAL,
      decision: sampleDecision(),
    });
    assert.equal(fake.posts.length, 1, "no duplicate post after restart");
    const ts = fake.posts[0].ts;
    await second.handleSocketPayload({
      type: "event_callback",
      event: {
        type: "reaction_added",
        user: "UHUMAN",
        reaction: "two",
        item: { type: "message", ts },
      },
    });
    assert.equal(second.testAnswers.length, 1);
    await second.stop();
  } finally {
    await fake.stop();
  }
});

test("saberaGlassesState reads the unix-socket ws handshake", async () => {
  const dir = tmpdir("sabera");
  const sockPath = path.join(dir, "relay.sock");
  const fake = await startFakeSabera({ glasses: "connected", sockPath });
  try {
    assert.equal(await saberaGlassesState(sockPath), "connected");
  } finally {
    fake.stop();
  }
  const gone = await saberaGlassesState(path.join(dir, "gone.sock"), 300);
  assert.equal(gone, null);
});

test("non-kazuph user -> exits without touching Slack", async () => {
  const state = tmpdir("notme");
  const result = await new Promise((resolve) => {
    execFile(
      process.execPath,
      [DAEMON_PATH, "run"],
      {
        env: {
          ...process.env,
          SLACK_DECISIONS_OS_USER: "someoneelse",
          SLACK_DECISIONS_STATE_DIR: state,
          SLACK_BOT_TOKEN: "xoxb-fake",
          SLACK_CHANNEL: "C1",
          SLACK_APP_TOKEN: "",
          SLACK_DECISIONS_API_BASE: "http://127.0.0.1:1",
        },
        timeout: 10_000,
      },
      (error, stdout, stderr) =>
        resolve({ code: error?.code ?? 0, stdout, stderr }),
    );
  });
  assert.equal(result.code, 0);
  assert.match(result.stderr, /Slack 送信をしません/);
});

test("kazuph but no credentials -> exits without posting", async () => {
  const state = tmpdir("nocreds");
  const result = await new Promise((resolve) => {
    execFile(
      process.execPath,
      [DAEMON_PATH, "run"],
      {
        env: {
          ...process.env,
          SLACK_DECISIONS_OS_USER: "kazuph",
          SLACK_DECISIONS_STATE_DIR: state,
          SLACK_BOT_TOKEN: "",
          SLACK_CHANNEL: "",
          SLACK_CHANNEL_ID: "",
          SLACK_APP_TOKEN: "",
          SLACK_DECISIONS_CONFIG: path.join(state, "missing.json"),
          SLACK_DECISIONS_NO_KEYCHAIN: "1",
          SLACK_DECISIONS_NO_PASS: "1",
          HOME: state,
        },
        timeout: 10_000,
      },
      (error, stdout, stderr) =>
        resolve({ code: error?.code ?? 0, stdout, stderr }),
    );
  });
  assert.equal(result.code, 0);
  assert.match(result.stderr, /資格情報/);
});
