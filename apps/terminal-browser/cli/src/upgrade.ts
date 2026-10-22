import { spawn } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { currentMultiplexer, multiplexerBin, detect } from "pixel-terminals";

export function installedVersion(): string | null {
  const root = process.env.TERMINAL_BROWSER_DIST_ROOT;
  if (!root) return null;
  try {
    return fs.readFileSync(path.join(root, "VERSION"), "utf8").trim() || null;
  } catch {
    return null;
  }
}

export function upgradeTarget(env: NodeJS.ProcessEnv = process.env): { runtime: string; bin: string; args: string[] } {
  const runtime = currentMultiplexer(env) ?? "herdr";
  return { runtime, bin: multiplexerBin(runtime, env), args: ["tb", "install"] };
}

export async function upgradeCommand(): Promise<number> {
  const { runtime, bin, args } = upgradeTarget();
  if (process.env.IKESU_PANE_ID) await detect()?.getCurrentPane?.({ tty: null, cwd: process.cwd() });
  const child = spawn(bin, args, { stdio: "inherit" });
  return new Promise((resolve, reject) => {
    child.on("error", (error) => reject(new Error(`Cannot update bundled tb: ${runtime} tb install could not start. Install ${runtime} and add it to PATH. ${error.message}`)));
    child.on("exit", (code) => resolve(code ?? 1));
  });
}
