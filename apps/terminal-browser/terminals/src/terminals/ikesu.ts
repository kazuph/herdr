import fs from "node:fs";
import { randomUUID } from "node:crypto";
import path from "node:path";
import { multiplexerBin, verifyPane } from "../multiplexer";
import { shellQuote } from "../shared";
import type { Detect } from "../terminal";
import { enableKittyGraphics } from "./herdr";

function realConfigPath(file: string): string {
  try { return fs.realpathSync(file); }
  catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
    const parent = path.dirname(file);
    if (parent === file) throw error;
    return path.join(realConfigPath(parent), path.basename(file));
  }
}

function configContents(file: string): string | null {
  try { return fs.readFileSync(file, "utf8"); }
  catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
    return null;
  }
}

function replaceConfig(file: string, content: string, expected: string | null): void {
  const temporary = path.join(path.dirname(file), `.${path.basename(file)}.tb-${randomUUID()}`);
  const mode = expected === null ? 0o600 : fs.statSync(file).mode & 0o777;
  let descriptor: number | undefined;
  try {
    descriptor = fs.openSync(temporary, "wx", mode);
    fs.writeFileSync(descriptor, content);
    fs.fsyncSync(descriptor);
    fs.closeSync(descriptor);
    descriptor = undefined;
    if (configContents(file) !== expected) throw new Error("ikesu config changed concurrently; keeping the other edit.");
    fs.renameSync(temporary, file);
  } finally {
    if (descriptor !== undefined) fs.closeSync(descriptor);
    try { fs.unlinkSync(temporary); }
    catch (error) { if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error; }
  }
}

export const ikesu: Detect = (env, run) => {
  if (!env.IKESU_PANE_ID) return null;
  const command = (args: string[]) => run(multiplexerBin("ikesu", env), args);
  const verify = () => verifyPane("ikesu", env, run);
  return {
    name: "ikesu",
    async prepare() {
      const requested = env.IKESU_CONFIG_PATH;
      if (!requested || !path.isAbsolute(requested)) throw new Error("ikesu 0.4.1 or newer is required: an absolute IKESU_CONFIG_PATH must identify the server configuration.");
      await verify();
      const preflight = JSON.parse(await command(["server", "reload-config"]));
      if (preflight.error || preflight.result?.status !== "applied") throw new Error("ikesu reload-config unavailable or rejected; configuration was not changed.");
      const reported = preflight.result.config_path;
      if (typeof reported !== "string" || !path.isAbsolute(reported)) throw new Error("ikesu 0.4.1 or newer is required: reload-config must report an absolute config_path; configuration was not changed.");
      const configPath = realConfigPath(requested);
      if (configPath !== realConfigPath(reported)) throw new Error("ikesu config path does not match the server config_path; configuration was not changed.");
      const current = configContents(configPath);
      const updated = enableKittyGraphics(current ?? "");
      if (updated === current) return;
      fs.mkdirSync(path.dirname(configPath), { recursive: true });
      let installed = false;
      try {
        replaceConfig(configPath, updated, current);
        installed = true;
        const reply = JSON.parse(await command(["server", "reload-config"]));
        if (reply.error || reply.result?.status !== "applied") throw new Error("ikesu rejected reload-config");
      } catch (error) {
        if (installed) {
          if (configContents(configPath) !== updated) throw new Error("ikesu reload failed and config changed concurrently; keeping the other edit.", { cause: error });
          if (current === null) fs.unlinkSync(configPath);
          else replaceConfig(configPath, current, updated);
        }
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
