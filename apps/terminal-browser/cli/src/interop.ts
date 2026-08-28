import { callerTty } from "pixel-terminals";
import type { Terminal } from "pixel-terminals";
import { INTEROP_PROTOCOL_VERSIONS, listInteropInstances } from "pixel-store";
import type { InteropInstance, OpenSpec } from "pixel-store";

import { control } from "./control";

export async function findHosts(terminal: Terminal | null): Promise<InteropInstance[]> {
  const records = listInteropInstances().filter((record) =>
    record.protocolVersions.some((version) => INTEROP_PROTOCOL_VERSIONS.includes(version)),
  );
  const target = process.env.TERMINAL_BROWSER_INTEROP_TARGET;
  if (target) return records.filter((record) => record.socket === target);
  // pane discovery writes to the caller's tty and can be slow, so never run
  // it with nothing to match against
  if (records.length === 0 || !terminal) return [];
  const current = await terminal
    .getCurrentPane?.({ tty: callerTty().path, cwd: process.cwd() })
    .catch(() => null);
  if (!current) return [];
  const answers = await Promise.all(
    records.map(async (record) => {
      const where = (await control(record.socket, { cmd: "where" }, 2000).catch(() => null)) as {
        terminal: string | null;
        tab: string | null;
      } | null;
      if (!where || where.terminal !== terminal.name) return null;
      if (!where.tab || where.tab !== current.tab) return null;
      return record;
    }),
  );
  return answers
    .filter((record): record is InteropInstance => record !== null)
    .sort((a, b) =>
      a.mode === b.mode ? b.startedAt - a.startedAt : a.mode === "browser" ? -1 : 1,
    );
}

export function openInHost(socket: string, spec: OpenSpec): Promise<{ tab: number }> {
  return control(socket, { cmd: "interop/1/open", ...spec }) as Promise<{ tab: number }>;
}
