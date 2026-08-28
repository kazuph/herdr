import fs from "node:fs";
import fsp from "node:fs/promises";
import path from "node:path";
import { captureFilmstrip } from "pixel-react";
import type { SurfaceCapture } from "pixel-react";
import type { BrowserController } from "../page/controller";

export interface FrameMeta {
  tMs: number;
  width: number;
  height: number;
}

export interface PointerSample {
  tMs: number;
  x: number;
  y: number;
}

export interface DecodedFrame {
  bgra: Buffer;
  width: number;
  height: number;
}

export const MAX_RECORDING_MS = 10 * 60 * 1000;

export function lastIndexAtOrBefore<T>(
  items: readonly T[],
  tMs: number,
  timeOf: (item: T) => number,
): number {
  let low = 0;
  let high = items.length - 1;
  let at = -1;
  while (low <= high) {
    const mid = (low + high) >> 1;
    if (timeOf(items[mid]) <= tMs) {
      at = mid;
      low = mid + 1;
    } else {
      high = mid - 1;
    }
  }
  return at;
}
export class Recorder {
  readonly frames: FrameMeta[] = [];
  readonly drops: { tMs: number; count: number }[] = [];
  readonly pointerTrail: PointerSample[] = [];
  readonly clicks: PointerSample[] = [];
  readonly links: { tMs: number; url: string }[] = [];
  readonly reloads: { tMs: number }[] = [];
  readonly loads: { tMs: number }[] = [];
  onCap: (() => void) | null = null;
  captureError: string | null = null;

  private readonly controller: BrowserController;
  private readonly framesDir: string;
  private capture: SurfaceCapture | null = null;
  private liveFrames: number[] = [];
  private stoppedTimes: number[] = [];
  private capTimer: NodeJS.Timeout | null = null;
  private wallStart = 0;
  private stopDuration = 0;
  private stoppedFlag = false;

  constructor(
    controller: BrowserController,
    readonly dir: string,
  ) {
    this.controller = controller;
    this.framesDir = path.join(dir, "frames");
  }

  get stopped(): boolean {
    return this.stoppedFlag;
  }

  get captureDir(): string {
    return this.framesDir;
  }

  async start(): Promise<void> {
    fs.mkdirSync(this.framesDir, { recursive: true });
    await this.controller.attachCdp();
    this.controller.onCdpEvent("Page.loadEventFired", () => this.addLoad());
    this.controller.pinFrameRate(true);
    this.controller.onFrameSubmitted = () => {
      if (!this.stoppedFlag && this.wallStart) {
        this.liveFrames.push(Date.now() - this.wallStart);
      }
    };
    this.capture = this.controller.surface.startCapture(this.framesDir);
    this.wallStart = Date.now();
    this.controller.invalidate();
    void this.nudgeRepaint();
    this.capTimer = setTimeout(() => {
      this.stop();
      this.onCap?.();
    }, MAX_RECORDING_MS);
  }

  private async nudgeRepaint(): Promise<void> {
    try {
      await this.controller.cdp("Emulation.setDefaultBackgroundColorOverride", {
        color: { r: 0, g: 0, b: 0, a: 0 },
      });
      await this.controller.cdp("Emulation.setDefaultBackgroundColorOverride", {});
    } catch {}
  }

  stop(): void {
    if (this.stoppedFlag) return;
    this.stoppedFlag = true;
    if (this.capTimer) {
      clearTimeout(this.capTimer);
      this.capTimer = null;
    }
    this.controller.onFrameSubmitted = null;
    try {
      this.controller.onCdpEvent("Page.loadEventFired", null);
      this.controller.pinFrameRate(false);
    } catch {}
    this.finishCapture();
  }

  durationMs(): number {
    if (this.stoppedFlag) return this.stopDuration;
    return this.wallStart ? Date.now() - this.wallStart : 0;
  }

  private stamp(): number | null {
    if (this.stoppedFlag || !this.wallStart) return null;
    return Date.now() - this.wallStart;
  }

  samplePointer(viewX: number, viewY: number, viewWidth: number, click: boolean) {
    const size = this.controller.frameSize();
    if (!size || viewWidth <= 0) return;
    const scale = size.width / viewWidth;
    if (click) this.addClick(viewX * scale, viewY * scale);
    else this.addPointer(viewX * scale, viewY * scale);
  }

  private addPointer(x: number, y: number) {
    const tMs = this.stamp();
    if (tMs == null) return;
    const last = this.pointerTrail[this.pointerTrail.length - 1];
    if (last && tMs - last.tMs < 16 && Math.hypot(x - last.x, y - last.y) < 2) return;
    this.pointerTrail.push({ tMs, x, y });
  }

  private addClick(x: number, y: number) {
    const tMs = this.stamp();
    if (tMs == null) return;
    this.pointerTrail.push({ tMs, x, y });
    this.clicks.push({ tMs, x, y });
  }

  addLink(url: string) {
    const tMs = this.stamp();
    if (tMs != null) this.links.push({ tMs, url });
  }

  addReload() {
    const tMs = this.stamp();
    if (tMs != null) this.reloads.push({ tMs });
  }

  private addLoad() {
    const tMs = this.stamp();
    if (tMs != null) this.loads.push({ tMs });
  }

  get startWallMs(): number {
    return this.wallStart;
  }

  frameTimes(): readonly number[] {
    return this.stoppedFlag ? this.stoppedTimes : this.liveFrames;
  }

  frameAt(tMs: number): number | null {
    if (this.frames.length === 0) return null;
    return Math.max(0, lastIndexAtOrBefore(this.frames, tMs, (frame) => frame.tMs));
  }

  bitmap(index: number): DecodedFrame {
    const meta = this.frames[index];
    if (!this.capture || !meta) throw new Error(`no captured frame ${index}`);
    return { bgra: this.capture.frame(index), width: meta.width, height: meta.height };
  }

  filmstrip(indices: number[], tileWidth: number, width: number, height: number): Promise<Buffer> {
    if (!this.capture) throw new Error("no capture");
    return captureFilmstrip(this.framesDir, indices, tileWidth, width, height);
  }

  async deleteRawFrames(): Promise<void> {
    this.capture?.release();
    this.capture = null;
    await fsp.rm(this.framesDir, { recursive: true, force: true }).catch(() => {});
  }

  async deleteAll(): Promise<void> {
    this.capture?.release();
    this.capture = null;
    await fsp.rm(this.dir, { recursive: true, force: true }).catch(() => {});
  }

  private finishCapture() {
    const capture = this.capture;
    const wall = this.wallStart ? Date.now() - this.wallStart : 0;
    this.stopDuration = wall;
    if (!capture) return;
    try {
      const stats = capture.stop();
      const index = capture.index();
      for (const frame of index.frames) {
        this.frames.push({ tMs: frame.tMs, width: frame.width, height: frame.height });
        this.stoppedTimes.push(frame.tMs);
        if (frame.dropsBefore > 0) this.drops.push({ tMs: frame.tMs, count: frame.dropsBefore });
      }
      this.stopDuration = Math.max(wall, stats.durationMs);
    } catch (error) {
      this.captureError = error instanceof Error ? error.message : String(error);
    }
    this.liveFrames = [];
  }
}
