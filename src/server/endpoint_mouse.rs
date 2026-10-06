//! Targeted mouse semantics adapted from fixed upstream server/pane_input.rs.

use bytes::Bytes;
use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};

use crate::protocol::endpoint_wire::{
    ClientMouseButton, ClientMouseGeometry, ClientMouseKind, ClientMousePosition,
};
use crate::terminal::TerminalRuntime;

pub(crate) fn coherent_position(
    position: ClientMousePosition,
    geometry: Option<ClientMouseGeometry>,
    pixel_mouse: bool,
    runtime_size: (u16, u16),
    runtime_pixels: Option<(u32, u32)>,
) -> Option<crate::input::mouse::Position> {
    let (rows, cols) = runtime_size;
    let (column, row) = match position {
        ClientMousePosition::Cell { column, row }
        | ClientMousePosition::Pixels { column, row, .. } => (column, row),
    };
    if column >= cols || row >= rows {
        return None;
    }
    if let ClientMousePosition::Pixels { x, y, .. } = position {
        let exact = pixel_mouse
            && geometry.is_some_and(|geometry| {
                (geometry.rows, geometry.cols) == runtime_size
                    && runtime_pixels == Some((geometry.width_px, geometry.height_px))
                    && crate::input::mouse::HostGeometry::new(
                        geometry.cols,
                        geometry.rows,
                        geometry.width_px,
                        geometry.height_px,
                    )
                    .is_some_and(|geometry| geometry.cell(x, y) == Some((column, row)))
            });
        if exact {
            return Some(crate::input::mouse::Position::Pixels { x, y });
        }
    }
    Some(crate::input::mouse::Position::Cell { column, row })
}

pub(crate) fn apply(
    runtime: &TerminalRuntime,
    kind: ClientMouseKind,
    position: ClientMousePosition,
    geometry: Option<ClientMouseGeometry>,
    pixel_mouse: bool,
    modifiers: u8,
    lines: u16,
) -> Result<(), String> {
    let sgr_pixels = runtime.input_state().is_some_and(|state| {
        state.mouse_protocol_encoding == crate::input::MouseProtocolEncoding::SgrPixels
    });
    let Some(position) = coherent_position(
        position,
        geometry,
        pixel_mouse && sgr_pixels,
        runtime.current_size(),
        runtime.pixel_size(),
    ) else {
        return Err("mouse position is outside the target pane".into());
    };
    let kind = mouse_kind(kind);
    let modifiers = KeyModifiers::from_bits_truncate(modifiers);
    let bytes = match kind {
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => match runtime.wheel_routing() {
            Some(crate::pane::WheelRouting::MouseReport) => {
                runtime.scroll_reset();
                runtime
                    .encode_mouse_wheel(kind, position, modifiers)
                    .ok_or("failed to encode target wheel event")?
            }
            Some(crate::pane::WheelRouting::AlternateScroll) => {
                runtime.scroll_reset();
                runtime.encode_alternate_scroll(kind).unwrap_or_default()
            }
            Some(crate::pane::WheelRouting::HostScroll) | None => {
                if kind == MouseEventKind::ScrollUp {
                    runtime.scroll_up(usize::from(lines.max(1)));
                } else {
                    runtime.scroll_down(usize::from(lines.max(1)));
                }
                return Ok(());
            }
        },
        MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight => runtime
            .encode_mouse_wheel(kind, position, modifiers)
            .unwrap_or_default(),
        MouseEventKind::Down(_) | MouseEventKind::Up(_) | MouseEventKind::Drag(_) => runtime
            .encode_mouse_button(kind, position, modifiers)
            .unwrap_or_default(),
        MouseEventKind::Moved => runtime
            .encode_mouse_motion(kind, position, modifiers)
            .unwrap_or_default(),
    };
    if !bytes.is_empty() {
        if kind != MouseEventKind::Moved {
            runtime.scroll_reset();
        }
        runtime
            .try_send_bytes(Bytes::from(bytes))
            .map_err(|error| format!("target pane mouse input failed: {error}"))?;
    }
    Ok(())
}

fn mouse_kind(kind: ClientMouseKind) -> MouseEventKind {
    let button = |button| match button {
        ClientMouseButton::Left => MouseButton::Left,
        ClientMouseButton::Right => MouseButton::Right,
        ClientMouseButton::Middle => MouseButton::Middle,
    };
    match kind {
        ClientMouseKind::Down(value) => MouseEventKind::Down(button(value)),
        ClientMouseKind::Up(value) => MouseEventKind::Up(button(value)),
        ClientMouseKind::Drag(value) => MouseEventKind::Drag(button(value)),
        ClientMouseKind::Moved => MouseEventKind::Moved,
        ClientMouseKind::ScrollUp => MouseEventKind::ScrollUp,
        ClientMouseKind::ScrollDown => MouseEventKind::ScrollDown,
        ClientMouseKind::ScrollLeft => MouseEventKind::ScrollLeft,
        ClientMouseKind::ScrollRight => MouseEventKind::ScrollRight,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_geometry_must_match_real_target_size_and_canonical_cell() {
        let pixel = ClientMousePosition::Pixels {
            x: 17,
            y: 33,
            column: 2,
            row: 2,
        };
        let geometry = ClientMouseGeometry {
            cols: 37,
            rows: 6,
            width_px: 296,
            height_px: 96,
        };
        assert_eq!(
            coherent_position(pixel, Some(geometry), true, (6, 37), Some((296, 96))),
            Some(crate::input::mouse::Position::Pixels { x: 17, y: 33 })
        );
        for (enabled, size, pixels) in [
            (false, (6, 37), Some((296, 96))),
            (true, (8, 47), Some((376, 128))),
            (true, (6, 37), None),
        ] {
            assert_eq!(
                coherent_position(pixel, Some(geometry), enabled, size, pixels),
                Some(crate::input::mouse::Position::Cell { column: 2, row: 2 })
            );
        }
        assert_eq!(
            coherent_position(
                ClientMousePosition::Cell { column: 37, row: 2 },
                None,
                false,
                (6, 37),
                None
            ),
            None
        );
    }
}
