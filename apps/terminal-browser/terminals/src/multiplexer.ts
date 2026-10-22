import { execFileSync } from "node:child_process";
import path from "node:path";
import type { Run } from "./run";
import type { Pane } from "./terminal";

export type Multiplexer = "herdr" | "ikesu" | "gtx";

export function currentMultiplexer(env: NodeJS.ProcessEnv): Multiplexer | null {
  if (env.HERDR_PANE_ID && env.IKESU_PANE_ID) {
    const rows = execFileSync("ps", ["-axo", "pid=,ppid=,comm="], { encoding: "utf8" });
    const processes = new Map<number, { parent: number; name: string }>();
    for (const row of rows.trim().split("\n")) {
      const match = row.trim().match(/^(\d+)\s+(\d+)\s+(.+)$/);
      if (match) processes.set(Number(match[1]), { parent: Number(match[2]), name: path.basename(match[3]) });
    }
    const visited = new Set<number>();
    for (let pid = process.ppid; pid > 1 && !visited.has(pid);) {
      visited.add(pid);
      const ancestor = processes.get(pid);
      if (!ancestor) break;
      if (ancestor.name === "herdr" || ancestor.name === "ikesu") return ancestor.name;
      pid = ancestor.parent;
    }
    throw new Error("Cannot establish the inner herdr/ikesu runtime from process ancestry; refusing inherited pane variables.");
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
