import { useEffect, useRef, useState } from "react";
import { Box, Markdown, Text } from "pixel-react";
import type { EngineInfo, NodeHandle } from "pixel-react";

import { DOC } from "./doc";
import { makeTheme } from "./theme";

const FONT_MONO = 1;
const HOLD_MS = 3000;

export function App({ info }: { info: EngineInfo }) {
  const theme = makeTheme(info.colors);
  const rem = info.basePx;
  const [cursor, setCursor] = useState(0);
  const [follow, setFollow] = useState(true);
  const scroller = useRef<NodeHandle>(null);
  const done = cursor >= DOC.length;

  useEffect(() => {
    if (done) {
      const hold = setTimeout(() => {
        setCursor(0);
        setFollow(true);
        scroller.current?.scrollTo(0);
      }, HOLD_MS);
      return () => clearTimeout(hold);
    }
    const tick = setTimeout(
      () => setCursor((at) => Math.min(DOC.length, at + 2 + Math.floor(Math.random() * 7))),
      24
    );
    return () => clearTimeout(tick);
  }, [done, cursor]);

  useEffect(() => {
    if (follow && !done) scroller.current?.scrollTo(1e9);
  }, [cursor, follow, done]);

  return (
    <Box
      style={{
        width: "100%",
        height: "100%",
        flexDirection: "column",
        background: theme.bg,
        color: theme.fg,
        fontSize: rem,
      }}
    >
      <Box
        style={{
          padding: { left: rem * 1.5, right: rem * 1.5, top: rem * 0.6, bottom: rem * 0.6 },
          border: { bottom: [1, theme.hairline] },
          justifyContent: "space-between",
          alignItems: "center",
        }}
      >
        <Text spans={[{ start: 0, end: 8, color: theme.fg, bold: true }]}>
          markdown{"  "}pixel engine demo
        </Text>
        <Text style={{ color: done ? theme.muted : theme.markdown.link }}>
          {done
            ? "complete — restarting"
            : `streaming ${Math.round((cursor / DOC.length) * 100)}%`}
        </Text>
      </Box>
      <Box
        ref={scroller}
        onWheel={() => setFollow(false)}
        style={{
          flexGrow: 1,
          overflow: "scroll",
          justifyContent: "center",
          alignItems: "start",
        }}
      >
        <Box
          style={{
            flexDirection: "column",
            maxWidth: rem * 46,
            flexGrow: 1,
            padding: { left: rem * 1.5, right: rem * 1.5, top: rem, bottom: rem * 2 },
          }}
        >
          <Markdown
            text={DOC.slice(0, cursor)}
            streaming={!done}
            theme={theme.markdown}
            rem={rem}
            monoFont={FONT_MONO}
          />
        </Box>
      </Box>
    </Box>
  );
}
