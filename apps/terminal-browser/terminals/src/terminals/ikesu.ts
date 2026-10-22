import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { multiplexerBin, verifyPane } from "../multiplexer";
import { shellQuote } from "../shared";
import type { Detect } from "../terminal";
import { enableKittyGraphics } from "./herdr";

export const ikesu: Detect = (env, run) => {
  if (!env.IKESU_PANE_ID) return null;
  const command = (args: string[]) => run(multiplexerBin("ikesu", env), args);
  const verify = () => verifyPane("ikesu", env, run);
  return {
    name: "ikesu",
    async prepare() {
      await verify();
      const configPath = env.IKESU_CONFIG_PATH || path.join(env.XDG_CONFIG_HOME || path.join(env.HOME ?? os.homedir(), ".config"), "ikesu", "config.toml");
      const existed = fs.existsSync(configPath);
      const current = existed ? fs.readFileSync(configPath, "utf8") : "";
      if (/^[ \t]*kitty_graphics[ \t]*=[ \t]*true[ \t]*$/m.test(current)) return;
      const preflight = JSON.parse(await command(["server", "reload-config"]));
      if (preflight.error || preflight.result?.status !== "applied") throw new Error("ikesu reload-config unavailable or rejected; configuration was not changed.");
      fs.mkdirSync(path.dirname(configPath), { recursive: true });
      fs.writeFileSync(configPath, enableKittyGraphics(current));
      try {
        const reply = JSON.parse(await command(["server", "reload-config"]));
        if (reply.error || reply.result?.status !== "applied") throw new Error("ikesu rejected reload-config");
      } catch (error) {
        if (existed) fs.writeFileSync(configPath, current);
        else fs.unlinkSync(configPath);
        throw error;
      }
    },
    async getCurrentPane() {
      return verify();
    },
    async split({ from, direction, command: argv, size }) {
      await verify();
      const native = direction === "up" || direction === "down" ? "down" : "right";
      const ratio = size !== null ? ["--ratio", String(size)] : [];
      const reply = JSON.parse(await command(["pane", "split", "--pane", from.id, "--direction", native, "--focus", ...ratio]));
      const id = reply.result?.pane?.pane_id;
      if (reply.error || typeof id !== "string") throw new Error("ikesu pane split did not return a pane");
      if (direction === "left" || direction === "up") {
        await command(["pane", "swap", "--pane", id, "--direction", native === "right" ? "left" : "up"]);
      }
      await command(["pane", "run", id, shellQuote(argv)]);
    },
  };
};
