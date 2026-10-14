// Shared fakes for slack-decisions tests and the e2e driver: a local Slack
// Web-API server, a minimal WebSocket server for Socket Mode, and a fake
// saberante relay on a unix socket.

import crypto from "node:crypto";
import fs from "node:fs";
import http from "node:http";
import net from "node:net";

export class FakeSlack {
  constructor() {
    this.posts = [];
    this.threadReplies = new Map(); // thread ts -> [message]
    this.updates = [];
    this.reactionsByTs = new Map(); // injected human reactions
    this.reactionAddCalls = [];
    this.deletes = [];
    this.calls = [];
    this.socketUrls = 0;
    this.wsUrl = "";
    this.botId = "UBOT";
    this._postSeq = 0;
    this.uploadMeta = new Map(); // file_id -> {filename, length}
    this.uploadBodies = new Map(); // file_id -> Buffer
    this.completedUploads = [];
    this._uploadSeq = 0;
  }

  async start() {
    this.server = http.createServer((req, res) => {
      const chunks = [];
      req.on("data", (c) => chunks.push(c));
      req.on("end", () => {
        const pathname = req.url.split("?")[0];
        if (req.method === "POST" && pathname.startsWith("/upload/")) {
          const fileId = decodeURIComponent(pathname.slice("/upload/".length));
          this.uploadBodies.set(fileId, Buffer.concat(chunks));
          res.statusCode = 200;
          res.end("OK");
          return;
        }
        const body = Buffer.concat(chunks).toString("utf8");
        const method = pathname.replace(/^\//, "");
        let params = {};
        if (req.method === "POST" && body) {
          const contentType = req.headers["content-type"] || "";
          if (contentType.includes("x-www-form-urlencoded")) {
            for (const [k, v] of new URLSearchParams(body)) params[k] = v;
          } else {
            try {
              params = JSON.parse(body);
            } catch {}
          }
        }
        const query = new URL(req.url, "http://x").searchParams;
        for (const [k, v] of query) params[k] = v;
        this.calls.push({ method, params });
        res.setHeader("content-type", "application/json");
        res.end(JSON.stringify(this.handle(method, params)));
      });
    });
    await new Promise((resolve) => this.server.listen(0, "127.0.0.1", resolve));
    this.base = `http://127.0.0.1:${this.server.address().port}`;
    return this;
  }

  handle(method, params) {
    switch (method) {
      case "auth.test":
        return { ok: true, user_id: this.botId };
      case "apps.connections.open": {
        this.socketUrls += 1;
        return { ok: true, url: this.wsUrl };
      }
      case "chat.postMessage": {
        const ts = `1000.${String((this._postSeq += 1)).padStart(4, "0")}`;
        if (params.thread_ts) {
          const list = this.threadReplies.get(params.thread_ts) || [];
          list.push({ ts, user: this.botId, text: params.text, bot_id: "B1" });
          this.threadReplies.set(params.thread_ts, list);
          return { ok: true, ts };
        }
        const post = {
          ts,
          user: this.botId,
          text: params.text,
          blocks: params.blocks,
          channel: params.channel,
          reactions: [],
        };
        this.posts.push(post);
        return { ok: true, ts };
      }
      case "chat.update": {
        const post = this.posts.find((p) => p.ts === params.ts);
        if (post) {
          post.blocks = params.blocks;
          post.text = params.text;
        }
        this.updates.push(params);
        return { ok: true, ts: params.ts };
      }
      case "chat.delete": {
        this.deletes.push(params);
        const index = this.posts.findIndex((p) => p.ts === params.ts);
        if (index !== -1) {
          this.posts.splice(index, 1);
          return { ok: true, ts: params.ts };
        }
        for (const list of this.threadReplies.values()) {
          const rindex = list.findIndex((m) => m.ts === params.ts);
          if (rindex !== -1) {
            list.splice(rindex, 1);
            return { ok: true, ts: params.ts };
          }
        }
        return { ok: false, error: "message_not_found" };
      }
      case "reactions.add": {
        this.reactionAddCalls.push(params);
        const post = this.posts.find((p) => p.ts === params.timestamp);
        if (post) {
          const reaction = post.reactions.find((r) => r.name === params.name);
          if (reaction) reaction.users.push(this.botId);
          else
            post.reactions.push({
              name: params.name,
              users: [this.botId],
              count: 1,
            });
        }
        return { ok: true };
      }
      case "conversations.history": {
        const ts = params.latest;
        const post = this.posts.find((p) => p.ts === ts) || null;
        const extra = this.reactionsByTs.get(ts) || [];
        const merged = post
          ? { ...post, reactions: [...post.reactions, ...extra] }
          : null;
        return { ok: true, messages: merged ? [merged] : [] };
      }
      case "conversations.replies": {
        const ts = params.ts;
        const post = this.posts.find((p) => p.ts === ts);
        const replies = this.threadReplies.get(ts) || [];
        return { ok: true, messages: post ? [post, ...replies] : replies };
      }
      case "files.getUploadURLExternal": {
        const fileId = `FAKE${String((this._uploadSeq += 1)).padStart(4, "0")}`;
        this.uploadMeta.set(fileId, {
          filename: params.filename,
          length: Number(params.length),
        });
        return {
          ok: true,
          upload_url: `${this.base}/upload/${fileId}`,
          file_id: fileId,
        };
      }
      case "files.completeUploadExternal": {
        this.completedUploads.push(params);
        return {
          ok: true,
          files: (params.files || []).map((f) => ({
            id: f.id,
            title: f.title,
            channel: params.channel_id,
          })),
        };
      }
      default:
        return { ok: true };
    }
  }

  // ---- test helpers ---------------------------------------------------------

  userReact(ts, name, user = "UHUMAN") {
    const list = this.reactionsByTs.get(ts) || [];
    list.push({ name, users: [user], count: 1 });
    this.reactionsByTs.set(ts, list);
  }

  userReply(threadTs, text, user = "UHUMAN") {
    const ts = `1000.9${String(Math.random()).slice(2, 8)}`;
    const list = this.threadReplies.get(threadTs) || [];
    list.push({ ts, user, text });
    this.threadReplies.set(threadTs, list);
    return ts;
  }

  async stop() {
    this.server?.close();
  }
}

export class FakeWsServer {
  constructor() {
    this.clients = [];
    this.received = [];
  }

