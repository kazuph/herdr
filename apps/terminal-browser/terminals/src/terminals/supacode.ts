import { paneById, shellQuote } from "../shared";
import type { Detect, Pane } from "../terminal";

export const supacode: Detect = (env, run) => {
  if (!env.SUPACODE_SURFACE_ID && env.TERM_PROGRAM !== "supacode") return null;

  const binary = "supacode";
  const supacode = (args: string[]) => run(binary, args);
  const lines = async (args: string[]) =>
    (await supacode(args))
      .split("\n")
      .map((line) => line.trim())
      .filter((line) => line !== "");

  async function panes(): Promise<Pane[]> {
    const found: Pane[] = [];
    for (const tab of await lines(["tab", "list"])) {
      for (const surface of await lines(["surface", "list", "--tab", tab])) {
        found.push({ id: surface, tab });
      }
    }
    return found;
  }

  return {
    name: "supacode",
    getCurrentPane: () => paneById(panes, env.SUPACODE_SURFACE_ID),
    async split({ from, direction, command }) {
      if (direction !== "right" && direction !== "down") {
        throw new Error("supacode only splits right and down");
      }
      await supacode([
        "surface",
        "split",
        "--surface",
        from.id,
        "--direction",
        direction === "right" ? "horizontal" : "vertical",
        "--input",
        shellQuote(command),
      ]);
    },
  };
};
