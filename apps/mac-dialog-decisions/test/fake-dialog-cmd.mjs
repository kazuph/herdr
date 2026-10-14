#!/usr/bin/env node
// Fake dialog helper for e2e: stands in for `osascript -l JavaScript dialog.js`.
//
//   node fake-dialog-cmd.mjs <specPath>
//
// Env:
//   MAC_DIALOG_FAKE_PLAN   path to a JSON map: spec-substring ->
//                          { action: "answer"|"dismiss"|"wait", option_ids?, text? }
//                          Polled every 100ms so the test can "press the button"
//                          on an already-open dialog.
//   MAC_DIALOG_FAKE_DIR    directory where spawn/kill evidence files are written
//
// Emits {"event":"shown"} on open. When the plan gives a non-wait action for
// this spec it emits the result line and exits. Every invocation appends its
// spec to <FAKE_DIR>/specs.jsonl for assertions.

import fs from "node:fs";

const specPath = process.argv[2];
const planPath = process.env.MAC_DIALOG_FAKE_PLAN || "";
const evidenceDir = process.env.MAC_DIALOG_FAKE_DIR || "";

const spec = JSON.parse(fs.readFileSync(specPath, "utf8"));

if (evidenceDir) {
  fs.mkdirSync(evidenceDir, { recursive: true });
  fs.appendFileSync(
    `${evidenceDir}/specs.jsonl`,
    JSON.stringify({ pid: process.pid, spec }) + "\n",
  );
  fs.writeFileSync(`${evidenceDir}/alive-${process.pid}`, String(Date.now()));
  process.on("SIGTERM", () => process.exit(0));
  process.on("exit", () => {
    try {
      fs.unlinkSync(`${evidenceDir}/alive-${process.pid}`);
    } catch {}
  });
}

const emit = (obj) => process.stdout.write(JSON.stringify(obj) + "\n");
emit({ event: "shown" });

const timer = setInterval(() => {
  let plan = {};
  try {
    plan = JSON.parse(fs.readFileSync(planPath, "utf8"));
  } catch {}
  const haystack = `${spec.window_title}\n${spec.text}`;
  const action = Object.entries(plan).find(([needle]) =>
    haystack.includes(needle),
  )?.[1];
  if (!action || action.action === "wait") return;
  if (action.action === "answer") {
    emit({
      result: "answer",
      option_ids: action.option_ids || [],
      text: action.text || "",
    });
    process.exit(0);
  }
  if (action.action === "dismiss") {
    emit({ result: "dismiss" });
    process.exit(0);
  }
}, 100);
