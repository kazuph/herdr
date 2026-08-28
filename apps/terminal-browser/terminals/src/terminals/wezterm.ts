import { paneById } from "../shared";
import type { Detect, Pane } from "../terminal";

interface WeztermPane {
  window_id: number;
  tab_id: number;
  pane_id: number;
  title: string;
}

const SPLIT_FLAG = { right: "--right", left: "--left", down: "--bottom", up: "--top" } as const;

export const wezterm: Detect = (env, run) => {
  if (env.TERM_PROGRAM !== "WezTerm" && !env.WEZTERM_PANE) return null;

  const wezterm = (args: string[]) => run("wezterm", args);

  async function panes(): Promise<Pane[]> {
    const list = JSON.parse(await wezterm(["cli", "list", "--format", "json"])) as WeztermPane[];
    return list.map((pane) => ({
      id: String(pane.pane_id),
      tab: `${pane.window_id}:${pane.tab_id}`,
    }));
  }

  return {
    name: "wezterm",
    getCurrentPane: () => paneById(panes, env.WEZTERM_PANE),
    async split({ direction, command }) {
      await wezterm(["cli", "split-pane", SPLIT_FLAG[direction], "--", ...command]);
    },
  };
};
