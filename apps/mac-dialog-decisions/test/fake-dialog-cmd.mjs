#!/usr/bin/env node
// Fake dialog helper for e2e: stands in for `osascript -l JavaScript dialog.js`.
//
//   node fake-dialog-cmd.mjs <specPath>
//
// Env:
//   MAC_DIALOG_FAKE_PLAN   path to a JSON map: spec-substring ->
//                          { action: "answer"|"later"|"wait", option_ids?, text? }
//                          Polled every 100ms so the test can "press the button"
//                          on an already-open dialog.
//   MAC_DIALOG_FAKE_DIR    directory where spawn/kill evidence files are written
//
// Emits {"event":"shown"} on open. When the plan gives a non-wait action for
// this spec it emits the result line and exits. Every invocation appends its
// spec to <FAKE_DIR>/specs.jsonl for assertions.

import fs from "node:fs";
import readline from "node:readline";

const planPath = process.env.MAC_DIALOG_FAKE_PLAN || "";
const evidenceDir = process.env.MAC_DIALOG_FAKE_DIR || "";
const windows = new Map();
const emit = (obj) => process.stdout.write(JSON.stringify(obj) + "\n");
function close(key) {
  windows.delete(key);
  try { fs.unlinkSync(`${evidenceDir}/alive-${Buffer.from(key).toString("hex")}`); } catch {}
}
const input = readline.createInterface({input: process.stdin});
input.on("line", raw => {
  const command = JSON.parse(raw);
  if (command.command === "close") { close(command.key); return; }
  if (command.command !== "open" || windows.has(command.key)) return;
  const spec = JSON.parse(fs.readFileSync(command.spec_path, "utf8"));
  windows.set(command.key, spec);
  fs.mkdirSync(evidenceDir, {recursive:true});
  fs.appendFileSync(`${evidenceDir}/specs.jsonl`, JSON.stringify({pid:process.pid, spec}) + "\n");
  fs.writeFileSync(`${evidenceDir}/alive-${Buffer.from(command.key).toString("hex")}`, "open");
  emit({event:"shown", key:command.key});
});
input.on("close", () => process.exit(0));
process.on("SIGTERM", () => process.exit(0));
process.on("exit", () => { for (const key of windows.keys()) close(key); });

const timer = setInterval(() => {
  let plan = {};
  try {
    plan = JSON.parse(fs.readFileSync(planPath, "utf8"));
  } catch {}
  for (const [key, spec] of windows) {
  const haystack = `${spec.title}\n${spec.body || ""}`;
  const action = Object.entries(plan).find(([needle]) =>
    haystack.includes(needle),
  )?.[1];
  if (!action || action.action === "wait") continue;
  if (action.action === "answer") {
    emit({
      key, result: "answer",
      option_ids: action.option_ids || [],
      text: action.text || "",
    });
    close(key);
  }
  if (action.action === "later") {
    emit({ key, result: "later" });
    close(key);
  }
  }
}, 100);
