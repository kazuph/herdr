import type { EngineInfo, Rgba } from "pixel-react";

const FALLBACK_BG: Rgba = [22, 22, 30, 255];
const FALLBACK_FG: Rgba = [222, 220, 235, 255];
const FALLBACK_ACCENT: Rgba = [159, 134, 235, 255];

export interface Theme {
  bg: Rgba;
  fg: Rgba;
  muted: Rgba;
  sidebarBg: Rgba;
  itemHover: Rgba;
  itemActive: Rgba;
  accent: Rgba;
  selection: Rgba;
  chipBg: Rgba;
  hairline: Rgba;
}

function mix(base: Rgba, toward: Rgba, t: number): Rgba {
  const channel = (b: number, w: number) => Math.round(b + (w - b) * t);
  return [
    channel(base[0], toward[0]),
    channel(base[1], toward[1]),
    channel(base[2], toward[2]),
    255,
  ];
}

/** Derives the palette from the terminal's reported colors, mixing toward
 * fg/bg so it works for both dark and light themes. */
export function makeTheme(colors: EngineInfo["colors"]): Theme {
  const bg = colors.background ?? FALLBACK_BG;
  const fg = colors.foreground ?? FALLBACK_FG;
  // ANSI 13 (bright magenta) reads as "accent" in most themes.
  const accent = colors.palette[13] ?? colors.palette[12] ?? FALLBACK_ACCENT;

  return {
    bg,
    fg,
    muted: mix(fg, bg, 0.45),
    sidebarBg: mix(bg, fg, 0.04),
    itemHover: mix(bg, fg, 0.1),
    itemActive: mix(bg, accent, 0.35),
    accent,
    selection: mix(bg, accent, 0.35),
    chipBg: mix(bg, fg, 0.09),
    hairline: mix(bg, fg, 0.15),
  };
}
