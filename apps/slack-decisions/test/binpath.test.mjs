// resolveHerdrBin / createRunAnswer: a missing HERDR_BIN_PATH (e.g. a
// versioned Homebrew Cellar path removed by `brew upgrade`) must fall back
// to PATH and the usual prefixes instead of failing every spawn.

import { test } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { createRunAnswer, resolveHerdrBin } from "../slack-decisions.mjs";

const GONE = "/opt/homebrew/Cellar/herdr/0.2.35/bin/herdr";

function fakeBinDir() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "herdr-bin-"));
  const bin = path.join(dir, "herdr");
  fs.writeFileSync(bin, "#!/bin/sh\necho fake-herdr $*\n", { mode: 0o755 });
  return { dir, bin };
}

test("missing HERDR_BIN_PATH falls back to a herdr on PATH", () => {
  const { dir, bin } = fakeBinDir();
  const originalPath = process.env.PATH;
  process.env.PATH = `${dir}${path.delimiter}${originalPath}`;
  try {
    assert.equal(
      resolveHerdrBin({ herdrBin: GONE }),
      bin,
    );
  } finally {
    process.env.PATH = originalPath;
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("existing HERDR_BIN_PATH wins over the fallbacks", () => {
  const { dir, bin } = fakeBinDir();
  try {
    assert.equal(resolveHerdrBin({ herdrBin: bin }), bin);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("runAnswer still sends the answer when HERDR_BIN_PATH is gone", async () => {
  const { dir } = fakeBinDir();
  const originalPath = process.env.PATH;
  process.env.PATH = `${dir}${path.delimiter}${originalPath}`;
  try {
    const runAnswer = createRunAnswer({ herdrBin: GONE });
    const result = await runAnswer(["decision", "answer", "dec-x", "1"]);
    assert.equal(result.code, 0);
    assert.match(result.stdout, /fake-herdr decision answer dec-x 1/);
  } finally {
    process.env.PATH = originalPath;
    fs.rmSync(dir, { recursive: true, force: true });
  }
});
