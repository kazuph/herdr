// Fake-Slack tests for `slack-decisions.mjs deliver`: the real CLI is spawned
// with SLACK_DECISIONS_API_BASE pointed at a local http fake and
// SLACK_DECISIONS_SABERA_SOCK pointed at a fake saberante hub. No request ever
// leaves this machine.

import { execFile } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { FakeSlack, startFakeSabera } from "./fakes.mjs";

const APP_ROOT = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const DAEMON_PATH = path.join(APP_ROOT, "slack-decisions.mjs");

function tmpdir(name) {
  return fs.mkdtempSync(path.join(os.tmpdir(), `slack-deliver-${name}-`));
}

function runCli(args, env) {
  return new Promise((resolve) => {
    execFile(
      process.execPath,
      [DAEMON_PATH, ...args],
      { env, encoding: "utf8", timeout: 20_000 },
      (error, stdout, stderr) => {
        resolve({
          code: error && typeof error.code === "number" ? error.code : 0,
          stdout: String(stdout || ""),
          stderr: String(stderr || "") + (error && !error.code ? `\n${error.message}` : ""),
        });
      },
    );
  });
}

// Minimal env: HOME points at an empty tmp dir so real slack-ask credential
// files / keychain / pass are never read, and the Slack API base is the fake.
function cliEnv(tmp, extra = {}) {
  return {
    PATH: process.env.PATH || "/usr/bin:/bin",
    HOME: tmp,
    SLACK_BOT_TOKEN: "xoxb-deliver-test",
    SLACK_CHANNEL: "C-deliver",
    SLACK_DECISIONS_API_BASE: extra.apiBase || "",
    SLACK_DECISIONS_STATE_DIR: path.join(tmp, "state"),
    SLACK_DECISIONS_SABERA_SOCK: path.join(tmp, "no-hub.sock"),
    SLACK_DECISIONS_NO_KEYCHAIN: "1",
    SLACK_DECISIONS_NO_PASS: "1",
    SLACK_DECISIONS_OS_USER: "kazuph",
    ...extra,
  };
}

test("deliver uploads the file, applies title/comment, and notifies the sabera hub", async () => {
  const tmp = tmpdir("ok");
  const file = path.join(tmp, "result.txt");
  fs.writeFileSync(file, "e2e artifact body\n");
  const fake = await new FakeSlack().start();
  const sabera = await startFakeSabera({
    sockPath: path.join(tmp, "sabera.sock"),
  });
  try {
    const env = cliEnv(tmp, {
      apiBase: fake.base,
      SLACK_DECISIONS_SABERA_SOCK: sabera.sockPath,
    });
    const run = await runCli(
      ["deliver", file, "--title", "テスト成果物", "--comment", "確認してください"],
      env,
    );
    assert.equal(run.code, 0, run.stderr);
    const out = JSON.parse(run.stdout);
    assert.equal(out.ok, true);
    assert.equal(out.file_id, "FAKE0001");
    assert.equal(out.channel, "C-deliver");
    assert.equal(out.notified, true);

    // comment was posted as a message first
    assert.equal(fake.posts.length, 1);
    assert.equal(fake.posts[0].text, "確認してください");
    assert.equal(out.message_ts, fake.posts[0].ts);

    // getUploadURLExternal carried filename+length (form params)
    const meta = fake.uploadMeta.get("FAKE0001");
    assert.equal(meta.filename, "result.txt");
    assert.equal(meta.length, fs.statSync(file).size);
    // the binary body reached the upload URL verbatim
    assert.deepEqual(
      fake.uploadBodies.get("FAKE0001").toString("utf8"),
      "e2e artifact body\n",
    );
    // completeUploadExternal carried the title and channel
    assert.equal(fake.completedUploads.length, 1);
    assert.equal(fake.completedUploads[0].channel_id, "C-deliver");
    assert.deepEqual(fake.completedUploads[0].files, [
      { id: "FAKE0001", title: "テスト成果物" },
    ]);

    // the sabera hub got 「<題名>」を Slack に送りました
    const notifies = sabera.notifies();
    assert.equal(notifies.length, 1);
    assert.equal(notifies[0].from, "slack-decisions");
    assert.match(notifies[0].text, /「テスト成果物」を Slack に送りました/);
  } finally {
    await fake.stop();
    sabera.stop();
  }
});

