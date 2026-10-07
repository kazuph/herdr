use ratatui::layout::Rect;

pub(super) fn compose_popup(
    frame: &mut crate::protocol::FrameData,
    area: Rect,
    popup: &crate::protocol::endpoint_wire::ClientShellPopupSurface,
    palette: &crate::app::state::Palette,
) -> bool {
    use ratatui::widgets::Widget;

    let size = |size| match size {
        crate::protocol::endpoint_wire::ClientShellPopupSize::Cells(cells) => {
            crate::popup_size::PopupSize::Cells(cells)
        }
        crate::protocol::endpoint_wire::ClientShellPopupSize::Percent(percent) => {
            crate::popup_size::PopupSize::Percent(percent)
        }
    };
    let Some(geometry) = crate::popup_size::resolve_popup_geometry(
        popup.width.map(size),
        popup.height.map(size),
        area,
    ) else {
        return false;
    };
    if popup.frame.width != geometry.inner.width || popup.frame.height != geometry.inner.height {
        return false;
    }
    let Some(mut buffer) = frame.to_ratatui_buffer() else {
        return false;
    };
    let block = ratatui::widgets::Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_style(ratatui::style::Style::default().fg(palette.accent))
        .title(popup.title.clone())
        .style(ratatui::style::Style::default().bg(palette.panel_bg));
    ratatui::widgets::Clear.render(geometry.outer, &mut buffer);
    block.render(geometry.outer, &mut buffer);
    let mut composed =
        crate::protocol::FrameData::from_ratatui_buffer_with_hyperlinks(&buffer, None, &[]);
    composed.hyperlinks.clone_from(&frame.hyperlinks);
    composed.graphics.clone_from(&frame.graphics);
    for (index, (cell, original)) in composed.cells.iter_mut().zip(&frame.cells).enumerate() {
        let position = (
            (index % usize::from(frame.width)) as u16,
            (index / usize::from(frame.width)) as u16,
        );
        if !geometry.outer.contains(position.into())
            && *cell
                == (crate::protocol::CellData {
                    hyperlink: None,
                    ..original.clone()
                })
        {
            cell.hyperlink = original.hyperlink;
        }
    }
    if !compose(&mut composed, geometry.inner, &popup.frame) {
        return false;
    }
    *frame = composed;
    true
}

pub(super) fn compose(
    frame: &mut crate::protocol::FrameData,
    area: Rect,
    surface: &crate::protocol::endpoint_wire::FrameData,
) -> bool {
    if surface.cells.len() != usize::from(surface.width) * usize::from(surface.height)
        || area.right() > frame.width
        || area.bottom() > frame.height
    {
        return false;
    }
    // Across a resize the previous frame stays visible, clipped to the new area, until the
    // endpoint's frame for the new geometry arrives. It is never stretched or offset.
    let exact = surface.width == area.width && surface.height == area.height;
    let width = surface.width.min(area.width);
    let height = surface.height.min(area.height);
    let Ok(hyperlink_base) = u32::try_from(frame.hyperlinks.len()) else {
        return false;
    };
    if surface.cells.iter().any(|cell| {
        cell.hyperlink.is_some_and(|index| {
            usize::try_from(index).map_or(true, |index| index >= surface.hyperlinks.len())
        })
    }) {
        return false;
    }
    frame.hyperlinks.extend(surface.hyperlinks.clone());
    for y in 0..height {
        for x in 0..width {
            let source =
                &surface.cells[usize::from(y) * usize::from(surface.width) + usize::from(x)];
            let target = &mut frame.cells
                [usize::from(area.y + y) * usize::from(frame.width) + usize::from(area.x + x)];
            *target = crate::protocol::CellData {
                symbol: source.symbol.clone(),
                fg: source.fg,
                bg: source.bg,
                modifier: source.modifier,
                skip: source.skip,
                hyperlink: source.hyperlink.map(|index| index + hyperlink_base),
            };
        }
    }
    frame.cursor = surface
        .cursor
        .as_ref()
        .filter(|cursor| exact && cursor.x < area.width && cursor.y < area.height)
        .map(|cursor| crate::protocol::CursorState {
            x: area.x + cursor.x,
            y: area.y + cursor.y,
            visible: cursor.visible,
            shape: cursor.shape,
        });
    // Desired graphics scenes are handled by the endpoint-qualified graphics owner separately.
    if exact {
        frame.graphics.clone_from(&surface.graphics);
    }
    exact
}

/// Hyperlinks of composed surface cells. A link survives only where no overlay redrew its cell.
#[derive(Default)]
pub(crate) struct Links {
    uris: Vec<String>,
    cells: Vec<LinkedCell>,
}

struct LinkedCell {
    x: u16,
    y: u16,
    index: u32,
    cell: ratatui::buffer::Cell,
}

impl Links {
    fn add_frame(&mut self, surface: &crate::protocol::endpoint_wire::FrameData) -> u32 {
        let base = u32::try_from(self.uris.len()).unwrap_or(u32::MAX);
        self.uris.extend(surface.hyperlinks.iter().cloned());
        base
    }

