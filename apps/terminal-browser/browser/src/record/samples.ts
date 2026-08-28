const EXPORT_ANIMATION_STEP_MS = 33; // hm
export const CLICK_PULSE_MS = 450;
export const CURSOR_LERP_MAX_MS = 200;
export const LINK_HOLD_MS = 2500;
export const TOAST_FADE_MS = 300;

export interface SampleInput {
  frameTimes: readonly number[];
  pointer: readonly { tMs: number }[];
  clicks: readonly { tMs: number }[];
  links: readonly { tMs: number }[];
  durationMs: number;
}

function frameCadenceMs(frameTimes: readonly number[]): number {
  const deltas: number[] = [];
  for (let i = 1; i < frameTimes.length; i++) {
    const delta = frameTimes[i] - frameTimes[i - 1];
    if (delta > 0) deltas.push(delta);
  }
  if (deltas.length < 8) return EXPORT_ANIMATION_STEP_MS;
  deltas.sort((a, b) => a - b);
  return Math.min(EXPORT_ANIMATION_STEP_MS, Math.max(1, deltas[deltas.length >> 1]));
}

export function buildSampleTimes(input: SampleInput): number[] {
  const times = new Set<number>([0, ...input.frameTimes]);
  for (const sample of input.pointer) times.add(sample.tMs);
  for (const click of input.clicks) times.add(click.tMs);
  for (const link of input.links) times.add(link.tMs);
  const cadence = frameCadenceMs(input.frameTimes);
  const kept = [...times].filter((t) => t <= input.durationMs).sort((a, b) => a - b);
  const out: number[] = [];
  let prev = 0;
  for (const t of kept) {
    for (let fill = prev + cadence; fill < t; fill += cadence) out.push(fill);
    out.push(t);
    prev = t;
  }
  for (let fill = prev + cadence; fill < input.durationMs; fill += cadence) {
    out.push(fill);
  }
  return out;
}
