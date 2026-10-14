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

async function feedAndSettle(daemon, event, ms = 300) {
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

test("resolved answered -> post and bot thread replies are deleted", async () => {
  // A resolved decision's post must not linger: Slack keeps only posts
  // that still need an answer. The bot's own thread replies are deleted
  // with it; a human's reply cannot be deleted with a bot token (it
  // leaves the channel view together with the parent post).
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
    const botNote = await slack.postThreadReply(ts, "ボットの返信");
    const humanNote = fake.userReply(ts, "店長の返信");
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
    assert.equal(fake.posts.length, 0, "the resolved post was deleted");
    const deletedTs = fake.deletes.map((d) => d.ts);
    assert.ok(deletedTs.includes(ts), "chat.delete called for the post");
    assert.ok(deletedTs.includes(botNote.ts), "bot reply deleted too");
    assert.ok(
      !deletedTs.includes(humanNote),
      "a human reply is not deleted by the bot",
    );
    assert.equal(daemon.entries.size, 0);
  } finally {
    await daemon.stop();
    await fake.stop();
  }
});

test("answered via a Slack reply -> resolved event deletes the post", async () => {
  // Answering on Slack itself still ends with the post deleted once the
  // resolved event arrives — Slack keeps only unanswered posts.
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
        subtype: undefined,
        user: "UHUMAN",
        text: "2",
        channel: "C1",
        ts: "1000.9002",
        thread_ts: ts,
      },
    });
    await waitUntil(() => daemon.testAnswers.length === 1);
    await feedAndSettle(daemon, {
      event: "resolved",
      machine: LOCAL,
      decision: {
        ...sampleDecision(),
        status: "answered",
        answer: {
          option_id: "opt-reject",
          responder: "slack",
          answered_unix_ms: Date.now(),
        },
      },
    });
    assert.equal(fake.posts.length, 0);
    assert.ok(fake.deletes.some((d) => d.ts === ts));
  } finally {
    await daemon.stop();
    await fake.stop();
  }
});

test("resolved expired deletes the post; cancelled deletes it after the reuse window", async () => {
  // Expired posts are deleted immediately. A cancelled post stays
  // adoptable for the reuse window (churn guard), then is deleted.
  const fake = await new FakeSlack().start();
  const slack = new SlackClient({ token: "t", channel: "C1", apiBase: fake.base });
  const daemon = makeDaemon({
    slack,
    glasses: null,
    config: { recreatePostReuseMs: 400 },
  });
  try {
    await feedAndSettle(daemon, {
      event: "created",
      machine: LOCAL,
      decision: sampleDecision({ decision_id: "dec-exp" }),
    });
    const expiredTs = fake.posts.at(-1).ts;
    await feedAndSettle(daemon, {
      event: "resolved",
      machine: LOCAL,
      decision: { ...sampleDecision({ decision_id: "dec-exp" }), status: "expired" },
    });
    await waitUntil(() => !fake.posts.some((p) => p.ts === expiredTs));
    assert.ok(
      fake.deletes.some((d) => d.ts === expiredTs),
      "expired post deleted immediately",
    );

    await feedAndSettle(daemon, {
      event: "created",
      machine: LOCAL,
      decision: sampleDecision({ decision_id: "dec-can" }),
    });
    const cancelledTs = fake.posts.at(-1).ts;
    feed(daemon, {
      event: "resolved",
      machine: LOCAL,
      decision: {
        ...sampleDecision({ decision_id: "dec-can" }),
        status: "cancelled",
      },
    });
    await waitUntil(() => fake.updates.some((u) => u.ts === cancelledTs));
    assert.ok(
      fake.posts.some((p) => p.ts === cancelledTs),
      "cancelled post stays adoptable during the reuse window",
    );
    assert.match(
      JSON.stringify(
        fake.updates.find((u) => u.ts === cancelledTs)?.blocks || [],
      ),
      /取り消し/,
    );
    await waitUntil(() => !fake.posts.some((p) => p.ts === cancelledTs));
    assert.ok(
      fake.deletes.some((d) => d.ts === cancelledTs),
      "cancelled post deleted once the reuse window ends",
    );
  } finally {
    await daemon.stop();
    await fake.stop();
  }
});

