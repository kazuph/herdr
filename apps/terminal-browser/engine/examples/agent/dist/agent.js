"use strict";

// examples/agent/src/main.tsx
var import_pixel_react2 = require("pixel-react");

// examples/agent/src/App.tsx
var import_react = require("react");
var import_pixel_react = require("pixel-react");

// examples/agent/src/session.ts
var import_fs = require("fs");
var import_claude_agent_sdk = require("@anthropic-ai/claude-agent-sdk");
var PERMISSION_MODES = [
  "default",
  "acceptEdits",
  "plan",
  "bypassPermissions"
];
var THINKING = [
  { label: "auto", tokens: null },
  { label: "off", tokens: 0 },
  { label: "high", tokens: 32e3 }
];
function detail(input) {
  const keys = ["command", "file_path", "path", "pattern", "url", "query", "description", "prompt"];
  const found = keys.map((k) => input[k]).find((v) => typeof v === "string");
  const text = found ?? JSON.stringify(input) ?? "";
  const flat = text.replace(/\s+/g, " ");
  return flat.length > 64 ? `${flat.slice(0, 61)}\u2026` : flat;
}
var Session = class {
  constructor(notify) {
    this.notify = notify;
    this.q = (0, import_claude_agent_sdk.query)({
      prompt: this.outgoing(),
      options: {
        systemPrompt: { type: "preset", preset: "claude_code" },
        permissionMode: this.mode,
        includePartialMessages: true,
        canUseTool: (tool, input) => new Promise((resolve) => {
          this.ask = {
            tool,
            detail: detail(input),
            resolve: (allow) => {
              this.ask = null;
              this.notify();
              resolve(
                allow ? { behavior: "allow", updatedInput: input } : { behavior: "deny", message: "The user declined this tool use." }
              );
            }
          };
          this.notify();
        })
      }
    });
    void this.run();
    void this.q.supportedModels().then((models) => this.models = models);
  }
  notify;
  items = [];
  working = false;
  activity = "";
  model = "";
  mode = "default";
  thinking = 0;
  cost = 0;
  ask = null;
  q;
  models = [];
  queue = [];
  wake = null;
  tools = /* @__PURE__ */ new Map();
  draftFrom = null;
  title() {
    const first = this.items.find((item) => item.kind === "user");
    if (first?.kind !== "user") return "new session";
    return first.text.length > 22 ? `${first.text.slice(0, 21)}\u2026` : first.text;
  }
  send(text) {
    this.items.push({ kind: "user", text });
    this.working = true;
    this.queue.push({
      type: "user",
      session_id: "",
      parent_tool_use_id: null,
      message: { role: "user", content: text }
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
  async *outgoing() {
    while (true) {
      while (this.queue.length > 0) yield this.queue.shift();
      await new Promise((resolve) => this.wake = resolve);
      this.wake = null;
    }
  }
  async run() {
    try {
      for await (const message of this.q) {
        if (process.env.AGENT_LOG) {
          (0, import_fs.appendFileSync)(process.env.AGENT_LOG, `${JSON.stringify(message)}
`);
        }
        this.handle(message);
      }
    } catch (error) {
      this.items.push({ kind: "assistant", text: `error: ${String(error)}` });
      this.working = false;
      this.notify();
    }
  }
  handle(message) {
    switch (message.type) {
      case "system":
        if (message.subtype === "init") {
          this.model = message.model;
          this.mode = message.permissionMode;
        }
        break;
      case "stream_event": {
        if (message.parent_tool_use_id !== null) break;
        const event = message.event;
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
        if (!inSubagent && this.draftFrom !== null) {
          this.items.splice(this.draftFrom);
          this.draftFrom = null;
        }
        for (const block of message.message.content) {
          if (block.type === "text" && block.text && !inSubagent) {
            this.items.push({ kind: "assistant", text: block.text });
          }
          if (block.type === "tool_use" && block.id && block.name) {
            const call = {
              id: block.id,
              name: block.name,
              detail: detail(block.input ?? {}),
              status: "running",
              kids: []
            };
            this.tools.set(call.id, call);
            const parent = message.parent_tool_use_id ? this.tools.get(message.parent_tool_use_id) : void 0;
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
        for (const block of content) {
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
};
var Store = class {
  sessions = [];
  at = 0;
  sidebar = false;
  version = 0;
  listeners = /* @__PURE__ */ new Set();
  notify = () => {
    this.version += 1;
    for (const listener of this.listeners) listener();
  };
  subscribe = (listener) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };
  snapshot = () => this.version;
  constructor() {
    this.sessions.push(new Session(this.notify));
  }
  active() {
    return this.sessions[this.at];
  }
  add() {
    this.sessions.push(new Session(this.notify));
    this.at = this.sessions.length - 1;
    this.notify();
  }
  select(at) {
    this.at = at;
    this.notify();
  }
  toggleSidebar() {
    this.sidebar = !this.sidebar;
    this.notify();
  }
};
var store = new Store();

// examples/agent/src/theme.ts
function mix(base, toward, t) {
  const channel = (b, w) => Math.round(b + (w - b) * t);
  return [
    channel(base[0], toward[0]),
    channel(base[1], toward[1]),
    channel(base[2], toward[2]),
    255
  ];
}
function makeTheme(colors) {
  const bg = colors.background ?? [22, 22, 30, 255];
  const fg = colors.foreground ?? [222, 220, 235, 255];
  const accent = colors.palette[13] ?? colors.palette[12] ?? [159, 134, 235, 255];
  return {
    bg,
    fg,
    muted: mix(fg, bg, 0.45),
    accent,
    green: colors.palette[2] ?? [140, 200, 140, 255],
    red: colors.palette[1] ?? [220, 120, 120, 255],
    chipBg: mix(bg, fg, 0.09),
    hairline: mix(bg, fg, 0.15),
    selection: mix(bg, accent, 0.35),
    sidebarBg: mix(bg, fg, 0.05),
    itemHover: mix(bg, fg, 0.11),
    itemActive: mix(bg, accent, 0.3)
  };
}

// examples/agent/src/App.tsx
var import_jsx_runtime = require("react/jsx-runtime");
var FONT_MONO = 1;
function App({ info }) {
  (0, import_react.useSyncExternalStore)(store.subscribe, store.snapshot);
  const theme = (0, import_react.useMemo)(() => makeTheme(info.colors), [info]);
  const rem = info.basePx;
  const ctx = { theme, rem };
  const session = store.active();
  const list = (0, import_react.useRef)(null);
  const input = (0, import_react.useRef)(null);
  const follow = (0, import_react.useRef)(true);
  const lastOffset = (0, import_react.useRef)(0);
  (0, import_react.useEffect)(() => {
    follow.current = true;
    list.current?.scrollTo(1e9);
  }, [store.at]);
  (0, import_react.useEffect)(() => {
    if (follow.current) list.current?.scrollTo(1e9, true);
  });
  (0, import_react.useEffect)(() => {
    if (session.ask) input.current?.blur();
    else input.current?.focus();
  }, [session.ask]);
  return /* @__PURE__ */ (0, import_jsx_runtime.jsxs)(
    import_pixel_react.Box,
    {
      style: {
        width: "100%",
        height: "100%",
        background: theme.bg,
        color: theme.fg,
        fontSize: rem
      },
      children: [
        store.sidebar && /* @__PURE__ */ (0, import_jsx_runtime.jsx)(Sidebar, { ctx }),
        /* @__PURE__ */ (0, import_jsx_runtime.jsxs)(import_pixel_react.Box, { style: { flexDirection: "column", flexGrow: 1, flexBasis: 0, overflow: "hidden" }, children: [
          /* @__PURE__ */ (0, import_jsx_runtime.jsx)(Header, { ctx, session }),
          /* @__PURE__ */ (0, import_jsx_runtime.jsxs)(
            import_pixel_react.Box,
            {
              ref: list,
              style: {
                flexDirection: "column",
                flexGrow: 1,
                flexBasis: 0,
                overflow: "scroll",
                padding: rem,
                gap: rem * 0.75
              },
              onScroll: (e) => {
                if (e.offset < lastOffset.current - 1) follow.current = false;
                if (e.offset >= e.max - 2) follow.current = true;
                lastOffset.current = e.offset;
              },
              children: [
                session.items.length === 0 && /* @__PURE__ */ (0, import_jsx_runtime.jsx)(import_pixel_react.Text, { style: { color: theme.muted }, children: "ask claude anything" }),
                session.items.map((item, i) => /* @__PURE__ */ (0, import_jsx_runtime.jsx)(Message, { ctx, item }, i))
              ]
            }
          ),
          session.ask && /* @__PURE__ */ (0, import_jsx_runtime.jsx)(AskBox, { ctx, ask: session.ask }),
          /* @__PURE__ */ (0, import_jsx_runtime.jsx)(Composer, { ctx, inputRef: input })
        ] })
      ]
    }
  );
}
function Sidebar({ ctx }) {
  const { theme, rem } = ctx;
  return /* @__PURE__ */ (0, import_jsx_runtime.jsxs)(
    import_pixel_react.Box,
    {
      style: {
        flexDirection: "column",
        width: rem * 13,
        flexShrink: 0,
        margin: rem * 0.4,
        padding: rem * 0.4,
        gap: rem * 0.125,
        background: theme.sidebarBg,
        cornerRadius: rem * 0.6
      },
      children: [
        /* @__PURE__ */ (0, import_jsx_runtime.jsx)(
          import_pixel_react.Text,
          {
            style: {
              padding: { left: rem * 0.6, right: rem * 0.6, top: rem * 0.35, bottom: rem * 0.35 },
              color: theme.muted,
              fontSize: rem * 0.85
            },
            children: "sessions"
          }
        ),
        store.sessions.map((session, i) => /* @__PURE__ */ (0, import_jsx_runtime.jsx)(SidebarItem, { ctx, session, at: i }, i)),
        /* @__PURE__ */ (0, import_jsx_runtime.jsx)(import_pixel_react.Box, { style: { flexGrow: 1 } }),
        /* @__PURE__ */ (0, import_jsx_runtime.jsx)(
          import_pixel_react.Text,
          {
            style: {
              padding: { left: rem * 0.6, right: rem * 0.6, top: rem * 0.35, bottom: rem * 0.35 },
              cornerRadius: rem * 0.4,
              border: { width: Math.max(rem / 16, 1), color: theme.hairline },
              color: theme.accent,
              hoverBackground: theme.itemHover
            },
            onClick: () => store.add(),
            children: "+ new session"
          }
        )
      ]
    }
  );
}
function SidebarItem({ ctx, session, at }) {
  const { theme, rem } = ctx;
  const active = at === store.at;
  return /* @__PURE__ */ (0, import_jsx_runtime.jsxs)(
    import_pixel_react.Box,
    {
      style: {
        alignItems: "center",
        gap: rem * 0.5,
        padding: { left: rem * 0.6, right: rem * 0.6, top: rem * 0.35, bottom: rem * 0.35 },
        cornerRadius: rem * 0.4,
        background: active ? theme.itemActive : void 0,
        hoverBackground: active ? void 0 : theme.itemHover,
        overflow: "hidden"
      },
      onClick: () => store.select(at),
      children: [
        /* @__PURE__ */ (0, import_jsx_runtime.jsx)(
          import_pixel_react.Text,
          {
            style: {
              color: active ? theme.fg : theme.muted,
              flexGrow: 1,
              flexBasis: 0,
              wrap: false
            },
            children: session.title()
          }
        ),
        session.working && /* @__PURE__ */ (0, import_jsx_runtime.jsx)(Dot, { ctx, color: theme.accent })
      ]
    }
  );
}
function Header({ ctx, session }) {
  const { theme, rem } = ctx;
  const [frame, setFrame] = (0, import_react.useState)(0);
  (0, import_react.useEffect)(() => {
    if (!session.working) return;
    const timer = setInterval(() => setFrame((f) => f + 1), 250);
    return () => clearInterval(timer);
  }, [session.working]);
  const status = session.working ? `${session.activity || "working"}${".".repeat(1 + frame % 3)}` : session.cost > 0 ? `$${session.cost.toFixed(4)}` : "idle";
  return /* @__PURE__ */ (0, import_jsx_runtime.jsxs)(
    import_pixel_react.Box,
    {
      style: {
        alignItems: "center",
        gap: rem * 0.5,
        padding: { left: rem, right: rem, top: rem * 0.5, bottom: rem * 0.5 }
      },
      children: [
        /* @__PURE__ */ (0, import_jsx_runtime.jsx)(Chip, { ctx, color: theme.fg, children: session.model.replace(/^claude-/, "") || "\u2026" }),
        /* @__PURE__ */ (0, import_jsx_runtime.jsx)(Chip, { ctx, color: session.mode === "bypassPermissions" ? theme.red : theme.muted, children: session.mode }),
        /* @__PURE__ */ (0, import_jsx_runtime.jsx)(Chip, { ctx, color: theme.muted, children: `thinking ${THINKING[session.thinking].label}` }),
        /* @__PURE__ */ (0, import_jsx_runtime.jsx)(import_pixel_react.Box, { style: { flexGrow: 1 } }),
        /* @__PURE__ */ (0, import_jsx_runtime.jsx)(
          import_pixel_react.Text,
          {
            style: {
              color: session.working ? theme.accent : theme.muted,
              fontSize: rem * 0.85,
              font: FONT_MONO,
              wrap: false,
              flexShrink: 0
            },
            children: status
          }
        )
      ]
    }
  );
}
function Message({ ctx, item }) {
  const { theme, rem } = ctx;
  if (item.kind === "user") {
    return /* @__PURE__ */ (0, import_jsx_runtime.jsxs)(import_pixel_react.Box, { style: { gap: rem * 0.5 }, children: [
      /* @__PURE__ */ (0, import_jsx_runtime.jsx)(import_pixel_react.Text, { style: { color: theme.accent, flexShrink: 0 }, children: ">" }),
      /* @__PURE__ */ (0, import_jsx_runtime.jsx)(import_pixel_react.Text, { style: { color: theme.muted }, children: item.text })
    ] });
  }
  if (item.kind === "tool") {
    return /* @__PURE__ */ (0, import_jsx_runtime.jsx)(ToolRow, { ctx, call: item.call });
  }
  return /* @__PURE__ */ (0, import_jsx_runtime.jsx)(import_pixel_react.Text, { children: item.text });
}
function ToolRow({ ctx, call }) {
  const { theme, rem } = ctx;
  const color = call.status === "running" ? theme.accent : call.status === "ok" ? theme.green : theme.red;
  return /* @__PURE__ */ (0, import_jsx_runtime.jsxs)(import_pixel_react.Box, { style: { flexDirection: "column", gap: rem * 0.25 }, children: [
    /* @__PURE__ */ (0, import_jsx_runtime.jsxs)(import_pixel_react.Box, { style: { gap: rem * 0.5, alignItems: "center", overflow: "hidden" }, children: [
      /* @__PURE__ */ (0, import_jsx_runtime.jsx)(Dot, { ctx, color }),
      /* @__PURE__ */ (0, import_jsx_runtime.jsx)(import_pixel_react.Text, { style: { font: FONT_MONO, fontSize: rem * 0.9, flexShrink: 0, wrap: false }, children: call.name }),
      /* @__PURE__ */ (0, import_jsx_runtime.jsx)(import_pixel_react.Text, { style: { color: theme.muted, font: FONT_MONO, fontSize: rem * 0.9, wrap: false }, children: call.detail })
    ] }),
    call.kids.length > 0 && /* @__PURE__ */ (0, import_jsx_runtime.jsx)(import_pixel_react.Box, { style: { flexDirection: "column", gap: rem * 0.25, margin: { left: rem } }, children: call.kids.map((kid) => /* @__PURE__ */ (0, import_jsx_runtime.jsx)(ToolRow, { ctx, call: kid }, kid.id)) })
  ] });
}
function AskBox({ ctx: { theme, rem }, ask }) {
  return /* @__PURE__ */ (0, import_jsx_runtime.jsxs)(
    import_pixel_react.Box,
    {
      style: {
        flexDirection: "column",
        gap: rem * 0.25,
        margin: { left: rem, right: rem, bottom: rem * 0.5 },
        padding: rem * 0.6,
        border: { width: Math.max(rem / 16, 1), color: theme.accent },
        cornerRadius: rem * 0.4
      },
      children: [
        /* @__PURE__ */ (0, import_jsx_runtime.jsxs)(import_pixel_react.Box, { style: { gap: rem * 0.5, overflow: "hidden" }, children: [
          /* @__PURE__ */ (0, import_jsx_runtime.jsx)(import_pixel_react.Text, { style: { color: theme.accent, font: FONT_MONO, flexShrink: 0 }, children: ask.tool }),
          /* @__PURE__ */ (0, import_jsx_runtime.jsx)(import_pixel_react.Text, { style: { color: theme.muted, font: FONT_MONO, wrap: false }, children: ask.detail })
        ] }),
        /* @__PURE__ */ (0, import_jsx_runtime.jsx)(import_pixel_react.Text, { style: { color: theme.muted, fontSize: rem * 0.85 }, children: "enter allow \xB7 esc deny" })
      ]
    }
  );
}
function Composer({ ctx: { theme, rem }, inputRef }) {
  return /* @__PURE__ */ (0, import_jsx_runtime.jsxs)(import_pixel_react.Box, { style: { flexDirection: "column", flexShrink: 0 }, children: [
    /* @__PURE__ */ (0, import_jsx_runtime.jsx)(import_pixel_react.Box, { style: { height: Math.max(rem / 16, 1), width: "100%", background: theme.hairline } }),
    /* @__PURE__ */ (0, import_jsx_runtime.jsxs)(import_pixel_react.Box, { style: { alignItems: "start", gap: rem * 0.5, padding: rem * 0.75 }, children: [
      /* @__PURE__ */ (0, import_jsx_runtime.jsx)(import_pixel_react.Text, { style: { color: theme.accent, flexShrink: 0 }, children: ">" }),
      /* @__PURE__ */ (0, import_jsx_runtime.jsx)(
        import_pixel_react.Input,
        {
          ref: inputRef,
          style: { flexGrow: 1, flexBasis: 0 },
          caretColor: theme.accent,
          selectionColor: theme.selection,
          autoFocus: true,
          onSubmit: (text) => {
            const trimmed = text.trim();
            if (trimmed) store.active().send(trimmed);
          }
        }
      )
    ] }),
    /* @__PURE__ */ (0, import_jsx_runtime.jsx)(
      import_pixel_react.Text,
      {
        style: {
          padding: { left: rem * 0.75, right: rem * 0.75, bottom: rem * 0.5 },
          color: theme.muted,
          fontSize: rem * 0.8
        },
        children: "enter send \xB7 shift+enter newline \xB7 cmd+b sessions \xB7 ^o model \xB7 ^p permissions \xB7 ^t thinking \xB7 esc interrupt \xB7 ^q quit"
      }
    )
  ] });
}
function Dot({ ctx: { rem }, color }) {
  return /* @__PURE__ */ (0, import_jsx_runtime.jsx)(
    import_pixel_react.Box,
    {
      style: {
        width: rem * 0.45,
        height: rem * 0.45,
        cornerRadius: 999,
        background: color,
        flexShrink: 0
      }
    }
  );
}
function Chip({ ctx: { theme, rem }, color, children }) {
  return /* @__PURE__ */ (0, import_jsx_runtime.jsx)(
    import_pixel_react.Text,
    {
      style: {
        padding: { left: rem * 0.6, right: rem * 0.6, top: rem * 0.15, bottom: rem * 0.15 },
        cornerRadius: 999,
        background: theme.chipBg,
        color,
        fontSize: rem * 0.85,
        font: FONT_MONO,
        flexShrink: 0,
        wrap: false
      },
      children
    }
  );
}

// examples/agent/src/main.tsx
var import_jsx_runtime2 = require("react/jsx-runtime");
var root = (0, import_pixel_react2.createRoot)({
  onKey(event) {
    if (event.mods.ctrl && event.key === "q") {
      root.stop();
      process.exit(0);
    }
    if (event.mods.super && event.key === "b") {
      store.toggleSidebar();
      return;
    }
    const session = store.active();
    if (session.ask) {
      if (event.key === "enter" || event.key === "y") session.ask.resolve(true);
      if (event.key === "escape" || event.key === "n") session.ask.resolve(false);
      return;
    }
    if (event.key === "escape") session.interrupt();
    if (event.mods.ctrl && event.key === "o") session.cycleModel();
    if (event.mods.ctrl && event.key === "p") session.cycleMode();
    if (event.mods.ctrl && event.key === "t") session.cycleThinking();
  }
});
root.render(/* @__PURE__ */ (0, import_jsx_runtime2.jsx)(App, { info: root.info }));
