import type { EngineInfo, MarkdownTheme, Rgba } from "pixel-react";

const FALLBACK_BG: Rgba = [22, 22, 30, 255];
const FALLBACK_FG: Rgba = [222, 220, 235, 255];

export interface Theme {
  bg: Rgba;
  fg: Rgba;
  muted: Rgba;
  hairline: Rgba;
  markdown: MarkdownTheme;
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
  const ansi = (bright: number, normal: number, fallback: Rgba) =>
    colors.palette[bright] ?? colors.palette[normal] ?? fallback;

  const magenta = ansi(13, 5, [197, 134, 235, 255]);
  const blue = ansi(12, 4, [130, 170, 255, 255]);
  const green = ansi(10, 2, [140, 210, 150, 255]);
  const yellow = ansi(11, 3, [225, 195, 120, 255]);
  const cyan = ansi(14, 6, [120, 210, 220, 255]);
  const muted = mix(fg, bg, 0.45);

  return {
    bg,
    fg,
    muted,
    hairline: mix(bg, fg, 0.15),
    markdown: {
      fg,
      muted,
      link: blue,
      inlineCode: cyan,
      codeBg: mix(bg, fg, 0.07),
      separator: mix(bg, fg, 0.18),
      syntax: {
        keyword: magenta,
        string: green,
        comment: muted,
        function: blue,
        number: yellow,
        type: cyan,
        constant: yellow,
        property: cyan,
        variable: fg,
        operator: mix(fg, bg, 0.25),
        punctuation: mix(fg, bg, 0.3),
        tag: magenta,
        attribute: yellow,
        constructor: blue,
        escape: yellow,
        embedded: fg,
      },
    },
  };
}
