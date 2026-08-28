import { useState } from "react";
import { Box, createRoot, Text } from "pixel-react";
import type { NodeHandle } from "pixel-react";

let list: NodeHandle | null = null;
let offset = 0;

const root = createRoot({
  onKey(event) {
    if (event.mods.ctrl && (event.key === "q" || event.key === "c")) {
      root.stop();
      process.exit(0);
    }
    const page = root.info.height * 0.8;
    if (event.key === "down") list?.scrollTo((offset += page), true);
    if (event.key === "up") list?.scrollTo((offset = Math.max(0, offset - page)), true);
  },
});

const TOTAL = 10_000;
const OVERSCAN = 5;
const rem = root.info.basePx;
const ROW_H = Math.round(rem * 1.6);

function VirtualList() {
  const [scroll, setScroll] = useState(0);
  const first = Math.max(0, Math.floor(scroll / ROW_H) - OVERSCAN);
  const last = Math.min(
    TOTAL,
    Math.ceil((scroll + root.info.height) / ROW_H) + OVERSCAN
  );
  const rows = Array.from({ length: last - first }, (_, i) => first + i);

  return (
    <Box
      style={{
        width: "100%",
        height: "100%",
        background: "#16161e",
        color: "#dedceb",
        fontSize: rem,
        flexDirection: "column",
      }}
    >
      <Text style={{ padding: rem * 0.5, color: "#9f86eb" }}>
        {`virtualized: ${rows.length} of ${TOTAL} rows mounted / arrows page / ctrl-q quits`}
      </Text>
      <Box
        ref={(handle) => (list = handle)}
        style={{ flexGrow: 1, flexBasis: 0, overflow: "scroll", flexDirection: "column" }}
        contentHeight={TOTAL * ROW_H}
        onScroll={(e) => {
          offset = e.offset;
          setScroll(e.offset);
        }}
      >
        <Box
          style={{
            position: "absolute",
            inset: { top: first * ROW_H, left: 0 },
            flexDirection: "column",
          }}
        >
          {rows.map((i) => (
            <Text
              key={i}
              style={{
                height: ROW_H,
                padding: { left: rem },
                color: i % 10 === 0 ? "#9f86eb" : undefined,
              }}
            >
              {`row ${i}`}
            </Text>
          ))}
        </Box>
      </Box>
    </Box>
  );
}

root.render(<VirtualList />);