test("recreated same question right after cancel reuses the post", async () => {
  // Screen-prompt churn rebuilds the pending decision under a new id;
  // the Slack post must not stack — the cancelled decision's post is
  // adopted and updated back to pending.
  const fake = await new FakeSlack().start();
  const slack = new SlackClient({ token: "t", channel: "C1", apiBase: fake.base });
  const daemon = makeDaemon({ slack, glasses: null });
  try {
    const question = {
      kind: "agent_prompt",
      title: "複数質問の確認（2つ目）。「いぬ」を選んでください",
      question_label: "質問 2/2 動物",
      options: [
        { id: "1", label: "ねこ" },
        { id: "2", label: "いぬ" },
      ],
      allow_text: true,
      origin: { pane_id: "p2301", agent: "claude", reason: "screen_prompt" },
    };
    await feedAndSettle(daemon, {
      event: "created",
      machine: LOCAL,
      decision: sampleDecision({ ...question, decision_id: "dec-a" }),
    });
    assert.equal(fake.posts.length, 1);
    const ts = fake.posts[0].ts;

    // Churn: cancelled, then recreated under a new id with the same
    // question payload.
    await feedAndSettle(daemon, {
      event: "resolved",
      machine: LOCAL,
      decision: {
        ...sampleDecision({ ...question, decision_id: "dec-a" }),
        status: "cancelled",
      },
    });
    await feedAndSettle(daemon, {
      event: "created",
      machine: LOCAL,
      decision: sampleDecision({ ...question, decision_id: "dec-b" }),
    });
    assert.equal(fake.posts.length, 1, "no second post for the same question");
    const backToPending = fake.updates.filter((u) => u.ts === ts);
    assert.ok(
      backToPending.some((u) => JSON.stringify(u.blocks).match(/取り消し/)),
      "cancel updated the reused post",
    );
    assert.ok(
      backToPending.some((u) =>
        JSON.stringify(u.blocks).match(/質問 2\/2 動物/),
      ),
      "the reused post renders the recreated decision as pending",
    );

    // A different question still gets its own post.
    await feedAndSettle(daemon, {
      event: "created",
      machine: LOCAL,
      decision: sampleDecision({
        ...question,
        decision_id: "dec-c",
        title: "別の質問",
      }),
    });
    assert.equal(fake.posts.length, 2, "a different question posts anew");
  } finally {
    await daemon.stop();
    await fake.stop();
  }
});

test("machine_down deletes posts; a still-pending decision is re-posted on machine_up", async () => {
  // A down machine's questions cannot be answered, so their posts are
  // deleted; when the machine returns and the decision is still pending,
  // it is posted again.
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
    await waitUntil(() => fake.posts.length === 0);
    assert.ok(
      fake.deletes.some((d) => d.ts === ts),
      "the down machine's post is deleted",
    );

    // machine_up: the created replay re-posts the still-pending decision
    await feedAndSettle(daemon, {
      event: "machine_up",
      machine: LOCAL,
    });
    await feedAndSettle(daemon, {
      event: "created",
      machine: LOCAL,
      decision: sampleDecision({ decision_id: "dec-up" }),
    });
    await waitUntil(() => fake.posts.length === 1);
    assert.notEqual(
      fake.posts[0].ts,
      ts,
      "a fresh post replaces the deleted one",
    );
  } finally {
    await daemon.stop();
    await fake.stop();
  }
});

