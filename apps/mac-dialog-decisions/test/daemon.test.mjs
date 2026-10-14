// Daemon tests: watch-line handling, the one-at-a-time dialog queue, answer
// command shape, closing on external resolution — all against fakes.

import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import assert from "node:assert/strict";
import { MacDialogDecisionsDaemon } from "../mac-dialog-decisions.mjs";
import { FakeDialog, FakeWatch, waitUntil } from "./fakes.mjs";

const MACHINE = { id: "local", label: "Local" };

function decision(id, overrides = {}) {
  return {
    decision_id: id,
    kind: "ask",
    title: `${id} の題名`,
    status: "pending",
    options: [
      { id: "a", label: "選択A" },
      { id: "b", label: "選択B" },
    ],
    ...overrides,
  };
}

function makeDaemon({ stateDir } = {}) {
  const dir = stateDir || fs.mkdtempSync(path.join(os.tmpdir(), "macd-test-"));
  const watch = new FakeWatch();
  const answers = [];
  const dialogs = [];
  const logs = [];
  const daemon = new MacDialogDecisionsDaemon({
    config: { stateDir: dir },
    spawnWatch: () => Promise.resolve(watch),
    runAnswer: (argv) => {
      answers.push(argv);
      return Promise.resolve({ code: 0, stdout: "{}", stderr: "" });
    },
    spawnDialog: (specPath, spec) => {
      const d = new FakeDialog();
      d.specPath = specPath;
      d.spec = spec;
      dialogs.push(d);
      return d;
    },
    specFilePath: (key) =>
      path.join(dir, "dialogs", `${key.replace(/[^A-Za-z0-9_.-]/g, "_")}.json`),
    log: (...a) => logs.push(a.join(" ")),
  });
  return { daemon, watch, answers, dialogs, logs, dir };
}

test("created decision opens one dialog carrying the full spec", async () => {
  const { daemon, watch, dialogs } = makeDaemon();
  await daemon.start();
  watch.push({ event: "machine_up", machine: MACHINE });
  watch.push({ event: "created", machine: MACHINE, decision: decision("dec-1") });
  await waitUntil(() => dialogs.length === 1);
  const spec = dialogs[0].spec;
  assert.match(spec.text, /dec-1 の題名/);
  assert.match(spec.text, /1\. 選択A/);
  assert.match(spec.text, /2\. 選択B/);
  assert.deepEqual(spec.options, [
    { id: "a", title: "1. 選択A" },
    { id: "b", title: "2. 選択B" },
  ]);
  await daemon.stop();
});

test("single-select answer becomes herdr decision answer with --responder mac-dialog", async () => {
  const { daemon, watch, answers, dialogs } = makeDaemon();
  await daemon.start();
  watch.push({ event: "created", machine: MACHINE, decision: decision("dec-1") });
  await waitUntil(() => dialogs.length === 1);
  dialogs[0].answer(["a"]);
  await waitUntil(() => answers.length === 1);
  assert.deepEqual(answers[0], [
    "decision", "answer", "dec-1", "a",
    "--responder", "mac-dialog", "--machine", "local",
  ]);
  await daemon.stop();
});

test("multi-select + free text sends every option id and --text", async () => {
  const { daemon, watch, answers, dialogs } = makeDaemon();
  await daemon.start();
  watch.push({
    event: "created",
    machine: { id: "mbp", label: "MacBook" },
    decision: decision("dec-2", { multi_select: true, allow_text: true }),
  });
  await waitUntil(() => dialogs.length === 1);
  dialogs[0].answer(["a", "b"], "補足メモ");
  await waitUntil(() => answers.length === 1);
  assert.deepEqual(answers[0], [
    "decision", "answer", "dec-2", "a", "b",
    "--responder", "mac-dialog", "--machine", "mbp",
    "--text", "補足メモ",
  ]);
  await daemon.stop();
});

test("resolving elsewhere closes the open dialog and never sends its answer", async () => {
  const { daemon, watch, answers, dialogs } = makeDaemon();
  await daemon.start();
  watch.push({ event: "created", machine: MACHINE, decision: decision("dec-1") });
  await waitUntil(() => dialogs.length === 1);
  watch.push({
    event: "resolved",
    machine: MACHINE,
    decision: { ...decision("dec-1"), status: "answered" },
  });
  await waitUntil(() => dialogs[0].killed === true);
  // a stale result that raced the close must not be sent
  dialogs[0].answer(["a"]);
  await new Promise((r) => setTimeout(r, 50));
  assert.equal(answers.length, 0);
  await daemon.stop();
});

