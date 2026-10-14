// Spec rendering tests: the panel spec must carry every decision field to the
// screen with no truncation, and lead with where the decision comes from.

import { test } from "node:test";
import assert from "node:assert/strict";
import {
  buildDialogSpec,
  fmtDeadline,
  locationLabel,
} from "../mac-dialog-decisions.mjs";

function decision(overrides = {}) {
  return {
    decision_id: "dec-abc",
    kind: "command_guard",
    title: "rm -rf ./dist を実行してよい？",
    body: "guard が danger_words に一致しました",
    question_label: "質問 2/3",
    multi_select: false,
    allow_text: false,
    expires_unix_ms: Date.now() + 120_000,
    origin: {
      machine: "macbook",
      workspace_label: "work",
      tab_label: "t1",
      pane_label: "deploy",
      pane_id: "p1",
      agent: "claude",
      cwd: "/repo",
      branch: "main",
      command: "rm -rf ./dist",
      reason: "danger_words",
    },
    options: [
      { id: "approve", label: "承認する", description: "このコマンドを実行", preview: "$ rm -rf ./dist" },
      { id: "allow-day", label: "1日許可" },
      { id: "reject", label: "却下する", preview_unavailable: true },
    ],
    ...overrides,
  };
}

const machine = { id: "local", label: "Local" };

test("spec carries origin, question and every option field without truncation", () => {
  const spec = buildDialogSpec(decision(), machine, 0, { daemonPid: 42 });
  assert.equal(spec.window_title, "herdr 判断待ち — work");
  assert.equal(spec.source, "🤖 claude");
  assert.equal(spec.location, "work › t1 › deploy");
  assert.equal(spec.machine, "");
  assert.equal(spec.question_label, "質問 2/3");
  assert.equal(spec.title, "rm -rf ./dist を実行してよい？");
  assert.equal(spec.body, "guard が danger_words に一致しました");
  const details = Object.fromEntries(spec.details.map((d) => [d.label, d.value]));
  assert.deepEqual(details, {
    コマンド: "rm -rf ./dist",
    場所: "/repo",
    ブランチ: "main",
    理由: "danger_words",
    ペイン: "p1",
  });
  assert.deepEqual(spec.options[0], {
    id: "approve",
    label: "承認する",
    description: "このコマンドを実行",
    preview: "$ rm -rf ./dist",
    preview_unavailable: false,
  });
  assert.equal(spec.options[1].label, "1日許可");
  assert.equal(spec.options[2].preview_unavailable, true);
  assert.match(spec.deadline, /^期限 \d{2}:\d{2}（残り約2分）$/);
  assert.equal(spec.daemon_pid, 42);
  assert.equal(spec.later_label, "10分");
});

test("long body and previews are preserved verbatim", () => {
  const longBody = "あ".repeat(5000) + "\n" + "B".repeat(3000);
  const longPreview = "preview-" + "x".repeat(4000);
  const spec = buildDialogSpec(
    decision({ body: longBody, options: [{ id: "ok", label: "OK", preview: longPreview }] }),
    machine,
  );
  assert.equal(spec.body, longBody);
  assert.equal(spec.options[0].preview, longPreview);
});

test("a saved machine is named; screen prompts hide the internal reason", () => {
  const remote = buildDialogSpec(
    decision({ origin: { ...decision().origin, reason: "screen_prompt" } }),
    { id: "m0e51", label: "仕事Mac" },
  );
  assert.equal(remote.machine, "仕事Mac");
  assert.ok(!remote.details.some((d) => d.label === "理由"));
});

test("queue count, flags and later minutes reach the spec", () => {
  const spec = buildDialogSpec(decision({ multi_select: true, allow_text: true }), machine, 2, {
    laterMs: 5 * 60_000,
  });
  assert.equal(spec.queue_remaining, 2);
  assert.equal(spec.multi_select, true);
  assert.equal(spec.allow_text, true);
  assert.equal(spec.later_label, "5分");
  assert.equal(buildDialogSpec(decision({ expires_unix_ms: null }), machine).deadline, "");
});

test("location falls back to the directory name when there is no space label", () => {
  assert.equal(locationLabel(decision()), "work › t1 › deploy");
  assert.equal(locationLabel(decision({ origin: { cwd: "/src/herdr" } })), "herdr");
  assert.equal(locationLabel(decision({ origin: {} })), "");
  const bare = buildDialogSpec(decision({ origin: { cwd: "/src/herdr" } }), machine);
  assert.equal(bare.window_title, "herdr 判断待ち — herdr");
  assert.equal(bare.source, "🛡 危険コマンドの承認");
});

test("fmtDeadline shows absolute time and remaining; no deadline when absent", () => {
  const now = Date.now();
  const d = decision({ expires_unix_ms: now + 5 * 60 * 1000 });
  assert.match(fmtDeadline(d, now), /^\d{2}:\d{2}（残り約5分）$/);
  assert.equal(fmtDeadline(decision({ expires_unix_ms: null }), now), "期限なし");
});
