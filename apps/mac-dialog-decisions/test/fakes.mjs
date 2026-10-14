// Shared fakes for mac-dialog-decisions tests. No real dialogs are opened.

import { EventEmitter } from "node:events";

// A controllable stand-in for the osascript dialog child. `lines` is an async
// iterable fed by emit()/finish(); kill() marks it terminated like a real
// SIGTERM would (no further lines, stream ends).
export class FakeDialog {
  constructor() {
    this.emitter = new EventEmitter();
    this.killed = false;
    this.specPath = "";
    this.spec = null;
    this._queue = [];
    this._waiters = [];
    this._done = false;
  }

  emit(line) {
    this._push(typeof line === "string" ? line : JSON.stringify(line));
  }

  shown() {
    this.emit({ event: "shown" });
  }

  answer(optionIds = [], text = "") {
    this.emit({ result: "answer", option_ids: optionIds, text });
    this.finish();
  }

  dismiss() {
    this.emit({ result: "dismiss" });
    this.finish();
  }

  // process exited without emitting a result (user closed / killed)
  finish() {
    if (this._done) return;
    this._done = true;
    for (const waiter of this._waiters) waiter();
    this._waiters = [];
  }

  kill() {
    this.killed = true;
    this.finish();
  }

  _push(line) {
    if (this._done) return;
    const waiter = this._waiters.shift();
    if (waiter) waiter(line);
    else this._queue.push(line);
  }

  get lines() {
    const self = this;
    return (async function* () {
      for (;;) {
        if (self._queue.length > 0) {
          yield self._queue.shift();
          continue;
        }
        if (self._done) return;
        const line = await new Promise((resolve) =>
          self._waiters.push(resolve),
        );
        if (line === undefined) return;
        yield line;
      }
    })();
  }
}

// A controllable stand-in for `herdr decision watch --all-machines`.
export class FakeWatch {
  constructor() {
    this.killed = false;
    this._queue = [];
    this._waiters = [];
    this._done = false;
  }

  push(obj) {
    const line = typeof obj === "string" ? obj : JSON.stringify(obj);
    const waiter = this._waiters.shift();
    if (waiter) waiter(line);
    else this._queue.push(line);
  }

  end() {
    if (this._done) return;
    this._done = true;
    for (const w of this._waiters) w();
    this._waiters = [];
  }

  close() {
    this.killed = true;
    this.end();
  }

  get lines() {
    const self = this;
    return (async function* () {
      for (;;) {
        if (self._queue.length > 0) {
          yield JSON.parse(self._queue.shift());
          continue;
        }
        if (self._done) return;
        const line = await new Promise((resolve) =>
          self._waiters.push(resolve),
        );
        if (line === undefined) return;
        yield JSON.parse(line);
      }
    })();
  }
}

export async function waitUntil(fn, { timeout = 5_000, interval = 20 } = {}) {
  const deadline = Date.now() + timeout;
  for (;;) {
    let value;
    try {
      value = await fn();
    } catch {
      value = undefined;
    }
    if (value) return value;
    if (Date.now() > deadline) {
      throw new Error(`waitUntil timed out after ${timeout}ms`);
    }
    await new Promise((resolve) => setTimeout(resolve, interval));
  }
}