test("restart reconcile deletes restored posts whose decisions already resolved", async () => {
  // Posts left over from before a restart are checked on machine_up;
  // ones whose decisions are no longer pending are deleted so Slack
  // keeps only unanswered posts.
  const dir = tmpdir("reconcile");
  const postsFile = path.join(dir, "posts.json");
  const fake = await new FakeSlack().start();
  const slack = new SlackClient({ token: "t", channel: "C1", apiBase: fake.base });
  const first = makeDaemon({ slack, glasses: null, postsFilePath: postsFile });
  try {
    await feedAndSettle(first, {
      event: "created",
      machine: LOCAL,
      decision: sampleDecision({ decision_id: "dec-gone" }),
    });
    assert.equal(fake.posts.length, 1);
    const ts = fake.posts[0].ts;
    await first.stop();

    const second = makeDaemon({
      slack,
      glasses: null,
      postsFilePath: postsFile,
      getDecision: async () => ({
        ...sampleDecision({ decision_id: "dec-gone" }),
        status: "answered",
      }),
    });
    try {
      second.loadPostsFile();
      assert.equal(second.posts.size, 1, "posts file restored");
      feed(second, { event: "machine_up", machine: LOCAL });
      await waitUntil(() => fake.posts.length === 0, { timeout: 6_000 });
      assert.ok(
        fake.deletes.some((d) => d.ts === ts),
        "the resolved decision's leftover post is deleted",
      );
    } finally {
      await second.stop();
    }
  } finally {
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

test("multi_select reactions accumulate then white_check_mark submits option_ids", async () => {
  const fake = await new FakeSlack().start();
  const slack = new SlackClient({ token: "t", channel: "C1", apiBase: fake.base });
  const daemon = makeDaemon({ slack, glasses: null });
  try {
    await feedAndSettle(daemon, {
      event: "created",
      machine: LOCAL,
      decision: sampleDecision({
        multi_select: true,
        options: [
          { id: "opt-a", label: "検索" },
          { id: "opt-b", label: "追加" },
          { id: "opt-c", label: "通知" },
        ],
      }),
    });
    const ts = fake.posts[0].ts;
    const react = async (reaction) =>
      daemon.handleSocketPayload({
        type: "event_callback",
        event: {
          type: "reaction_added",
          user: "UHUMAN",
          reaction,
          item: { type: "message", channel: "C1", ts },
        },
      });
    await react("one");
    await react("three");
    // Number reactions only stage the selection; nothing is answered yet.
    assert.equal(daemon.testAnswers.length, 0);
    await react("white_check_mark");
    assert.equal(daemon.testAnswers.length, 1);
    assert.deepEqual(daemon.testAnswers[0], [
      "decision",
      "answer",
      "dec-test-1",
      "opt-a",
      "opt-c",
      "--responder",
      "slack",
      "--machine",
      "local",
    ]);
  } finally {
    await daemon.stop();
    await fake.stop();
  }
});

test("multi_select white_check_mark without picks posts a note instead of answering", async () => {
  const fake = await new FakeSlack().start();
  const slack = new SlackClient({ token: "t", channel: "C1", apiBase: fake.base });
  const daemon = makeDaemon({ slack, glasses: null });
  try {
    await feedAndSettle(daemon, {
      event: "created",
      machine: LOCAL,
      decision: sampleDecision({ multi_select: true }),
    });
    const ts = fake.posts[0].ts;
    await daemon.handleSocketPayload({
      type: "event_callback",
      event: {
        type: "reaction_added",
        user: "UHUMAN",
        reaction: "white_check_mark",
        item: { type: "message", channel: "C1", ts },
      },
    });
    assert.equal(daemon.testAnswers.length, 0);
    const notes = [...fake.threadReplies.values()]
      .flat()
      .map((reply) => reply.text)
      .join("\n");
    assert.match(notes, /番号リアクション/);
  } finally {
    await daemon.stop();
    await fake.stop();
  }
});

test("multi_select thread reply '1,3' submits option_ids", async () => {
  const fake = await new FakeSlack().start();
  const slack = new SlackClient({ token: "t", channel: "C1", apiBase: fake.base });
  const daemon = makeDaemon({ slack, glasses: null });
  try {
    await feedAndSettle(daemon, {
      event: "created",
      machine: LOCAL,
      decision: sampleDecision({
        multi_select: true,
        options: [
          { id: "opt-a", label: "検索" },
          { id: "opt-b", label: "追加" },
          { id: "opt-c", label: "通知" },
        ],
      }),
    });
    const ts = fake.posts[0].ts;
    await daemon.handleSocketPayload({
      type: "event_callback",
      event: {
        type: "message",
        user: "UHUMAN",
        text: "1,3",
        ts: "1000.9001",
        thread_ts: ts,
        channel: "C1",
      },
    });
    assert.equal(daemon.testAnswers.length, 1);
    assert.deepEqual(daemon.testAnswers[0].slice(3, 5), ["opt-a", "opt-c"]);
  } finally {
    await daemon.stop();
    await fake.stop();
  }
});
