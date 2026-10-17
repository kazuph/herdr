use super::*;
use crate::protocol::endpoint_wire::{ClientMessage, ServerMessage};
use crate::server::headless::tests::endpoint_tests::{
    connect_with_graphics_receipt, dispatch_input, owned_activation_pty, receive, wait_output,
};
use crate::server::headless::tests::{shutdown_test_runtimes, test_headless_server};

fn drain(
    server: &HeadlessServer,
    id: u64,
    stream: &mut crate::ipc::LocalStream,
) -> Vec<ServerMessage> {
    server.endpoint_measurement_barrier(id);
    let mut out = Vec::new();
    loop {
        let message = receive(stream);
        if matches!(&message, ServerMessage::EndpointControl { kind, .. } if kind == "test.measurement.barrier")
        {
            break;
        }
        out.push(message);
    }
    out
}

fn surface(messages: &[ServerMessage]) -> wire::PaneSurfaceFrame {
    messages
        .iter()
        .find_map(|m| match m {
            ServerMessage::PaneSurface(s) => Some(s.clone()),
            _ => None,
        })
        .expect("full surface")
}

fn patch_count(messages: &[ServerMessage]) -> usize {
    messages
        .iter()
        .filter(|m| {
            matches!(m, ServerMessage::EndpointControl {kind, ..}
        if kind == crate::protocol::endpoint::GRAPHICS_PATCH_KIND)
        })
        .count()
}

fn set_image(server: &mut HeadlessServer, generation: u8) {
    let pane = server.app.state.workspaces[0].focused_pane_id().unwrap();
    let key = (pane, api::schema::PANE_GRAPHICS_PRIMARY_LAYER_ID.into());
    let image_id = server.app.pane_graphics.reserve_image_id(&key).unwrap();
    // The same 512x256 RGBA geometry used by the patch reader tests (512 KiB).
    let layer = crate::app::pane_graphics::Layer::inline(
        api::schema::PaneGraphicsFormat::Rgba,
        512,
        256,
        vec![generation; 512 * 256 * 4],
        Default::default(),
        0,
    );
    server.app.pane_graphics.slots.insert(
        key,
        crate::app::pane_graphics::Slot::test(image_id, Some(layer)),
    );
}

async fn receipt_server() -> (
    HeadlessServer,
    crate::terminal::TerminalId,
    u64,
    crate::ipc::LocalStream,
    wire::PaneSurfaceFrame,
) {
    let mut server = test_headless_server();
    let terminal = owned_activation_pty(&mut server, "RECEIPT-TEXT").await;
    server.app.state.kitty_graphics_enabled = true;
    set_image(&mut server, 1);
    let (id, mut stream, _) = connect_with_graphics_receipt(&mut server, true, false, true).await;
    server.stream_endpoint_views();
    let messages = drain(&server, id, &mut stream);
    assert_eq!(patch_count(&messages), 1);
    let first = surface(&messages);
    assert!(first.graphics.assets.is_empty());
    assert_eq!(first.graphics.placements.len(), 1);
    assert!(server.endpoint_clients[&id].waiting_for_patch_ack());
    (server, terminal, id, stream, first)
}

async fn type_text(
    server: &mut HeadlessServer,
    terminal: &crate::terminal::TerminalId,
    text: &str,
) {
    server
        .app
        .terminal_runtimes
        .get(terminal)
        .unwrap()
        .send_paste(format!("{text}\n"))
        .await
        .unwrap();
    wait_output(server, terminal, &format!("ACK:{text}")).await;
}