  async start() {
    this.server = http.createServer((_req, res) => {
      res.statusCode = 404;
      res.end();
    });
    this.server.on("upgrade", (req, socket) => {
      const key = req.headers["sec-websocket-key"];
      const accept = crypto
        .createHash("sha1")
        .update(`${key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11`)
        .digest("base64");
      socket.write(
        "HTTP/1.1 101 Switching Protocols\r\n" +
          "Upgrade: websocket\r\n" +
          "Connection: Upgrade\r\n" +
          `Sec-WebSocket-Accept: ${accept}\r\n\r\n`,
      );
      const client = { socket, buffer: Buffer.alloc(0) };
      this.clients.push(client);
      socket.on("data", (chunk) => this._onData(client, chunk));
      socket.on("close", () => {
        this.clients = this.clients.filter((c) => c !== client);
      });
      socket.on("error", () => {});
    });
    await new Promise((resolve) => this.server.listen(0, "127.0.0.1", resolve));
    this.url = `ws://127.0.0.1:${this.server.address().port}`;
    return this;
  }

  _onData(client, chunk) {
    client.buffer = Buffer.concat([client.buffer, chunk]);
    for (;;) {
      const b = client.buffer;
      if (b.length < 2) return;
      let len = b[1] & 0x7f;
      let offset = 2;
      if (len === 126) {
        if (b.length < 4) return;
        len = b.readUInt16BE(2);
        offset = 4;
      } else if (len === 127) {
        if (b.length < 10) return;
        len = Number(b.readBigUInt64BE(2));
        offset = 10;
      }
      const masked = (b[1] & 0x80) !== 0;
      const maskLen = masked ? 4 : 0;
      if (b.length < offset + maskLen + len) return;
      let payload = b.subarray(offset + maskLen, offset + maskLen + len);
      if (masked) {
        const mask = b.subarray(offset, offset + 4);
        const out = Buffer.alloc(payload.length);
        for (let i = 0; i < payload.length; i += 1) {
          out[i] = payload[i] ^ mask[i % 4];
        }
        payload = out;
      }
      client.buffer = b.subarray(offset + maskLen + len);
      const opcode = b[0] & 0x0f;
      if (opcode === 0x1) this.received.push(payload.toString("utf8"));
      if (opcode === 0x8) client.socket.end();
    }
  }

