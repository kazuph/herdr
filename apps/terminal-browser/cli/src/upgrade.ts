import { spawn } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { currentMultiplexer, multiplexerBin, UnknownMultiplexerError, verifyPane, shellIn } from "pixel-terminals";

export function installedVersion(): string | null {
  const root = process.env.TERMINAL_BROWSER_DIST_ROOT;
  if (!root) return null;
  try {
    return fs.readFileSync(path.join(root, "VERSION"), "utf8").trim() || null;
  } catch {
    return null;
  }
}

function resolveUpgrade(env: NodeJS.ProcessEnv) {
  let selected;
  try { selected = currentMultiplexer(env); }
  catch (error) {
    if (!(error instanceof UnknownMultiplexerError)) throw error;
    selected = null;
  }
  const runtime = selected ?? "herdr";
  return { selected, runtime, bin: multiplexerBin(runtime, env), args: ["tb", "install"] };
}

export function upgradeTarget(env: NodeJS.ProcessEnv = process.env): { runtime: string; bin: string; args: string[] } {
  const { selected, ...target } = resolveUpgrade(env);
  return target;
}

export async function upgradeCommand(): Promise<number> {
  const { selected, runtime, bin, args } = resolveUpgrade(process.env);
  if (selected && process.env.IKESU_PANE_ID) await verifyPane(selected, process.env, shellIn(process.env));
  const child = spawn(bin, args, { stdio: "inherit" });
  return new Promise((resolve, reject) => {
    child.on("error", (error) => reject(new Error(`Cannot update bundled tb: ${runtime} tb install could not start. Install ${runtime} and add it to PATH. ${error.message}`)));
    child.on("exit", (code) => resolve(code ?? 1));
  });
}
