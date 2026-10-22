import { SKIP_ENV, probeGraphics } from "./graphics";
import type { GraphicsSupport } from "./graphics";
import { shellIn } from "./run";
import type { Run } from "./run";
import type { Terminal } from "./terminal";
import { currentMultiplexer, verifyPane } from "./multiplexer";
import { ikesu } from "./terminals/ikesu";
import { herdr } from "./terminals/herdr";
import { TERMINALS } from "./terminals";

export function detect(env: NodeJS.ProcessEnv = process.env, run?: Run): Terminal | null {
  const runIn = run ?? shellIn(env);
  const multiplexer = currentMultiplexer(env);
  if (multiplexer === "ikesu") return ikesu(env, runIn);
  if (multiplexer === "herdr" && env.IKESU_PANE_ID) {
    const terminal = herdr(env, runIn)!;
    return {
      ...terminal,
      async prepare() { await verifyPane("herdr", env, runIn); await terminal.prepare?.(); },
      async getCurrentPane() { return verifyPane("herdr", env, runIn); },
      async split(request) { await verifyPane("herdr", env, runIn); return terminal.split!(request); },
    };
  }
  for (const recognise of TERMINALS) {
    const terminal = recognise(env, runIn);
    if (terminal) return terminal;
  }
  return null;
}

export interface TerminalCheck {
  terminal: Terminal | null;
  graphics: GraphicsSupport;
}

export async function checkTerminal(
  terminal: Terminal | null,
  env: NodeJS.ProcessEnv = process.env,
): Promise<TerminalCheck> {
  await terminal?.prepare?.();
  if (env[SKIP_ENV]) return { terminal, graphics: "supported" };
  const probed = await probeGraphics(terminal);
  const graphics = probed === "unknown" ? (terminal ? "supported" : "unsupported") : probed;
  return { terminal, graphics };
}

export function cannotOpenPanes(terminal: Terminal | null): string {
  const who = terminal
    ? `${terminal.name} cannot open panes from a command`
    : "terminal-browser does not recognise this terminal";
  const recommendation =
    process.platform === "darwin"
      ? "We recommend Ghostty (https://ghostty.org/download)."
      : "We recommend Kitty (https://sw.kovidgoyal.net/kitty/binary)";
  return `${who}. ${recommendation}`;
}