  send(obj) {
    const payload = Buffer.from(JSON.stringify(obj), "utf8");
    const header =
      payload.length < 126
        ? Buffer.from([0x81, payload.length])
        : (() => {
            const h = Buffer.alloc(4);
            h[0] = 0x81;
            h[1] = 126;
            h.writeUInt16BE(payload.length, 2);
            return h;
          })();
    for (const client of this.clients) {
      client.socket.write(Buffer.concat([header, payload]));
    }
  }

  waitForClient() {
    return waitUntil(() => this.clients.length > 0);
  }

  async stop() {
    for (const client of this.clients) client.socket.destroy();
    this.server?.close();
  }
}

// Fake saberante hub: sends {"type":"link"} on connect (and on hello/status),
// records every client frame, and answers notify requests with {"type":"queued"}.
export async function startFakeSabera({ glasses = "connected", sockPath } = {}) {
  try {
    fs.unlinkSync(sockPath);
  } catch {}
  const received = [];
  const server = net.createServer((socket) => {
    let handshake = Buffer.alloc(0);
    let frames = Buffer.alloc(0);
    let upgraded = false;
    const sendObj = (obj) => {
      const payload = Buffer.from(JSON.stringify(obj), "utf8");
      const header =
        payload.length < 126
          ? Buffer.from([0x81, payload.length])
          : (() => {
              const h = Buffer.alloc(4);
              h[0] = 0x81;
              h[1] = 126;
              h.writeUInt16BE(payload.length, 2);
              return h;
            })();
      try {
        socket.write(Buffer.concat([header, payload]));
      } catch {}
    };
    const sendLink = () =>
      sendObj({
        type: "link",
        version: 1,
        viewer_url: null,
        landline_host: null,
        glasses,
      });
    const onFrame = (text) => {
      let message;
      try {
        message = JSON.parse(text);
      } catch {
        return;
      }
      received.push(message);
      if (message?.type === "notify") sendObj({ type: "queued" });
      else if (message?.type === "hello" || message?.type === "status") {
        sendLink();
      }
    };
    const drain = () => {
      for (;;) {
        const b = frames;
        if (b.length < 2) return;
        let len = b[1] & 0x7f;
        let offset = 2;
        if (len === 126) {
          if (b.length < 4) return;
          len = b.readUInt16BE(2);
          offset = 4;
        } else if (len === 127) {
          if (b.length < 10) return;
          len = Number(b.readBigUInt64BE(2));
          offset = 10;
        }
        const masked = (b[1] & 0x80) !== 0;
        const maskLen = masked ? 4 : 0;
        if (b.length < offset + maskLen + len) return;
        let payload = b.subarray(offset + maskLen, offset + maskLen + len);
        if (masked) {
          const mask = b.subarray(offset, offset + 4);
          const out = Buffer.alloc(payload.length);
          for (let i = 0; i < payload.length; i += 1) {
            out[i] = payload[i] ^ mask[i % 4];
          }
          payload = out;
        }
        frames = b.subarray(offset + maskLen + len);
        const opcode = b[0] & 0x0f;
        if (opcode === 0x1) onFrame(payload.toString("utf8"));
        if (opcode === 0x8) socket.end();
      }
    };
    socket.on("data", (chunk) => {
      if (upgraded) {
        frames = Buffer.concat([frames, chunk]);
        drain();
        return;
      }
      handshake = Buffer.concat([handshake, chunk]);
      const marker = handshake.indexOf("\r\n\r\n");
      if (marker === -1) return;
      upgraded = true;
      socket.write(
        "HTTP/1.1 101 Switching Protocols\r\n" +
          "Upgrade: websocket\r\n" +
          "Connection: Upgrade\r\n" +
          "Sec-WebSocket-Accept: dummy\r\n\r\n",
      );
      sendLink();
      const rest = handshake.subarray(marker + 4);
      if (rest.length > 0) {
        frames = rest;
        drain();
      }
    });
    socket.on("error", () => {});
  });
  await new Promise((resolve) => server.listen(sockPath, resolve));
  return {
    sockPath,
    received,
    notifies: () => received.filter((m) => m?.type === "notify"),
    stop() {
      server.close();
      try {
        fs.unlinkSync(sockPath);
      } catch {}
    },
  };
}

export async function waitUntil(fn, { timeout = 5_000, interval = 20 } = {}) {
  const deadline = Date.now() + timeout;
  for (;;) {
    const value = await fn();
    if (value) return value;
    if (Date.now() > deadline) throw new Error("waitUntil timed out");
    await new Promise((r) => setTimeout(r, interval));
  }
}
