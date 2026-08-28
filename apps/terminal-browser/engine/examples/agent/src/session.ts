import { appendFileSync } from "fs";

import { query } from "@anthropic-ai/claude-agent-sdk";
import type {
  ModelInfo,
  PermissionMode,
  Query,
  SDKMessage,
  SDKUserMessage,
} from "@anthropic-ai/claude-agent-sdk";

export interface ToolCall {
  id: string;
  name: string;
  detail: string;
  status: "running" | "ok" | "error";
  kids: ToolCall[];
}

export type Item =
  | { kind: "user"; text: string }
  | { kind: "assistant"; text: string }
  | { kind: "tool"; call: ToolCall };

export interface Ask {
  tool: string;
  detail: string;
  resolve: (allow: boolean) => void;
}

export const PERMISSION_MODES: PermissionMode[] = [
  "default",
  "acceptEdits",
  "plan",
  "bypassPermissions",
];

export const THINKING = [
  { label: "auto", tokens: null },
  { label: "off", tokens: 0 },
  { label: "high", tokens: 32_000 },
];

interface Block {
  type: string;
  text?: string;
  id?: string;
  name?: string;
  input?: Record<string, unknown>;
  tool_use_id?: string;
  is_error?: boolean;
}

function detail(input: Record<string, unknown>): string {
  const keys = ["command", "file_path", "path", "pattern", "url", "query", "description", "prompt"];
  const found = keys.map((k) => input[k]).find((v) => typeof v === "string");
  const text = (found as string) ?? JSON.stringify(input) ?? "";
  const flat = text.replace(/\s+/g, " ");
  return flat.length > 64 ? `${flat.slice(0, 61)}…` : flat;
}

export class Session {
  items: Item[] = [];
  working = false;
  activity = "";
  model = "";
  mode: PermissionMode = "default";
  thinking = 0;
  cost = 0;
  ask: Ask | null = null;

  private q: Query;
  private models: ModelInfo[] = [];
  private queue: SDKUserMessage[] = [];
  private wake: (() => void) | null = null;
  private tools = new Map<string, ToolCall>();
  private draftFrom: number | null = null;

  constructor(private notify: () => void) {
    this.q = query({
      prompt: this.outgoing(),
      options: {
        systemPrompt: { type: "preset", preset: "claude_code" },
        permissionMode: this.mode,
        includePartialMessages: true,
        canUseTool: (tool, input) =>
          new Promise((resolve) => {
            this.ask = {
              tool,
              detail: detail(input),
              resolve: (allow) => {
                this.ask = null;
                this.notify();
                resolve(
                  allow
                    ? { behavior: "allow", updatedInput: input }
                    : { behavior: "deny", message: "The user declined this tool use." }
                );
              },
            };
            this.notify();
          }),
      },
    });
    void this.run();
    void this.q.supportedModels().then((models) => (this.models = models));
  }

  title(): string {
    const first = this.items.find((item) => item.kind === "user");
    if (first?.kind !== "user") return "new session";
    return first.text.length > 22 ? `${first.text.slice(0, 21)}…` : first.text;
  }

  send(text: string) {
    this.items.push({ kind: "user", text });
    this.working = true;
    this.queue.push({
      type: "user",
      session_id: "",
      parent_tool_use_id: null,
      message: { role: "user", content: text },
    });
    this.wake?.();
    this.notify();
  }

  interrupt() {
    if (this.ask) {
      this.ask.resolve(false);
      return;
    }
    if (!this.working) return;
    void this.q.interrupt().then(() => {
      this.working = false;
      this.activity = "";
      this.notify();
    });
  }

  cycleModel() {
    if (this.models.length === 0) return;
    const at = this.models.findIndex((m) => m.value === this.model);
    const next = this.models[(at + 1) % this.models.length];
    this.model = next.value;
    void this.q.setModel(next.value);
    this.notify();
  }

  cycleMode() {
    const at = PERMISSION_MODES.indexOf(this.mode);
    this.mode = PERMISSION_MODES[(at + 1) % PERMISSION_MODES.length];
    void this.q.setPermissionMode(this.mode);
    this.notify();
  }