    fn clear_area(&mut self, area: Rect) {
        self.cells
            .retain(|linked| !area.contains((linked.x, linked.y).into()));
    }

    pub(crate) fn apply(self, frame: &mut crate::protocol::FrameData) {
        if self.uris.is_empty() {
            return;
        }
        let width = usize::from(frame.width);
        let mut used = vec![None::<u32>; self.uris.len()];
        let mut uris = Vec::new();
        for linked in self.cells {
            let index = usize::from(linked.y) * width + usize::from(linked.x);
            let Some(cell) = frame.cells.get_mut(index) else {
                continue;
            };
            let unchanged = cell.symbol == linked.cell.symbol()
                && cell.fg == crate::protocol::color_to_u32(linked.cell.fg)
                && cell.bg == crate::protocol::color_to_u32(linked.cell.bg)
                && cell.modifier == linked.cell.modifier.bits()
                && cell.skip == linked.cell.skip;
            if !unchanged {
                continue;
            }
            let Some(slot) = used.get_mut(linked.index as usize) else {
                continue;
            };
            let mapped = *slot.get_or_insert_with(|| {
                uris.push(self.uris[linked.index as usize].clone());
                (uris.len() - 1) as u32
            });
            cell.hyperlink = Some(mapped);
        }
        frame.hyperlinks = uris;
    }
}

fn write_cell(
    target: &mut ratatui::buffer::Cell,
    source: &crate::protocol::endpoint_wire::CellData,
) {
    target.set_symbol(&source.symbol);
    target.fg = crate::protocol::u32_to_color(source.fg);
    target.bg = crate::protocol::u32_to_color(source.bg);
    // Keep Herdr's extension bits (underline style) intact through the buffer.
    target.modifier = ratatui::style::Modifier::from_bits_retain(source.modifier);
    target.skip = source.skip;
}

/// Buffer form of [`compose`]: draws `surface` into `area`, clipped when the geometry differs.
/// Returns true only for an exact geometry, which is also the only case granting a cursor.
pub(crate) fn compose_into(
    buffer: &mut ratatui::buffer::Buffer,
    area: Rect,
    surface: &crate::protocol::endpoint_wire::FrameData,
    links: &mut Links,
    cursor: &mut Option<crate::protocol::CursorState>,
) -> bool {
    if surface.cells.len() != usize::from(surface.width) * usize::from(surface.height)
        || area.right() > buffer.area.right()
        || area.bottom() > buffer.area.bottom()
    {
        return false;
    }
    let exact = surface.width == area.width && surface.height == area.height;
    let width = surface.width.min(area.width);
    let height = surface.height.min(area.height);
    let base = links.add_frame(surface);
    links.clear_area(area);
    for y in 0..height {
        let row = usize::from(y) * usize::from(surface.width);
        for x in 0..width {
            let source = &surface.cells[row + usize::from(x)];
            let position = (area.x + x, area.y + y);
            let Some(target) = buffer.cell_mut(position) else {
                continue;
            };
            write_cell(target, source);
            if let Some(index) = source
                .hyperlink
                .filter(|index| (*index as usize) < surface.hyperlinks.len())
            {
                links.cells.push(LinkedCell {
                    x: position.0,
                    y: position.1,
                    index: base + index,
                    cell: target.clone(),
                });
            }
        }
    }
    *cursor = surface
        .cursor
        .as_ref()
        .filter(|state| exact && state.x < area.width && state.y < area.height)
        .map(|state| crate::protocol::CursorState {
            x: area.x + state.x,
            y: area.y + state.y,
            visible: state.visible,
            shape: state.shape,
        });
    exact
}

/// Buffer form of [`compose_popup`].
pub(crate) fn compose_popup_into(
    buffer: &mut ratatui::buffer::Buffer,
    area: Rect,
    popup: &crate::protocol::endpoint_wire::ClientShellPopupSurface,
    palette: &crate::app::state::Palette,
    links: &mut Links,
    cursor: &mut Option<crate::protocol::CursorState>,
) -> bool {
    use ratatui::widgets::Widget;

    let size = |size| match size {
        crate::protocol::endpoint_wire::ClientShellPopupSize::Cells(cells) => {
            crate::popup_size::PopupSize::Cells(cells)
        }
        crate::protocol::endpoint_wire::ClientShellPopupSize::Percent(percent) => {
            crate::popup_size::PopupSize::Percent(percent)
        }
    };
    let Some(geometry) = crate::popup_size::resolve_popup_geometry(
        popup.width.map(size),
        popup.height.map(size),
        area,
    ) else {
        return false;
    };
    if popup.frame.width != geometry.inner.width || popup.frame.height != geometry.inner.height {
        return false;
    }
    let block = ratatui::widgets::Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_style(ratatui::style::Style::default().fg(palette.accent))
        .title(popup.title.clone())
        .style(ratatui::style::Style::default().bg(palette.panel_bg));
    ratatui::widgets::Clear.render(geometry.outer, buffer);
    block.render(geometry.outer, buffer);
    links.clear_area(geometry.outer);
    compose_into(buffer, geometry.inner, &popup.frame, links, cursor)
}
