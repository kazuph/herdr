// Unit tests for buildDecisionBlocks option descriptions and block splitting.
// No network: the block builders are pure functions.

import { test } from "node:test";
import assert from "node:assert/strict";
import {
  buildDecisionBlocks,
  fallbackText,
} from "../slack-decisions.mjs";

const LOCAL = { id: "local", label: "Local" };

const SLACK_HARD_LIMIT = 3000;

function pendingDecision(overrides = {}) {
  return {
    decision_id: "dec-desc",
    title: "どう進める？",
    body: "",
    allow_text: false,
    status: "pending",
    created_unix_ms: 1_000,
    expires_unix_ms: 4_000_000_000_000,
    options: [],
    ...overrides,
  };
}

function sectionTexts(blocks) {
  return blocks
    .filter((block) => block.type === "section")
    .map((block) => block.text.text);
}

test("option descriptions render under their labels", () => {
  const decision = pendingDecision({
    options: [
      { id: "1", label: "取り込んで入れ直す", description: "店長の Mac の gtx を入れ直す" },
      { id: "2", label: "取り込むだけ", description: "gtx は今のままにしておく" },
      { id: "3", label: "まだ取り込まない" },
    ],
  });
  const blocks = buildDecisionBlocks({ decision, machine: LOCAL });
  const text = sectionTexts(blocks).join("\n");
  assert.match(text, /:one: 取り込んで入れ直す\n> 店長の Mac の gtx を入れ直す/);
  assert.match(text, /:two: 取り込むだけ\n> gtx は今のままにしておく/);
  // Options without a description still render their label only.
  assert.match(text, /:three: まだ取り込まない/);
});

test("long option descriptions split across blocks without losing text", () => {
  // A multi-paragraph description that cannot fit a single section.
  const parts = Array.from(
    { length: 400 },
    (_, index) => `説明文の断片 ${index} あいうえお`,
  );
  const decision = pendingDecision({
    options: [
      { id: "1", label: "長い説明つき", description: parts.join("\n") },
      { id: "2", label: "ふつう", description: "おわりの説明" },
    ],
  });
  const blocks = buildDecisionBlocks({ decision, machine: LOCAL });
  const sections = sectionTexts(blocks);
  const optionSections = sections.filter((text) => text.includes(":one:") || text.includes("断片") || text.includes(":two:"));
  assert.ok(optionSections.length > 1, "expected the description to span multiple sections");
  for (const text of optionSections) {
    assert.ok(
      text.length <= SLACK_HARD_LIMIT,
      `section exceeds slack limit: ${text.length}`,
    );
  }
  // Every fragment is present somewhere — nothing truncated.
  const all = optionSections.join("\n");
  for (const part of ["断片 0 ", "断片 399 ", "おわりの説明"]) {
    assert.ok(all.includes(part), `missing ${part}`);
  }
  // Multi-line descriptions keep quoted continuation lines.
  const first = optionSections.find((text) => text.includes(":one:"));
  assert.match(first, /\n> /);
});

test("fallback text carries option descriptions", () => {
  const decision = pendingDecision({
    options: [
      { id: "1", label: "はい", description: "一度だけ実行する" },
      { id: "2", label: "いいえ" },
    ],
  });
  const text = fallbackText({ decision, machine: LOCAL });
  assert.match(text, /1\. はい\n   一度だけ実行する/);
  assert.match(text, /2\. いいえ/);
});

test("origin labels and question_label appear in the post", () => {
  const decision = pendingDecision({
    question_label: "質問 1/2 追加機能",
    origin: {
      machine: "darwin-mini",
      workspace_label: "pv-src",
      tab_label: "1",
      pane_label: "pv-claude5",
      pane_id: "p8",
      agent: "claude",
      cwd: "/tmp/repo",
      command: "just test",
      reason: "screen prompt",
    },
    options: [{ id: "1", label: "はい" }],
  });
  const blocks = buildDecisionBlocks({ decision, machine: LOCAL });
  const all = blocks
    .map((block) => (block.text?.text || block.elements?.[0]?.text || ""))
    .join("\n");
  assert.match(all, /質問 1\/2 追加機能/);
  assert.match(all, /\*出どころマシン\* darwin-mini/);
  assert.match(all, /\*スペース\* pv-src/);
  assert.match(all, /\*タブ\* 1/);
  assert.match(all, /\*ペイン\* pv-claude5/);
  assert.match(all, /\*ペインID\* `p8`/);
  assert.match(all, /\*エージェント\* `claude`/);
});

test("decision without labels omits those lines", () => {
  const decision = pendingDecision({
    origin: { machine: "darwin-mini" },
    options: [{ id: "1", label: "はい" }],
  });
  const blocks = buildDecisionBlocks({ decision, machine: LOCAL });
  const all = blocks
    .map((block) => (block.text?.text || block.elements?.[0]?.text || ""))
    .join("\n");
  assert.match(all, /\*出どころマシン\* darwin-mini/);
  assert.doesNotMatch(all, /スペース|タブ\*|\*ペイン\*/);
});

test("multi_select hint explains number reactions plus white_check_mark confirm", () => {
  const decision = pendingDecision({
    multi_select: true,
    allow_text: true,
    options: [
      { id: "1", label: "検索" },
      { id: "2", label: "通知" },
    ],
  });
  const blocks = buildDecisionBlocks({ decision, machine: LOCAL });
  const context = blocks
    .filter((block) => block.type === "context")
    .flatMap((block) => block.elements.map((el) => el.text))
    .join("\n");
  assert.match(context, /:white_check_mark: で確定/);
  assert.match(context, /「1,3」/);
  assert.match(context, /自由記述はスレッドに文で返信/);
});

test("option previews render in code blocks and unavailable is explicit", () => {
  const decision = pendingDecision({
    options: [
      { id: "1", label: "A", preview: "+---+\n| A |\n+---+" },
      { id: "2", label: "B", preview_unavailable: true },
    ],
  });
  const blocks = buildDecisionBlocks({ decision, machine: LOCAL });
  const text = sectionTexts(blocks).join("\n");
  assert.match(text, /:one: A\n```\n\+---\+\n\| A \|\n\+---\+\n```/);
  assert.match(text, /:two: B\n> プレビューあり（画面で確認）/);
});