test("two decisions are presented one at a time, in order", async () => {
  const { daemon, watch, dialogs } = makeDaemon();
  await daemon.start();
  watch.push({ event: "created", machine: MACHINE, decision: decision("dec-1") });
  watch.push({ event: "created", machine: MACHINE, decision: decision("dec-2") });
  await waitUntil(() => dialogs.length === 1);
  assert.match(dialogs[0].spec.text, /dec-1 の題名/);
  assert.match(dialogs[0].spec.window_title, /ほか1件/);
  dialogs[0].answer(["a"]);
  await waitUntil(() => dialogs.length === 2);
  assert.match(dialogs[1].spec.text, /dec-2 の題名/);
  await daemon.stop();
});

test("あとで dismisses without answering and does not reshow", async () => {
  const { daemon, watch, dialogs, answers } = makeDaemon();
  await daemon.start();
  watch.push({ event: "created", machine: MACHINE, decision: decision("dec-1") });
  await waitUntil(() => dialogs.length === 1);
  dialogs[0].dismiss();
  await new Promise((r) => setTimeout(r, 50));
  // replay of the same created event (watch reconnect) keeps it dismissed
  watch.push({ event: "created", machine: MACHINE, decision: decision("dec-1") });
  await new Promise((r) => setTimeout(r, 50));
  assert.equal(dialogs.length, 1);
  assert.equal(answers.length, 0);
  await daemon.stop();
});

test("machine_down closes the dialog; machine_up replay re-shows still-pending", async () => {
  const { daemon, watch, dialogs } = makeDaemon();
  await daemon.start();
  const remote = { id: "mbp", label: "MacBook" };
  watch.push({ event: "created", machine: remote, decision: decision("dec-9") });
  await waitUntil(() => dialogs.length === 1);
  watch.push({ event: "machine_down", machine: remote, reason: "ssh lost" });
  await waitUntil(() => dialogs[0].killed === true);
  watch.push({ event: "machine_up", machine: remote });
  watch.push({ event: "created", machine: remote, decision: decision("dec-9") });
  await waitUntil(() => dialogs.length === 2);
  assert.match(dialogs[1].spec.text, /dec-9 の題名/);
  await daemon.stop();
});

test("decisions created while their machine is down are not shown", async () => {
  const { daemon, watch, dialogs } = makeDaemon();
  await daemon.start();
  const remote = { id: "mbp", label: "MacBook" };
  watch.push({ event: "machine_down", machine: remote, reason: "offline" });
  watch.push({ event: "created", machine: remote, decision: decision("dec-x") });
  await new Promise((r) => setTimeout(r, 100));
  assert.equal(dialogs.length, 0);
  await daemon.stop();
});

test("already_resolved answer failure is logged, not retried or fatal", async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "macd-test-"));
  const watch = new FakeWatch();
  const dialogs = [];
  const logs = [];
  const daemon = new MacDialogDecisionsDaemon({
    config: { stateDir: dir },
    spawnWatch: () => Promise.resolve(watch),
    runAnswer: () =>
      Promise.resolve({
        code: 1,
        stdout: "",
        stderr: "error: decision_already_resolved",
      }),
    spawnDialog: (specPath, spec) => {
      const d = new FakeDialog();
      d.specPath = specPath;
      d.spec = spec;
      dialogs.push(d);
      return d;
    },
    specFilePath: (key) => path.join(dir, `${key}.json`),
    log: (...a) => logs.push(a.join(" ")),
  });
  await daemon.start();
  watch.push({ event: "created", machine: MACHINE, decision: decision("dec-1") });
  await waitUntil(() => dialogs.length === 1);
  dialogs[0].answer(["a"]);
  await waitUntil(() => logs.some((l) => l.includes("決定済み") || l.includes("already_resolved")));
  await daemon.stop();
});

test("non-JSON dialog lines are skipped", async () => {
  const { daemon, watch, dialogs, answers } = makeDaemon();
  await daemon.start();
  watch.push({ event: "created", machine: MACHINE, decision: decision("dec-1") });
  await waitUntil(() => dialogs.length === 1);
  dialogs[0].emit("not json");
  dialogs[0].answer(["b"]);
  await waitUntil(() => answers.length === 1);
  assert.equal(answers[0][3], "b");
  await daemon.stop();
});
