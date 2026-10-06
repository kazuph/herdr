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
    if surface.width != area.width
        || surface.height != area.height
        || surface.cells.len() != usize::from(area.width) * usize::from(area.height)
        || area.right() > frame.width
        || area.bottom() > frame.height
    {
        return false;
    }
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
    for y in 0..area.height {
        for x in 0..area.width {
            let source = &surface.cells[usize::from(y) * usize::from(area.width) + usize::from(x)];
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
        .filter(|cursor| cursor.x < area.width && cursor.y < area.height)
        .map(|cursor| crate::protocol::CursorState {
            x: area.x + cursor.x,
            y: area.y + cursor.y,
            visible: cursor.visible,
            shape: cursor.shape,
        });
    // Desired graphics scenes are handled by the endpoint-qualified graphics owner separately.
    frame.graphics.clone_from(&surface.graphics);
    true
}
