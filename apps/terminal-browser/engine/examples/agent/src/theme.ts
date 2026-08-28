import type { EngineInfo, Rgba } from "pixel-react";

export interface Theme {
  bg: Rgba;
  bgAlt: Rgba;
  fg: Rgba;
  muted: Rgba;
  accent: Rgba;
  green: Rgba;
  red: Rgba;
  chipBg: Rgba;
  hairline: Rgba;
  selection: Rgba;
  sidebarBg: Rgba;
  itemHover: Rgba;
  itemActive: Rgba;
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

export function makeTheme(colors: EngineInfo["colors"]): Theme {
  const bg = colors.background ?? [22, 22, 30, 255];
  const fg = colors.foreground ?? [222, 220, 235, 255];
  const accent = colors.palette[13] ?? colors.palette[12] ?? [159, 134, 235, 255];
  return {
    bg,
    bgAlt: mix(bg, fg, 0.05),
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
    itemActive: mix(bg, accent, 0.3),
  };
}
