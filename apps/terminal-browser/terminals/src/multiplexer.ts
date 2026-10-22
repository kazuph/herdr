import { execFileSync } from "node:child_process";
import path from "node:path";
import fs from "node:fs";
import type { Run } from "./run";
import type { Pane } from "./terminal";

export type Multiplexer = "herdr" | "ikesu" | "gtx";

export class UnknownMultiplexerError extends Error {}

function executablePath(name: "herdr" | "ikesu", env: NodeJS.ProcessEnv): string | null {
  const explicit = env[`${name.toUpperCase()}_BIN_PATH`];
  const candidates = explicit ? [explicit] : (env.PATH ?? process.env.PATH ?? "").split(path.delimiter).map(dir => path.join(dir, name));
  for (const candidate of candidates) {
    try {
      fs.accessSync(candidate, fs.constants.X_OK);
      return fs.realpathSync(candidate);
    } catch {}
  }
  return null;
}

function processExecutable(pid: number): string {
  if (process.platform === "linux") return fs.realpathSync(`/proc/${pid}/exe`);
  const files = execFileSync("/usr/sbin/lsof", ["-a", "-p", String(pid), "-d", "txt", "-Fn"], { encoding: "utf8" });
  const executable = files.split("\n").find(line => line.startsWith("n"))?.slice(1);
  if (!executable) throw new Error(`Executable for ancestor ${pid} is unavailable`);
  return fs.realpathSync(executable);
}

export function currentMultiplexer(env: NodeJS.ProcessEnv): Multiplexer | null {
  if (env.HERDR_PANE_ID && env.IKESU_PANE_ID) {
    try {
      const expected = { herdr: executablePath("herdr", env), ikesu: executablePath("ikesu", env) };
      const rows = execFileSync("/bin/ps", ["-axo", "pid=,ppid=,comm="], { encoding: "utf8" });
      const processes = new Map<number, { parent: number; executable: string }>();
      for (const row of rows.trim().split("\n")) {
        const match = row.trim().match(/^(\d+)\s+(\d+)\s+(.+)$/);
        if (match) processes.set(Number(match[1]), { parent: Number(match[2]), executable: match[3] });
      }
      const visited = new Set<number>();
      for (let pid = process.ppid; pid > 1 && !visited.has(pid);) {
        visited.add(pid);
        const ancestor = processes.get(pid);
        if (!ancestor) break;
        const real = processExecutable(pid);
        for (const name of ["herdr", "ikesu"] as const) {
          if (real === expected[name] || (!env[`${name.toUpperCase()}_BIN_PATH`] && path.basename(ancestor.executable) === name)) return name;
        }
        pid = ancestor.parent;
      }
    } catch (error) {
      throw new UnknownMultiplexerError(`Cannot establish the inner herdr/ikesu runtime from executable ancestry. ${error instanceof Error ? error.message : String(error)}`);
    }
    throw new UnknownMultiplexerError("Cannot establish the inner herdr/ikesu runtime from executable ancestry; refusing inherited pane variables.");
  }
  if (env.IKESU_PANE_ID) return "ikesu";
  if (env.GTX_PANE_ID) return "gtx";
  if (env.HERDR_PANE_ID) return "herdr";
  return null;
}

export function multiplexerBin(name: Multiplexer, env: NodeJS.ProcessEnv): string {
  return env[`${name.toUpperCase()}_BIN_PATH`] || name;
}

export async function verifyPane(name: Multiplexer, env: NodeJS.ProcessEnv, run: Run): Promise<Pane> {
  const id = env[`${name.toUpperCase()}_PANE_ID`];
  try {
    const reply = JSON.parse(await run(multiplexerBin(name, env), ["pane", "get", id!]));
    const pane = reply.result?.pane;
    if (reply.error || typeof pane?.pane_id !== "string" || typeof pane?.tab_id !== "string") throw new Error("pane get did not return a pane");
    return { id: pane.pane_id, tab: pane.tab_id };
  } catch (error) {
    throw new Error(`Cannot verify ${name} pane ${id}; refusing to use another runtime. ${error instanceof Error ? error.message : String(error)}`);
  }
}
