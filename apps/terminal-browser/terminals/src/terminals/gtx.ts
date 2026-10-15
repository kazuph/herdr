import { shellQuote } from "../shared";
import type { Detect, Direction } from "../terminal";

interface GtxPaneSplitResult {
  result: { pane: { pane_id: string } };
}

const NATIVE_DIRECTION: Record<Direction, "right" | "down"> = {
  right: "right",
  left: "right",
  down: "down",
  up: "down",
};

const OPPOSITE: Record<"right" | "down", "left" | "up"> = { right: "left", down: "up" };

export const gtx: Detect = (env, run) => {
  if (!env.GTX_PANE_ID) return null;

  const bin = env.GTX_BIN_PATH || "gtx";
  const gtx = (args: string[]) => run(bin, args);

  return {
    name: "gtx",
    getCurrentPane: async () => ({ id: env.GTX_PANE_ID!, tab: env.GTX_TAB_ID! }),
    async split({ from, direction, command, size }) {
      const native = NATIVE_DIRECTION[direction];
      const ratio = size !== null ? ["--ratio", String(size)] : [];
      const { result } = JSON.parse(await gtx([
        "pane", "split", "--pane", from.id, "--direction", native, "--focus", ...ratio,
      ])) as GtxPaneSplitResult;
      const newPaneId = result.pane.pane_id;
      if (direction === "left" || direction === "up") {
        await gtx(["pane", "swap", "--pane", newPaneId, "--direction", OPPOSITE[native]]);
      }
      await gtx(["pane", "run", newPaneId, shellQuote(command)]);
    },
  };
};
