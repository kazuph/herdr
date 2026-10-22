// Daemon tests: watch-line handling, the keyed multi-window app transport, answer
// command shape, closing on external resolution — all against fakes.

import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import assert from "node:assert/strict";
import { MacDialogDecisionsDaemon } from "../mac-dialog-decisions.mjs";
import { dialogSession, FakeWatch, waitUntil } from "./fakes.mjs";

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

function makeDaemon({ stateDir, laterMs, answerRetryMs, runAnswer } = {}) {
  const dir = stateDir || fs.mkdtempSync(path.join(os.tmpdir(), "macd-test-"));
  const watch = new FakeWatch();
  const answers = [];
  const dialogs = [];
  const logs = [];
  const daemon = new MacDialogDecisionsDaemon({
    config: { stateDir: dir, laterMs, answerRetryMs },
    spawnWatch: () => Promise.resolve(watch),
    runAnswer: (argv) => {
      answers.push(argv);
      return runAnswer ? runAnswer(argv) : Promise.resolve({ code: 0, stdout: "{}", stderr: "" });
    },
    spawnDialog: () => dialogSession(dialogs),
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
  assert.equal(spec.title, "dec-1 の題名");
  assert.deepEqual(
    spec.options.map((o) => [o.id, o.label]),
    [
      ["a", "選択A"],
      ["b", "選択B"],
    ],
  );
  // the spec on disk is what the panel reads
  assert.deepEqual(JSON.parse(fs.readFileSync(dialogs[0].specPath, "utf8")).title, "dec-1 の題名");
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

test("two decisions open together in one process and answering either preserves the other", async () => {
  const { daemon, watch, dialogs } = makeDaemon();
  await daemon.start();
  watch.push({ event: "created", machine: MACHINE, decision: decision("dec-1") });
  watch.push({ event: "created", machine: MACHINE, decision: decision("dec-2") });
  await waitUntil(() => dialogs.length === 2);
  const process = daemon.child;
  assert.equal(daemon.showing.size, 2);
  dialogs[1].answer(["a"]);
  await waitUntil(() => daemon.showing.size === 1);
  assert.equal(daemon.child, process);
  assert.equal(dialogs[0].killed, false);
  assert.match(dialogs[0].spec.title, /dec-1 の題名/);
  await daemon.stop();
});

test("あとで hides the dialog without answering and shows it again later", async () => {
  const { daemon, watch, dialogs, answers } = makeDaemon({ laterMs: 150 });
  await daemon.start();
  watch.push({ event: "created", machine: MACHINE, decision: decision("dec-1") });
  await waitUntil(() => dialogs.length === 1);
  dialogs[0].later();
  await new Promise((r) => setTimeout(r, 50));
  // a watch reconnect replay does not bring it back early
  watch.push({ event: "created", machine: MACHINE, decision: decision("dec-1") });
  await new Promise((r) => setTimeout(r, 30));
  assert.equal(dialogs.length, 1);
  assert.equal(answers.length, 0);
  await waitUntil(() => dialogs.length === 2);
  assert.match(dialogs[1].spec.title, /dec-1 の題名/);
  await daemon.stop();
});

test("あとで does not come back once the decision is answered elsewhere", async () => {
  const { daemon, watch, dialogs } = makeDaemon({ laterMs: 120 });
  await daemon.start();
  watch.push({ event: "created", machine: MACHINE, decision: decision("dec-1") });
  await waitUntil(() => dialogs.length === 1);
  dialogs[0].later();
  watch.push({ event: "resolved", machine: MACHINE, decision: { ...decision("dec-1"), status: "answered" } });
  await new Promise((r) => setTimeout(r, 250));
  assert.equal(dialogs.length, 1);
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
  assert.match(dialogs[1].spec.title, /dec-9 の題名/);
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
    spawnDialog: () => dialogSession(dialogs),
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

test("answer blocked by Tailscale re-approval is resent until it lands", async () => {
  let calls = 0;
  const { daemon, watch, dialogs, answers } = makeDaemon({
    answerRetryMs: 40,
    runAnswer: async () => {
      calls += 1;
      return calls < 3
        ? { code: 255, stdout: "", stderr: "# To authenticate, visit: https://login.tailscale.com/a/xyz" }
        : { code: 0, stdout: "{}", stderr: "" };
    },
  });
  await daemon.start();
  const remote = { id: "m0e51", label: "仕事Mac" };
  watch.push({ event: "created", machine: remote, decision: decision("dec-r") });
  await waitUntil(() => dialogs.length === 1);
  dialogs[0].answer(["a"]);
  await waitUntil(() => calls === 3);
  assert.ok(answers.every((argv) => argv.includes("m0e51")));
  await daemon.stop();
});

test("machine and decision keys isolate close, replay and disconnect", async () => {
  const { daemon, watch, dialogs, answers } = makeDaemon();
  await daemon.start();
  const remote = { id: "mini", label: "mini" };
  for (const [machine, id] of [[MACHINE, "same"], [remote, "same"], [remote, "other"]]) {
    watch.push({event: "created", machine, decision: decision(id)});
  }
  await waitUntil(() => dialogs.length === 3);
  const child = daemon.child;
  watch.push({event:"created", machine:MACHINE, decision:decision("same")});
  watch.push({event:"machine_down", machine:remote});
  await waitUntil(() => daemon.showing.size === 1);
  assert.equal(daemon.child, child);
  assert.equal(dialogs[0].killed, false);
  assert.equal(dialogs[1].killed, true);
  assert.equal(dialogs[2].killed, true);
  assert.equal(dialogs.length, 3);
  assert.equal(answers.length, 0);
  await daemon.stop();
  assert.equal(dialogs[0].killed, true);
  assert.equal(daemon.showing.size, 0);
});

test("replay while an answer waits for SSH does not reopen its window", async () => {
  const {daemon, watch, dialogs} = makeDaemon({answerRetryMs: 1000, runAnswer: async () => ({code:255, stdout:"", stderr:"https://login.tailscale.com/a/test"})});
  await daemon.start();
  watch.push({event:"created", machine:MACHINE, decision:decision("retry")});
  await waitUntil(() => dialogs.length === 1);
  dialogs[0].answer(["a"]);
  await waitUntil(() => daemon.entries.get("local retry")?.answerRetryTimer);
  watch.push({event:"created", machine:MACHINE, decision:decision("retry")});
  await new Promise(resolve => setTimeout(resolve, 30));
  assert.equal(dialogs.length, 1);
  assert.equal(daemon.showing.size, 0);
  await daemon.stop();
});
