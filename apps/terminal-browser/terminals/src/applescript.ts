import type { Run } from "./run";

export function appleScript(app: string, run: Run) {
  return async function osascript(script: string, args: string[]): Promise<string> {
    try {
      return (await run("osascript", ["-", ...args], script)).trim();
    } catch (error) {
      if ((error as { signal?: string }).signal === "SIGTERM") {
        throw new Error(
          `controlling ${app} timed out — this environment may lack macOS automation permission for ${app}`,
        );
      }
      const stderr = String((error as { stderr?: unknown }).stderr ?? "").trim();
      if (stderr) throw new Error(`controlling ${app} failed: ${stderr}`);
      throw error;
    }
  };
}
