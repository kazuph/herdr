// Spec/text rendering tests: the dialog spec must carry every decision field
// to the screen with no truncation.

import { test } from "node:test";
import assert from "node:assert/strict";
import {
  buildDialogSpec,
  buildDialogText,
  fmtDeadline,
  originBreadcrumb,
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

test("dialog text carries every decision field without truncation", () => {
  const d = decision();
  const text = buildDialogText(d, machine, 0);
  for (const required of [
    "【判断待ち】",
    "質問 2/3",
    "■ rm -rf ./dist を実行してよい？",
    "guard が danger_words に一致しました",
    "Local › work › t1 › deploy（claude）",
    "ペインID: p1",
    "ディレクトリ: /repo",
    "ブランチ: main",
    "コマンド: rm -rf ./dist",
    "理由: danger_words",
    "マシン: macbook",
    "1. 承認する",
    "このコマンドを実行",
    "$ rm -rf ./dist",
    "2. 1日許可",
    "3. 却下する",
    "プレビューあり（画面で確認）",
    "期限:",
    "残り約",
    "判断待ちID: dec-abc",
  ]) {
    assert.ok(text.includes(required), `missing: ${required}`);
  }
});

test("long body and previews are preserved verbatim", () => {
  const longBody = "あ".repeat(5000) + "\n" + "B".repeat(3000);
  const longPreview = "preview-" + "x".repeat(4000);
  const d = decision({
    body: longBody,
    options: [{ id: "ok", label: "OK", preview: longPreview }],
  });
  const text = buildDialogText(d, machine, 0);
  assert.ok(text.includes(longBody));
  assert.ok(text.includes(`   ${longPreview}`));
});

test("multi_select adds the checkbox hint, allow_text the input hint", () => {
  const multi = buildDialogText(decision({ multi_select: true }), machine, 0);
  assert.match(multi, /複数選択/);
  const textable = buildDialogText(decision({ allow_text: true }), machine, 0);
  assert.match(textable, /自由入力/);
});

test("queue remaining count appears in text and window title", () => {
  const d = decision();
  assert.match(buildDialogText(d, machine, 2), /ほかに 2 件の判断待ち/);
  const spec = buildDialogSpec(d, machine, 2);
  assert.match(spec.window_title, /ほか2件/);
  assert.match(spec.window_title, /質問 2\/3/);
  assert.equal(spec.options.length, 3);
  assert.equal(spec.options[0].id, "approve");
  assert.equal(spec.options[0].title, "1. 承認する");
  assert.equal(spec.multi_select, false);
  assert.equal(spec.allow_text, false);
});

test("origin breadcrumb degrades gracefully when fields are missing", () => {
  assert.equal(originBreadcrumb(decision(), machine), "Local › work › t1 › deploy（claude）");
  const bare = decision({ origin: {} });
  assert.equal(originBreadcrumb(bare, machine), "Local");
  const agentOnly = decision({ origin: { agent: "codex" } });
  assert.equal(originBreadcrumb(agentOnly, { id: "m1", label: "M1" }), "M1 › codex");
});

test("fmtDeadline shows absolute time and remaining; no deadline when absent", () => {
  const now = Date.now();
  const d = decision({ expires_unix_ms: now + 5 * 60 * 1000 });
  assert.match(fmtDeadline(d, now), /^\d{2}:\d{2}（残り約5分）$/);
  assert.equal(fmtDeadline(decision({ expires_unix_ms: null }), now), "期限なし");
});