test("deliver without --title falls back to the basename and skips a missing hub", async () => {
  const tmp = tmpdir("nohub");
  const file = path.join(tmp, "note.md");
  fs.writeFileSync(file, "# memo\n");
  const fake = await new FakeSlack().start();
  try {
    const run = await runCli(
      ["deliver", file],
      cliEnv(tmp, { apiBase: fake.base }),
    );
    assert.equal(run.code, 0, run.stderr);
    const out = JSON.parse(run.stdout);
    assert.equal(out.ok, true);
    assert.equal(out.notified, false); // hub socket absent -> skipped, not an error
    assert.equal(out.message_ts, ""); // no comment -> no message post
    assert.equal(fake.posts.length, 0);
    assert.equal(fake.completedUploads[0].files[0].title, "note.md");
  } finally {
    await fake.stop();
  }
});

test("deliver sends nothing on a non-kazuph machine", async () => {
  const tmp = tmpdir("otherhost");
  const file = path.join(tmp, "result.txt");
  fs.writeFileSync(file, "must not leave this machine\n");
  const fake = await new FakeSlack().start();
  try {
    const run = await runCli(
      ["deliver", file],
      cliEnv(tmp, {
        apiBase: fake.base,
        SLACK_DECISIONS_OS_USER: "someoneelse",
      }),
    );
    assert.equal(fake.calls.length, 0, "must not call Slack at all");
    assert.equal(run.stdout.trim(), "");
    assert.match(run.stderr, /Slack 送信をしません/);
  } finally {
    await fake.stop();
  }
});

test("deliver fails cleanly when the file does not exist", async () => {
  const tmp = tmpdir("missing");
  const fake = await new FakeSlack().start();
  try {
    const run = await runCli(
      ["deliver", path.join(tmp, "nope.txt")],
      cliEnv(tmp, { apiBase: fake.base }),
    );
    assert.equal(run.code, 1);
    assert.match(run.stderr, /file not found/);
    assert.equal(fake.calls.length, 0, "no Slack call before the file check");
  } finally {
    await fake.stop();
  }
});

test("deliver-context reads the path from HERDR_PLUGIN_CONTEXT_JSON.selected_text", async () => {
  const tmp = tmpdir("ctx");
  const file = path.join(tmp, "picked.log");
  fs.writeFileSync(file, "context picked\n");
  const fake = await new FakeSlack().start();
  const sabera = await startFakeSabera({
    sockPath: path.join(tmp, "sabera.sock"),
  });
  try {
    const run = await runCli(
      ["deliver-context"],
      cliEnv(tmp, {
        apiBase: fake.base,
        SLACK_DECISIONS_SABERA_SOCK: sabera.sockPath,
        HERDR_PLUGIN_CONTEXT_JSON: JSON.stringify({
          selected_text: `  ${file}\n`,
          focused_pane_cwd: tmp,
        }),
      }),
    );
    assert.equal(run.code, 0, run.stderr);
    const out = JSON.parse(run.stdout);
    assert.equal(out.path, file);
    assert.equal(fake.completedUploads[0].files[0].title, "picked.log");
    assert.equal(sabera.notifies().length, 1);
  } finally {
    await fake.stop();
    sabera.stop();
  }
});

test("deliver-context resolves a relative path against focused_pane_cwd", async () => {
  const tmp = tmpdir("ctx-rel");
  fs.mkdirSync(path.join(tmp, "out"));
  fs.writeFileSync(path.join(tmp, "out", "a.txt"), "relative\n");
  const fake = await new FakeSlack().start();
  try {
    const run = await runCli(
      ["deliver-context"],
      cliEnv(tmp, {
        apiBase: fake.base,
        HERDR_PLUGIN_CONTEXT_JSON: JSON.stringify({
          selected_text: "out/a.txt",
          focused_pane_cwd: tmp,
        }),
      }),
    );
    assert.equal(run.code, 0, run.stderr);
    const out = JSON.parse(run.stdout);
    assert.equal(out.path, path.join(tmp, "out", "a.txt"));
    assert.equal(fake.uploadMeta.get("FAKE0001").filename, "a.txt");
  } finally {
    await fake.stop();
  }
});

test("deliver-context exits non-zero without a selection", async () => {
  const tmp = tmpdir("ctx-empty");
  const fake = await new FakeSlack().start();
  try {
    const run = await runCli(
      ["deliver-context"],
      cliEnv(tmp, { apiBase: fake.base }),
    );
    assert.equal(run.code, 1);
    assert.match(run.stderr, /selected_text/);
    assert.equal(fake.calls.length, 0);
  } finally {
    await fake.stop();
  }
});

test("deliver without a file argument prints usage", async () => {
  const tmp = tmpdir("usage");
  const run = await runCli(["deliver"], cliEnv(tmp));
  assert.equal(run.code, 2);
  assert.match(run.stderr, /deliver <file>/);
});