#[tokio::test]
async fn receipt_text_frames_keep_image_state_and_original_ack_then_deliver_latest_image() {
    let (mut server, terminal, id, mut stream, first) = receipt_server().await;
    set_image(&mut server, 2);
    type_text(&mut server, &terminal, "typed-during-receipt").await;
    // Position the receipt window after real PTY setup, so CI load cannot expire the fixture.
    server.endpoint_clients.get_mut(&id).unwrap().patch_ack_wait =
        Some((first.surface_revision, Instant::now()));
    let delivery = server.endpoint_clients[&id].graphics_delivery.clone();
    let bases = server.endpoint_clients[&id].patch_bases.clone();
    let waiting = server.endpoint_clients[&id].patch_ack_wait;
    server.stream_endpoint_views();
    let messages = drain(&server, id, &mut stream);
    let text = surface(&messages);
    assert_eq!(patch_count(&messages), 0);
    assert!(text.graphics.assets.is_empty());
    assert_eq!(text.graphics.placements, first.graphics.placements);
    assert_eq!(
        text.graphics.retained_assets,
        first.graphics.retained_assets
    );
    assert_ne!(text.frame, first.frame);
    assert!(text.surface_revision > first.surface_revision);
    assert_eq!(server.endpoint_clients[&id].graphics_delivery, delivery);
    assert_eq!(server.endpoint_clients[&id].patch_bases, bases);
    assert_eq!(server.endpoint_clients[&id].patch_ack_wait, waiting);
    // Repeated unchanged text must not send or move the receipt baseline.
    server.stream_endpoint_views();
    assert!(drain(&server, id, &mut stream)
        .iter()
        .all(|m| !matches!(m, ServerMessage::PaneSurface(_))));
    set_image(&mut server, 3);
    type_text(&mut server, &terminal, "second-text-while-image-advances").await;
    server.stream_endpoint_views();
    let second_messages = drain(&server, id, &mut stream);
    let second_text = surface(&second_messages);
    assert_eq!(patch_count(&second_messages), 0);
    assert!(second_text.graphics.assets.is_empty());
    assert_eq!(second_text.graphics.placements, first.graphics.placements);
    assert_eq!(
        second_text.graphics.retained_assets,
        first.graphics.retained_assets
    );
    assert_ne!(second_text.frame, text.frame);
    assert!(second_text.surface_revision > text.surface_revision);
    assert_eq!(server.endpoint_clients[&id].graphics_delivery, delivery);
    assert_eq!(server.endpoint_clients[&id].patch_bases, bases);
    assert_eq!(server.endpoint_clients[&id].patch_ack_wait, waiting);
    protocol::write_message(
        &mut stream,
        &ClientMessage::EndpointControl {
            kind: crate::protocol::endpoint::GRAPHICS_PATCH_ACK_KIND.into(),
            data: first.surface_revision.to_string(),
        },
    )
    .unwrap();
    assert!(dispatch_input(&mut server).await);
    assert!(server.endpoint_clients[&id].patch_ack_wait.is_none());
    server.stream_endpoint_views();
    let latest = drain(&server, id, &mut stream);
    assert_eq!(patch_count(&latest), 1);
    let latest = surface(&latest);
    assert_ne!(
        latest.graphics.placements[0].asset,
        first.graphics.placements[0].asset
    );
    assert!(latest.surface_revision > text.surface_revision);
    // An old acknowledgement must not release this newer image's wait.
    protocol::write_message(
        &mut stream,
        &ClientMessage::EndpointControl {
            kind: crate::protocol::endpoint::GRAPHICS_PATCH_ACK_KIND.into(),
            data: first.surface_revision.to_string(),
        },
    )
    .unwrap();
    assert!(!dispatch_input(&mut server).await);
    assert_eq!(
        server.endpoint_clients[&id].patch_ack_wait.unwrap().0,
        latest.surface_revision
    );
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn receipt_timeout_and_miss_resume_latest_image_after_text() {
    for reason in ["timeout", "miss"] {
        let (mut server, terminal, id, mut stream, first) = receipt_server().await;
        set_image(&mut server, 2);
        type_text(&mut server, &terminal, reason).await;
        server.endpoint_clients.get_mut(&id).unwrap().patch_ack_wait =
            Some((first.surface_revision, Instant::now()));
        server.stream_endpoint_views();
        let text = surface(&drain(&server, id, &mut stream));
        assert_eq!(text.graphics.placements, first.graphics.placements);
        if reason == "timeout" {
            // This fixture uses the existing documented 500ms receipt limit.
            server.endpoint_clients.get_mut(&id).unwrap().patch_ack_wait = Some((
                first.surface_revision,
                Instant::now() - Duration::from_millis(500),
            ));
        } else {
            protocol::write_message(
                &mut stream,
                &ClientMessage::EndpointControl {
                    kind: crate::protocol::endpoint::GRAPHICS_PATCH_MISS_KIND.into(),
                    data: String::new(),
                },
            )
            .unwrap();
            assert!(dispatch_input(&mut server).await);
        }
        server.stream_endpoint_views();
        let latest = drain(&server, id, &mut stream);
        assert_eq!(patch_count(&latest), 1);
        assert_ne!(
            surface(&latest).graphics.placements[0].asset,
            first.graphics.placements[0].asset
        );
        shutdown_test_runtimes(&mut server);
    }
}

#[tokio::test]
async fn receipt_incompatible_geometry_and_failed_send_do_not_commit() {
    let (mut server, terminal, id, mut stream, first) = receipt_server().await;
    type_text(&mut server, &terminal, "held-text").await;
    server.endpoint_clients.get_mut(&id).unwrap().patch_ack_wait =
        Some((first.surface_revision, Instant::now()));
    let original = server.endpoint_clients[&id].surface.clone();
    for changed in ["size", "pane", "split", "popup", "boot", "placement", "tab"] {
        let client = server.endpoint_clients.get_mut(&id).unwrap();
        client.surface = original.clone();
        let snapshot = client.snapshot.clone();
        let baseline = client.surface.as_mut().unwrap();
        match changed {
            "size" => baseline.frame.width += 1,
            "pane" => baseline.panes[0].rect.x += 1,
            "split" => baseline.splits.push(wire::PaneSurfaceSplit {
                direction: wire::PaneSurfaceSplitDirection::Horizontal,
                pos: 1,
                area: baseline.panes[0].rect,
                hit_rect: baseline.panes[0].rect,
                path: vec![],
            }),
            "popup" => {
                baseline.popup = Some(Box::new(wire::ClientShellPopupSurface {
                    terminal_id: "not-the-current-popup".into(),
                    title: String::new(),
                    width: None,
                    height: None,
                    frame: baseline.frame.clone(),
                    mouse_reporting: false,
                    sgr_pixel_mouse: false,
                    pixel_width: 0,
                    pixel_height: 0,
                }))
            }
            "boot" => baseline.boot_id = "other-boot".into(),
            "placement" => baseline.graphics.placements[0].x += 1,
            "tab" => client.snapshot.as_mut().unwrap().focused_tab_id = Some("other-tab".into()),
            _ => unreachable!(),
        }
        server.stream_endpoint_views();
        assert!(
            !drain(&server, id, &mut stream)
                .iter()
                .any(|m| matches!(m, ServerMessage::PaneSurface(_))),
            "{changed}"
        );
        server.endpoint_clients.get_mut(&id).unwrap().snapshot = snapshot;
    }
    let client = server.endpoint_clients.get_mut(&id).unwrap();
    client.surface = original.clone();
    let waiting = client.patch_ack_wait;
    let delivery = client.graphics_delivery.clone();
    let bases = client.patch_bases.clone();
    let revision = client.surface_revision;
    client.writer.test_close();
    server.stream_endpoint_views();
    let client = &server.endpoint_clients[&id];
    assert_eq!(client.surface, original);
    assert_eq!(client.surface_revision, revision);
    assert_eq!(client.patch_ack_wait, waiting);
    assert_eq!(client.graphics_delivery, delivery);
    assert_eq!(client.patch_bases, bases);
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn receipt_placement_comparison_ignores_only_pixel_generation() {
    use super::text_receipt::{same_placement_geometry, same_popup_geometry};
    let (mut server, _, _, _, first) = receipt_server().await;
    let original = &first.graphics.placements[0];
    let mut generation = original.clone();
    generation.asset.data_fingerprint ^= 1;
    generation.asset.data_len += 1;
    assert!(same_placement_geometry(original, &generation));
    for field in [
        "source",
        "image_width",
        "image_height",
        "format",
        "id",
        "x",
        "y",
        "cols",
        "rows",
        "source_x",
        "source_y",
        "source_width",
        "source_height",
        "x_offset",
        "y_offset",
        "z",
        "scrollback",
    ] {
        let mut changed = generation.clone();
        match field {
            "source" => {
                changed.asset.source = wire::SurfaceGraphicsSource::PaneLayer {
                    pane_id: "other-pane".into(),
                    layer_id: "other-layer".into(),
                }
            }
            "image_width" => changed.asset.image_width += 1,
            "image_height" => changed.asset.image_height += 1,
            "format" => changed.asset.format = wire::SurfaceGraphicsFormat::Png,
            "id" => changed.logical_placement_id += 1,
            "x" => changed.x += 1,
            "y" => changed.y += 1,
            "cols" => changed.cols += 1,
            "rows" => changed.rows += 1,
            "source_x" => changed.source_x += 1,
            "source_y" => changed.source_y += 1,
            "source_width" => changed.source_width += 1,
            "source_height" => changed.source_height += 1,
            "x_offset" => changed.x_offset += 1,
            "y_offset" => changed.y_offset += 1,
            "z" => changed.z += 1,
            "scrollback" => changed.scrollback_offset += 1,
            _ => unreachable!(),
        }
        assert!(!same_placement_geometry(original, &changed), "{field}");
    }
    let popup = wire::ClientShellPopupSurface {
        terminal_id: "popup".into(),
        title: String::new(),
        width: None,
        height: None,
        frame: first.frame.clone(),
        mouse_reporting: false,
        sgr_pixel_mouse: false,
        pixel_width: 0,
        pixel_height: 0,
    };
    assert!(same_popup_geometry(Some(&popup), Some(&popup)));
    assert!(!same_popup_geometry(None, Some(&popup)));
    for dimension in ["width", "height", "pixels"] {
        let mut changed = popup.clone();
        match dimension {
            "width" => changed.frame.width += 1,
            "height" => changed.frame.height += 1,
            "pixels" => changed.pixel_width += 1,
            _ => unreachable!(),
        }
        assert!(
            !same_popup_geometry(Some(&popup), Some(&changed)),
            "{dimension}"
        );
    }
    shutdown_test_runtimes(&mut server);
}
