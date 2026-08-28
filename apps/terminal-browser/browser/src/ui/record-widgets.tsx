import { Box, Text } from "pixel-react";
import type { Rgba } from "pixel-react";
import type { Rect } from "../record/model";
import { Icon } from "./icons";
import { mix, withAlpha } from "./theme";
import type { Theme } from "./theme";

export function DismissButton({
  rem,
  theme,
  onClick,
}: {
  rem: number;
  theme: Theme;
  onClick(): void;
}) {
  return (
    <Box
      style={{
        width: rem * 1.2,
        height: rem * 1.2,
        alignItems: "center",
        justifyContent: "center",
        cornerRadius: rem * 0.6,
        background: withAlpha(theme.hoverStrong, 245),
        hoverBackground: theme.red,
        border: { width: 1, color: theme.fieldBorder },
        flexShrink: 0,
      }}
      onClick={onClick}
    >
      <Icon icon="close" size={rem * 0.65} color={theme.fg} />
    </Box>
  );
}

export interface PopupMenuItem {
  label: string;
  danger?: boolean;
  run(): void;
}

export function PopupMenu({
  items,
  width,
  rem,
  theme,
  focused = null,
  onPick,
  onDismiss,
}: {
  items: PopupMenuItem[];
  width: number;
  rem: number;
  theme: Theme;
  focused?: number | null;
  onPick(): void;
  onDismiss(): void;
}) {
  return (
    <Box
      style={{
        width,
        flexDirection: "column",
        padding: rem * 0.2,
        gap: 2,
        background: mix(theme.bg, [0, 0, 0, 255], 0.25),
        cornerRadius: rem * 0.35,
        border: { width: 1, color: theme.fieldBorder },
      }}
      onClickOutside={onDismiss}
    >
      {items.map((item, index) => (
        <Box
          key={item.label}
          style={{
            height: rem * 1.4,
            alignItems: "center",
            padding: { left: rem * 0.5, right: rem * 0.5 },
            cornerRadius: rem * 0.25,
            background: item.danger
              ? mix(theme.bg, theme.red, 0.3)
              : index === focused
                ? theme.hover
                : undefined,
            hoverBackground: item.danger ? theme.red : theme.hover,
          }}
          onClick={() => {
            onPick();
            item.run();
          }}
        >
          <Text
            style={{
              fontSize: rem * 0.75,
              color: item.danger ? mix(theme.red, theme.fg, 0.8) : theme.fg,
              wrap: false,
              selectable: false,
            }}
          >
            {item.label}
          </Text>
        </Box>
      ))}
    </Box>
  );
}

export function ShadeAround({
  rect,
  width,
  height,
  shade,
}: {
  rect: Rect;
  width: number;
  height: number;
  shade: Rgba;
}) {
  const regions = [
    { x: 0, y: 0, width, height: rect.y },
    { x: 0, y: rect.y + rect.height, width, height: height - rect.y - rect.height },
    { x: 0, y: rect.y, width: rect.x, height: rect.height },
    { x: rect.x + rect.width, y: rect.y, width: width - rect.x - rect.width, height: rect.height },
  ];
  return (
    <>
      {regions.map((region, index) =>
        region.width > 0 && region.height > 0 ? (
          <Box
            key={index}
            style={{
              position: "absolute",
              inset: { top: region.y, left: region.x },
              width: region.width,
              height: region.height,
              background: shade,
            }}
          />
        ) : null,
      )}
    </>
  );
}
