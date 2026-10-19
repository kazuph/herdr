import { spawn } from "node:child_process";
import fs from "node:fs";
import path from "node:path";

export function installedVersion(): string | null {
  const root = process.env.TERMINAL_BROWSER_DIST_ROOT;
  if (!root) return null;
  try {
    return fs.readFileSync(path.join(root, "VERSION"), "utf8").trim() || null;
  } catch {
    return null;
  }
}

export async function upgradeCommand(): Promise<number> {
  // Herdr installs beside running versions; open browsers keep their executable.
  const child = spawn("herdr", ["tb", "install"], { stdio: "inherit" });
  return new Promise((resolve, reject) => {
    child.on("error", (error) => reject(new Error(`Cannot update bundled tb: herdr tb install could not start. Install herdr and add it to PATH. ${error.message}`)));
    child.on("exit", (code) => resolve(code ?? 1));
  });
}