  cycleThinking() {
    this.thinking = (this.thinking + 1) % THINKING.length;
    void this.q.setMaxThinkingTokens(THINKING[this.thinking].tokens);
    this.notify();
  }

  private async *outgoing(): AsyncGenerator<SDKUserMessage> {
    while (true) {
      while (this.queue.length > 0) yield this.queue.shift()!;
      await new Promise<void>((resolve) => (this.wake = resolve));
      this.wake = null;
    }
  }

  private async run() {
    try {
      for await (const message of this.q) {
        if (process.env.AGENT_LOG) {
          appendFileSync(process.env.AGENT_LOG, `${JSON.stringify(message)}\n`);
        }
        this.handle(message);
      }
    } catch (error) {
      this.items.push({ kind: "assistant", text: `error: ${String(error)}` });
      this.working = false;
      this.notify();
    }
  }

  private handle(message: SDKMessage) {
    switch (message.type) {
      case "system":
        if (message.subtype === "init") {
          this.model = message.model;
          this.mode = message.permissionMode;
        }
        break;
      case "stream_event": {
        if (message.parent_tool_use_id !== null) break;
        const event = message.event as {
          type: string;
          content_block?: Block;
          delta?: { type: string; text?: string };
        };
        if (event.type === "content_block_start") {
          if (event.content_block?.type === "thinking") this.activity = "thinking";
          if (event.content_block?.type === "text") {
            this.activity = "";
            this.draftFrom ??= this.items.length;
            this.items.push({ kind: "assistant", text: "" });
          }
        }
        if (event.type === "content_block_delta" && event.delta?.type === "text_delta") {
          const last = this.items[this.items.length - 1];
          if (last?.kind === "assistant") last.text += event.delta.text ?? "";
        }
        break;
      }
      case "assistant": {
        const inSubagent = message.parent_tool_use_id !== null;
        // Replace streamed drafts with the authoritative message.
        if (!inSubagent && this.draftFrom !== null) {
          this.items.splice(this.draftFrom);
          this.draftFrom = null;
        }
        for (const block of message.message.content as Block[]) {
          if (block.type === "text" && block.text && !inSubagent) {
            this.items.push({ kind: "assistant", text: block.text });
          }
          if (block.type === "tool_use" && block.id && block.name) {
            const call: ToolCall = {
              id: block.id,
              name: block.name,
              detail: detail(block.input ?? {}),
              status: "running",
              kids: [],
            };
            this.tools.set(call.id, call);
            const parent = message.parent_tool_use_id
              ? this.tools.get(message.parent_tool_use_id)
              : undefined;
            if (parent) parent.kids.push(call);
            else this.items.push({ kind: "tool", call });
            this.activity = block.name;
          }
        }
        break;
      }
      case "user": {
        const content = message.message.content;
        if (!Array.isArray(content)) break;
        for (const block of content as Block[]) {
          if (block.type !== "tool_result" || !block.tool_use_id) continue;
          const call = this.tools.get(block.tool_use_id);
          if (call) call.status = block.is_error ? "error" : "ok";
          if (message.parent_tool_use_id === null) this.activity = "";
        }
        break;
      }
      case "result":
        this.working = false;
        this.activity = "";
        this.cost = message.total_cost_usd;
        if (message.subtype !== "success") {
          this.items.push({ kind: "assistant", text: `error: ${message.subtype}` });
        }
        break;
    }
    this.notify();
  }
}

class Store {
  sessions: Session[] = [];
  at = 0;
  sidebar = false;

  private version = 0;
  private listeners = new Set<() => void>();

  notify = () => {
    this.version += 1;
    for (const listener of this.listeners) listener();
  };

  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  snapshot = () => this.version;

  constructor() {
    this.sessions.push(new Session(this.notify));
  }

  active(): Session {
    return this.sessions[this.at];
  }

  add() {
    this.sessions.push(new Session(this.notify));
    this.at = this.sessions.length - 1;
    this.notify();
  }

  select(at: number) {
    this.at = at;
    this.notify();
  }

  toggleSidebar() {
    this.sidebar = !this.sidebar;
    this.notify();
  }
}

export const store = new Store();
