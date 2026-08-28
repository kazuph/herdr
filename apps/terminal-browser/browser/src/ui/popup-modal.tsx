import { useState } from "react";
import { Box, Text } from "pixel-react";
import type { Surface } from "pixel-react";
import { Icon } from "./icons";
import type { Theme } from "./theme";
import type { ChromeActions, ChromeLayout, PopupView } from "./types";

export function PopupModal({
  view,
  actions,
  layout,
  theme,
  surface,
}: {
  view: PopupView;
  actions: ChromeActions;
  layout: ChromeLayout;
  theme: Theme;
  surface: Surface;
}) {
  const rem = layout.rem;
  const [closeHover, setCloseHover] = useState(false);
  const headerH = Math.round(rem * 1.7);
  const cardH = headerH + view.height;
  const left = layout.page.x + Math.round((layout.page.width - view.width) / 2);
  const top =
    layout.page.y + Math.max(Math.round(rem * 0.5), Math.round((layout.page.height - cardH) / 2));
  return (
    <>
      <Box
        style={{
          position: "absolute",
          inset: { top: layout.page.y, left: layout.page.x },
          width: layout.page.width,
          height: layout.page.height,
          background: [8, 9, 12, 150],
        }}
        onPointer={(event) => {
          if (event.kind === "down") actions.popupClose();
        }}
        onWheel={() => {}}
      />
      <Box
        style={{
          position: "absolute",
          inset: { top, left },
          width: view.width,
          flexDirection: "column",
          background: theme.bg,
          cornerRadius: rem * 0.5,
          border: { width: 1, color: theme.fieldBorder },
          overflow: "hidden",
        }}
        onPointer={() => {}}
        onWheel={() => {}}
      >
        <Box
          style={{
            height: headerH,
            alignItems: "center",
            gap: rem * 0.5,
            padding: { left: rem * 0.65, right: rem * 0.35 },
            background: theme.field,
            border: { bottom: [1, theme.hairline] },
          }}
        >
          <Text style={{ fontSize: rem * 0.78, wrap: false, selectable: false }}>
            {view.title || (view.loading ? "loading…" : view.host)}
          </Text>
          <Box style={{ flexGrow: 1, flexBasis: 0 }} />
          <Text style={{ fontSize: rem * 0.72, color: theme.muted, wrap: false, selectable: false }}>
            {view.host}
          </Text>
          <Box
            style={{
              width: rem * 1.15,
              height: rem * 1.15,
              alignItems: "center",
              justifyContent: "center",
              cornerRadius: rem * 0.3,
              background: closeHover ? theme.hover : undefined,
            }}
            onClick={() => actions.popupClose()}
            onMouseEnter={() => setCloseHover(true)}
            onMouseLeave={() => setCloseHover(false)}
          >
            <Icon icon="close" size={rem * 0.8} color={theme.muted} />
          </Box>
        </Box>
        <Box
          surface={surface}
          style={{ width: view.width, height: view.height, background: theme.bg }}
          onPointer={actions.popupPointer}
          onWheel={actions.popupWheel}
          onMouseEnter={() => actions.popupHover(true)}
          onMouseLeave={() => actions.popupHover(false)}
        />
      </Box>
    </>
  );
}
