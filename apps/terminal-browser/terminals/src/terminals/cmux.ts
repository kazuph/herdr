import { paneById, shellQuote } from "../shared";
import type { Detect, Pane } from "../terminal";

interface Listed {
  id?: string;
  ref?: string;
  title?: string;
}

export const cmux: Detect = (env, run) => {
  if (!env.CMUX_SURFACE_ID) return null;

  const binary = env.CMUX_BUNDLED_CLI_PATH ?? "cmux";
  const cmux = (args: string[]) => run(binary, args);
  const asked = async (args: string[]) =>
    JSON.parse(await cmux([...args, "--json", "--id-format", "both"]));

  async function panes(): Promise<Pane[]> {
    const listed: Listed[] = (await asked(["list-panes"])).panes ?? [];
    const found: Pane[] = [];
    for (const pane of listed) {
      const paneId = pane.id ?? pane.ref;
      if (!paneId) continue;
      const surfaces: Listed[] = (await asked(["list-pane-surfaces", "--pane", paneId]))
        .surfaces ?? [];
      for (const surface of surfaces) {
        if (!surface.id) continue;
        found.push({ id: surface.id, tab: env.CMUX_WORKSPACE_ID ?? "" });
      }
    }
    return found;
  }

  return {
    name: "cmux",
    getCurrentPane: () => paneById(panes, env.CMUX_SURFACE_ID),
    async split({ from, direction, command }) {
      const created = await asked(["new-split", direction, "--surface", from.id, "--focus", "true"]);
      const opened = created.surface_id ?? created.surface_ref;
      if (!opened) throw new Error("cmux opened a split but did not say which surface it is");
      await cmux(["send", "--surface", opened, "--", `${shellQuote(command)}\\n`]);
    },
  };
};
