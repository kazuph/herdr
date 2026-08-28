import { execFileSync } from "node:child_process";

import { paneById, shellQuote } from "../shared";
import type { Detect, Pane } from "../terminal";

const SPLIT_FLAG = { right: "-h", left: "-h", down: "-v", up: "-v" } as const;

export const tmux: Detect = (env, run) => {
  if (!env.TMUX) return null;

  const tmux = (args: string[]) => run("tmux", args);

  async function panes(): Promise<Pane[]> {
    const listing = await tmux([
      "list-panes",
      "-a",
      "-F",
      "#{session_name}\t#{window_id}\t#{pane_id}\t#{pane_title}",
    ]);
    const panes: Pane[] = [];
    for (const line of listing.split("\n")) {
      if (!line.trim()) continue;
      const [session, window, pane, ...title] = line.split("\t");
      panes.push({ id: pane, tab: `${session}:${window}` });
    }
    return panes;
  }

  return {
    name: "tmux",
    wrapper: "tmux",
    prepare: () => prepareTmux(env),
    getCurrentPane: () => paneById(panes, env.TMUX_PANE),
    async split({ from, direction, command, size }) {
      const before = direction === "left" || direction === "up" ? ["-b"] : [];
      const length = size ? ["-l", `${Math.round(size * 100)}%`] : [];
      await tmux([
        "split-window",
        SPLIT_FLAG[direction],
        ...before,
        ...length,
        "-t",
        from.id,
        shellQuote(command),
      ]);
    },
  };
};

function prepareTmux(env: NodeJS.ProcessEnv): void {
  const tmux = (args: string[]): string | null => {
    try {
      return execFileSync("tmux", args, { env, encoding: "utf8", timeout: 5000 }).trim();
    } catch {
      return null;
    }
  };

  tmux(["set", "-p", "allow-passthrough", "on"]);
  tmux(["set", "-s", "focus-events", "on"]);
  tmux(["set", "-s", "extended-keys", "on"]);
  tmux(["set", "-s", "extended-keys-format", "csi-u"]);
  const termname = tmux(["display", "-p", "#{client_termname}"]);
  if (termname) tmux(["set", "-as", "terminal-features", `${termname}:extkeys`]);
}
