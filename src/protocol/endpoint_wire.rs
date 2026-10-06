//! Frozen endpoint generation-1 envelope and payloads from upstream
//! `5da0a01e1eedda054db0c81dd3a780000c40d9f0`.
//!
//! Kept distinct from the fork's private wire: both variant indices and every
//! reachable bincode field are ABI. Snapshot extensions belong in named JSON
//! controls, never in these binary types. Framing limits remain fork-owned.

// The complete frozen envelope includes optional operations not yet negotiated
// by every server. Keeping its reachable types preserves generation-1 bytes.
#![allow(dead_code)]

pub use super::{MAX_FRAME_SIZE, MAX_GRAPHICS_FRAME_SIZE};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// First-frame discrimination happens once; the caller pins that codec for
/// the lifetime of the connection. Never retry a failed decode as another ABI.
#[derive(Debug)]
pub(crate) enum InitialHello {
    Private(super::ClientMessage),
    Endpoint(super::endpoint::EndpointClientHello),
}

pub(crate) fn read_initial_hello<R: std::io::Read>(
    reader: &mut R,
) -> Result<InitialHello, super::FramingError> {
    let payload = super::read_payload(reader, MAX_FRAME_SIZE)?;
    match payload.first() {
        Some(20) => {
            let message: ClientMessage = super::decode_payload(&payload)?;
            match message {
                ClientMessage::EndpointControl { kind, data }
                    if kind == super::endpoint::ENDPOINT_HELLO_KIND =>
                {
                    let hello = serde_json::from_str(&data)
                        .map_err(|error| super::FramingError::Bincode(error.to_string()))?;
                    Ok(InitialHello::Endpoint(hello))
                }
                _ => Err(super::FramingError::Bincode(
                    "expected endpoint hello".into(),
                )),
            }
        }
        Some(0) => {
            let message = super::decode_payload(&payload)?;
            if matches!(message, super::ClientMessage::Hello { .. }) {
                Ok(InitialHello::Private(message))
            } else {
                Err(super::FramingError::Bincode(
                    "expected private hello".into(),
                ))
            }
        }
        _ => Err(super::FramingError::Bincode(
            "expected a handshake envelope".into(),
        )),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowsKeyRecord {
    pub key_down: bool,
    pub repeat_count: u16,
    pub virtual_key_code: u16,
    pub virtual_scan_code: u16,
    pub unicode: u16,
    pub control_key_state: u32,
}

/// Render payload encoding negotiated during client handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RenderEncoding {
    /// Send full semantic FrameData values. This is the local/default mode.
    SemanticFrame,
    /// Send already-diffed terminal ANSI byte streams.
    TerminalAnsi,
}

/// Size of the pane surface requested by a client-owned shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientSurfaceSize {
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientKeyKind {
    Press,
    Repeat,
    Release,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ClientKeyCode {
    Backspace,
    Enter,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Tab,
    BackTab,
    Delete,
    Insert,
    Esc,
    Char(char),
    F(u8),
    Null,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ClientMouseButton {
    Left,
    Right,
    Middle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientMouseKind {
    Down(ClientMouseButton),
    Up(ClientMouseButton),
    Drag(ClientMouseButton),
    Moved,
    ScrollUp,
    ScrollDown,
    ScrollLeft,
    ScrollRight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientMousePosition {
    Cell {
        column: u16,
        row: u16,
    },
    Pixels {
        x: u32,
        y: u32,
        column: u16,
        row: u16,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientMouseGeometry {
    pub cols: u16,
    pub rows: u16,
    pub width_px: u32,
    pub height_px: u32,
}

/// Pane-domain input after the client has classified and consumed shell actions.
///
/// Keys are semantic rather than outer-terminal VT bytes so the target pane can
/// encode them for the child application's negotiated keyboard protocol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientPaneInputEvent {
    Key {
        code: ClientKeyCode,
        modifiers: u8,
        kind: ClientKeyKind,
        repeat_count: u16,
        shifted_codepoint: Option<u32>,
        generated_text: Option<String>,
        tracks_release: bool,
        physical_key_id: Option<u32>,
        windows_record: Option<WindowsKeyRecord>,
    },
    TextCommit(String),
    Mouse {
        kind: ClientMouseKind,
        position: ClientMousePosition,
        geometry: Option<ClientMouseGeometry>,
        modifiers: u8,
        lines: u16,
    },
    Paste(String),
}

/// Messages sent from the client to the server over the client protocol socket.
///
/// Variant order is frozen for endpoint generation 1. Add compatible endpoint
/// behavior through `EndpointControl` or advertised API methods, not new enum variants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientMessage {
    /// Direct terminal handshake: announces protocol version and terminal dimensions.
    TerminalHello {
        version: u32,
        cols: u16,
        rows: u16,
        cell_width_px: u32,
        cell_height_px: u32,
        pixel_mouse: bool,
    },

    /// Raw input bytes read from the client's stdin.
    Input {
        /// Raw terminal input (possibly multi-byte escape sequences).
        data: Vec<u8>,
    },

    /// Image bytes read from the client's local clipboard for remote paste bridging.
    ClipboardImage {
        /// Stable terminal target selected by the client that read the clipboard.
        target: ClientClipboardImageTarget,
        /// Image file extension without a leading dot.
        extension: String,
        /// Raw image bytes.
        data: Vec<u8>,
    },

    /// Terminal resize notification from the client.
    Resize {
        /// New terminal width in columns.
        cols: u16,
        /// New terminal height in rows.
        rows: u16,
        /// Width of a terminal cell in physical pixels, or 0 when client-side Kitty graphics are disabled.
        cell_width_px: u32,
        /// Height of a terminal cell in physical pixels, or 0 when unavailable.
        cell_height_px: u32,
        /// Whether this resize carries coherent exact geometry for SGR pixel mouse input.
        pixel_mouse: bool,
    },

    /// Graceful disconnect request.
    Detach,

    /// Switch this connection into direct terminal attach mode.
    AttachTerminal {
        /// Terminal id to attach to.
        terminal_id: String,
        /// Replace an existing writable attach owner for this terminal.
        takeover: bool,
    },

    /// Scroll input handled by a direct terminal attach client.
    AttachScroll {
        /// Original input source for routing.
        source: AttachScrollSource,
        /// Scroll direction.
        direction: AttachScrollDirection,
        /// Number of terminal rows to move when using host scrollback.
        lines: u16,
        /// Mouse column relative to the attached terminal, when available.
        column: Option<u16>,
        /// Mouse row relative to the attached terminal, when available.
        row: Option<u16>,
        /// Crossterm-compatible modifier bits for forwarded mouse wheel events.
        modifiers: u8,
    },

    /// Switch this connection into read-only terminal observe mode.
    ObserveTerminal {
        /// Pane, terminal, or agent target to observe.
        target: String,
    },

    /// Switch this connection into writable terminal control mode.
    ControlTerminal {
        /// Pane, terminal, or agent target to control.
        target: String,
        /// Replace an existing writable controller for this terminal.
        takeover: bool,
    },

    /// Result of the one armed Herdr-owned direct Kitty transmission.
    GraphicsTransmissionResult {
        transfer_id: u64,
        image_id: u32,
        success: bool,
    },

    /// The direct command was written and flushed; terminal response timing starts now.
    GraphicsTransmissionStarted { transfer_id: u64, image_id: u32 },

    /// Handshake for the client-owned shell around one pane surface.
    ClientShellHello {
        version: u32,
        cell_width_px: u32,
        cell_height_px: u32,
        surface_size: ClientSurfaceSize,
        pixel_mouse: bool,
        direct_graphics: bool,
        /// Whether the endpoint's keymap, rather than the client's, owns shell bindings.
        endpoint_keybindings: bool,
        /// Whether this client wants shell mouse capture even without pane demand.
        mouse_capture: bool,
    },

    /// Resize the pane viewport of a client-owned shell.
    ClientShellResize {
        cell_width_px: u32,
        cell_height_px: u32,
        surface_size: ClientSurfaceSize,
        /// Whether this resize carries coherent exact geometry for SGR pixel mouse input.
        pixel_mouse: bool,
    },

    /// Deliver client-classified semantic input directly to a stable pane target.
    ClientShellPaneInput {
        pane_id: String,
        events: Vec<ClientPaneInputEvent>,
    },

    /// Deliver client-classified semantic input to the active popup terminal.
    ClientShellPopupInput {
        terminal_id: String,
        events: Vec<ClientPaneInputEvent>,
    },

    /// Invoke one endpoint operation through this client shell's selected connection.
    ClientShellEndpointRequest { boot_id: String, request: String },

    /// Deliver one structured mouse event to a directly attached terminal.
    AttachMouse {
        kind: ClientMouseKind,
        position: ClientMousePosition,
        geometry: Option<ClientMouseGeometry>,
        modifiers: u8,
        lines: u16,
    },

    /// Publish one host terminal color or appearance update observed by a client-owned shell.
    ClientShellHostTheme { update: ClientHostThemeUpdate },

    /// Publish whether the outer terminal containing a client shell has focus.
    ClientShellFocus { focused: bool },

    /// Update this client's shell mouse-capture preference after config reload.
    ClientShellMouseCapture { enabled: bool },

    /// Extensible named control message for the stable client-owned endpoint protocol.
    ///
    /// This variant is append-only. Its bincode tag and two-string payload are part
    /// of endpoint generation 1 and must not change.
    EndpointControl { kind: String, data: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientHostColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl From<crate::terminal_theme::RgbColor> for ClientHostColor {
    fn from(color: crate::terminal_theme::RgbColor) -> Self {
        Self {
            r: color.r,
            g: color.g,
            b: color.b,
        }
    }
}

impl From<ClientHostColor> for crate::terminal_theme::RgbColor {
    fn from(color: ClientHostColor) -> Self {
        Self {
            r: color.r,
            g: color.g,
            b: color.b,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientHostDefaultColorKind {
    Foreground,
    Background,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientHostAppearance {
    Dark,
    Light,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientHostThemeUpdate {
    DefaultColor {
        kind: ClientHostDefaultColorKind,
        color: ClientHostColor,
    },
    PaletteColors(Vec<(u8, ClientHostColor)>),
    Appearance(ClientHostAppearance),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientClipboardImageTarget {
    DirectTerminal,
    Pane(String),
    Popup(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttachScrollDirection {
    Up,
    Down,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttachScrollSource {
    Wheel,
    PageKey {
        /// Original key bytes to forward when the child application owns page keys.
        input: Vec<u8>,
    },
}

// ---------------------------------------------------------------------------
// Server → Client messages
// ---------------------------------------------------------------------------

/// A single cell in a rendered frame, serialized independently from ratatui's
/// `Cell` type to keep the wire protocol stable.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize, bincode::Decode)]
pub struct CellData {
    /// Grapheme cluster displayed in this cell (usually 1–2 chars).
    pub symbol: String,
    /// Foreground color as a packed u32 (0xAARRGGBB or ratatui Color index).
    pub fg: u32,
    /// Background color as a packed u32.
    pub bg: u32,
    /// Bitmask of style modifiers (bold, italic, etc.) plus Herdr extension bits.
    pub modifier: u16,
    /// Whether this cell should be skipped during diff-based rendering.
    pub skip: bool,
    /// Index into `FrameData::hyperlinks` for this cell's OSC 8 target, if any.
    pub hyperlink: Option<u32>,
}

impl Clone for CellData {
    fn clone(&self) -> Self {
        Self {
            symbol: self.symbol.clone(),
            ..*self
        }
    }

    fn clone_from(&mut self, source: &Self) {
        let mut symbol = std::mem::take(&mut self.symbol);
        symbol.clone_from(&source.symbol);
        *self = Self { symbol, ..*source };
    }
}

impl CellData {
    pub(crate) fn from_ratatui_cell(cell: &ratatui::buffer::Cell) -> Self {
        Self {
            symbol: cell.symbol().to_owned(),
            fg: color_to_u32(cell.fg),
            bg: color_to_u32(cell.bg),
            modifier: modifier_to_u16(cell.modifier),
            skip: cell.skip,
            hyperlink: None,
        }
    }
}

/// Cursor shape encoded as a DECSCUSR parameter.
///
/// 0 = terminal default, 1 = blinking block, 2 = steady block,
/// 3 = blinking underline, 4 = steady underline, 5 = blinking bar,
/// 6 = steady bar.
pub type CursorShapeParam = u8;

/// Cursor position within a rendered frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, bincode::Decode)]
pub struct CursorState {
    /// Column offset (0-based) of the cursor.
    pub x: u16,
    /// Row offset (0-based) of the cursor.
    pub y: u16,
    /// Whether the cursor is visible.
    pub visible: bool,
    /// Cursor shape as a DECSCUSR parameter.
    #[serde(default)]
    pub shape: CursorShapeParam,
}

/// A rendered frame to be displayed by the client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameData {
    /// Cells in row-major order. Length must equal `width * height`.
    pub cells: Vec<CellData>,
    /// Frame width in columns.
    pub width: u16,
    /// Frame height in rows.
    pub height: u16,
    /// Cursor state for this frame, if applicable.
    pub cursor: Option<CursorState>,
    /// OSC 8 hyperlink URIs referenced by cells.
    pub hyperlinks: Vec<String>,
    /// Kitty graphics protocol bytes to apply after the text frame.
    pub graphics: Vec<u8>,
}

impl FrameData {
    /// Creates a `FrameData` from a ratatui `Buffer` and optional cursor.
    ///
    /// This converts ratatui's internal cell representation into the
    /// wire-protocol cell format. The conversion is lossless for all
    /// commonly used cell attributes.
    #[cfg(test)]
    pub fn from_ratatui_buffer(
        buffer: &ratatui::buffer::Buffer,
        cursor: Option<CursorState>,
    ) -> Self {
        Self::from_ratatui_buffer_with_hyperlinks(buffer, cursor, &[])
    }

    pub fn from_ratatui_buffer_with_hyperlinks(
        buffer: &ratatui::buffer::Buffer,
        cursor: Option<CursorState>,
        hyperlinks: &[((u16, u16), String, String)],
    ) -> Self {
        let area = buffer.area;
        let width = area.width;
        let height = area.height;

        let mut hyperlink_uris = Vec::<String>::new();
        let mut hyperlink_indices = HashMap::<&str, u32>::new();
        let mut hyperlink_by_position = HashMap::<(u16, u16), (&str, &str)>::new();
        for ((x, y), symbol, uri) in hyperlinks {
            hyperlink_by_position.insert((*x, *y), (symbol.as_str(), uri.as_str()));
        }
        let mut cells = Vec::with_capacity((width as usize) * (height as usize));
        for row in 0..height {
            for col in 0..width {
                let cell = buffer.cell((col, row)).expect("cell within bounds");
                let hyperlink = hyperlink_by_position
                    .get(&(col, row))
                    .and_then(|(symbol, uri)| {
                        if *symbol != cell.symbol() {
                            return None;
                        }
                        Some(*hyperlink_indices.entry(*uri).or_insert_with(|| {
                            let index = hyperlink_uris.len() as u32;
                            hyperlink_uris.push((*uri).to_owned());
                            index
                        }))
                    });
                let mut cell = CellData::from_ratatui_cell(cell);
                cell.hyperlink = hyperlink;
                cells.push(cell);
            }
        }

        FrameData {
            cells,
            width,
            height,
            cursor,
            hyperlinks: hyperlink_uris,
            graphics: Vec::new(),
        }
    }

    pub(crate) fn replace_from_ratatui_buffer_preserving_effects(
        &mut self,
        buffer: &ratatui::buffer::Buffer,
        cursor: Option<CursorState>,
    ) {
        let width = self.width;
        let hyperlinks = if width == 0 {
            Vec::new()
        } else {
            self.cells
                .iter()
                .enumerate()
                .filter_map(|(index, cell)| {
                    let uri = self.hyperlinks.get(cell.hyperlink? as usize)?;
                    let x = u16::try_from(index % usize::from(width)).ok()?;
                    let y = u16::try_from(index / usize::from(width)).ok()?;
                    Some(((x, y), cell.symbol.clone(), uri.clone()))
                })
                .collect::<Vec<_>>()
        };
        let graphics = std::mem::take(&mut self.graphics);
        let mut replacement =
            Self::from_ratatui_buffer_with_hyperlinks(buffer, cursor, &hyperlinks);
        replacement.graphics = graphics;
        *self = replacement;
    }

    /// Reconstructs a ratatui `Buffer` from this frame data.
    ///
    /// Returns `None` if the cells vector length doesn't match `width * height`.
    pub(crate) fn to_ratatui_buffer(&self) -> Option<ratatui::buffer::Buffer> {
        let expected = (self.width as usize) * (self.height as usize);
        if self.cells.len() != expected {
            return None;
        }

        let area = ratatui::layout::Rect::new(0, 0, self.width, self.height);
        let mut buffer = ratatui::buffer::Buffer::filled(area, ratatui::buffer::Cell::new(" "));

        for row in 0..self.height {
            for col in 0..self.width {
                let idx = (row as usize) * (self.width as usize) + (col as usize);
                let cell_data = &self.cells[idx];
                let cell = buffer.cell_mut((col, row)).expect("cell within bounds");
                cell.set_symbol(&cell_data.symbol);
                cell.fg = u32_to_color(cell_data.fg);
                cell.bg = u32_to_color(cell_data.bg);
                cell.modifier = u16_to_modifier(cell_data.modifier);
                cell.skip = cell_data.skip;
            }
        }

        Some(buffer)
    }
}

fn deserialize_client_shell_agent_status<'de, D>(
    deserializer: D,
) -> Result<crate::api::schema::AgentStatus, D::Error>
where
    D: serde::Deserializer<'de>,
{
    if !deserializer.is_human_readable() {
        return crate::api::schema::AgentStatus::deserialize(deserializer);
    }
    let value = String::deserialize(deserializer)?;
    Ok(match value.as_str() {
        "idle" => crate::api::schema::AgentStatus::Idle,
        "working" => crate::api::schema::AgentStatus::Working,
        "blocked" => crate::api::schema::AgentStatus::Blocked,
        "done" => crate::api::schema::AgentStatus::Done,
        _ => crate::api::schema::AgentStatus::Unknown,
    })
}

/// Initial resource projection used by the stable client-owned shell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientShellSnapshot {
    /// Changes whenever the endpoint process restarts.
    pub boot_id: String,
    /// Monotonic replacement revision within one endpoint boot.
    pub revision: u64,
    /// Endpoint startup/reload config warning, filtered for client-owned keybindings.
    pub config_diagnostic: Option<String>,
    /// Unseen announcement owned and persisted by this endpoint.
    pub product_announcement: Option<ClientShellProductAnnouncement>,
    /// Future-version update advertised by the endpoint.
    pub update_available: Option<String>,
    /// Endpoint-specific command shown in update instructions.
    pub update_install_command: String,
    /// Endpoint's normalized built-in keybindings, used only when a remote client selects server bindings.
    pub server_keybindings_toml: Option<String>,
    /// Whether the endpoint has a What's New entry, even if its body is unavailable.
    pub latest_release_notes_available: bool,
    /// Whether endpoint-owned integration assets need an update.
    pub integration_updates_available: bool,
    /// Endpoint-owned base directory used for new linked worktree checkouts.
    pub worktree_directory: String,
    /// Cached endpoint-owned notes used by the client-rendered overlay.
    pub release_notes: Option<ClientShellReleaseNotes>,
    pub focused_workspace_id: Option<String>,
    pub focused_tab_id: Option<String>,
    pub focused_pane_id: Option<String>,
    pub tab_bar_right: Vec<ClientShellTabStatusSegment>,
    pub tab_bar_right_separator: String,
    pub agent_view_label: Option<String>,
    pub agent_order: Vec<String>,
    pub workspaces: Vec<ClientShellWorkspace>,
    pub tabs: Vec<ClientShellTab>,
    pub panes: Vec<ClientShellPane>,
    pub agents: Vec<ClientShellAgent>,
    pub commands: Vec<ClientShellCommand>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientShellProductAnnouncement {
    pub version: String,
    pub id: String,
    pub title: String,
    pub body: String,
    pub preview: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientShellReleaseNotes {
    pub version: String,
    pub body: String,
    pub preview: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientShellCommandAction {
    Shell,
    Pane,
    Popup,
    PluginAction,
    /// A future endpoint action kind that this client cannot execute.
    #[serde(other)]
    Unknown,
}

impl From<crate::config::CustomCommandAction> for ClientShellCommandAction {
    fn from(action: crate::config::CustomCommandAction) -> Self {
        match action {
            crate::config::CustomCommandAction::Shell => Self::Shell,
            crate::config::CustomCommandAction::Pane => Self::Pane,
            crate::config::CustomCommandAction::Popup => Self::Popup,
            crate::config::CustomCommandAction::PluginAction => Self::PluginAction,
        }
    }
}

impl TryFrom<ClientShellCommandAction> for crate::config::CustomCommandAction {
    type Error = ();

    fn try_from(action: ClientShellCommandAction) -> Result<Self, Self::Error> {
        match action {
            ClientShellCommandAction::Shell => Ok(Self::Shell),
            ClientShellCommandAction::Pane => Ok(Self::Pane),
            ClientShellCommandAction::Popup => Ok(Self::Popup),
            ClientShellCommandAction::PluginAction => Ok(Self::PluginAction),
            ClientShellCommandAction::Unknown => Err(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientShellCommand {
    pub command_id: String,
    pub binding_label: String,
    pub binding_labels: Vec<String>,
    pub action: ClientShellCommandAction,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientShellTabStatusSegment {
    pub text: String,
    pub accent: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientShellWorkspace {
    pub workspace_id: String,
    pub active_tab_id: String,
    pub new_workspace_cwd: String,
    pub number: usize,
    pub label: String,
    pub custom_label: bool,
    pub branch: Option<String>,
    pub git_ahead_behind: Option<(usize, usize)>,
    pub tokens: Vec<(String, String)>,
    pub worktree: Option<ClientShellWorktree>,
    pub focused: bool,
    #[serde(deserialize_with = "deserialize_client_shell_agent_status")]
    pub agent_status: crate::api::schema::AgentStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientShellWorktree {
    pub key: String,
    pub label: String,
    pub is_linked_worktree: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientShellTab {
    pub tab_id: String,
    pub workspace_id: String,
    pub number: usize,
    pub label: String,
    pub custom_label: bool,
    pub zoomed: bool,
    pub focused: bool,
    #[serde(deserialize_with = "deserialize_client_shell_agent_status")]
    pub agent_status: crate::api::schema::AgentStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientShellPane {
    pub pane_id: String,
    pub workspace_id: String,
    pub tab_id: String,
    pub label: Option<String>,
    pub cwd: Option<String>,
    pub foreground_cwd: Option<String>,
    pub focused: bool,
    pub right_click_passthrough: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientShellAgent {
    pub pane_id: String,
    pub workspace_id: String,
    pub tab_id: String,
    pub name: Option<String>,
    pub display_agent: Option<String>,
    pub agent: Option<String>,
    pub title: Option<String>,
    pub terminal_title: Option<String>,
    pub terminal_title_stripped: Option<String>,
    #[serde(deserialize_with = "deserialize_client_shell_agent_status")]
    pub agent_status: crate::api::schema::AgentStatus,
    pub state_change_seq: u64,
    pub state_labels: Vec<(String, String)>,
    pub tokens: Vec<(String, String)>,
    pub focused: bool,
}

/// Origin-relative geometry for one pane in a rendered pane surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, bincode::Decode)]
pub struct PaneSurfacePane {
    pub pane_id: String,
    pub content_revision: u64,
    pub rect: SurfaceRect,
    pub inner_rect: SurfaceRect,
    pub scrollbar_rect: Option<SurfaceRect>,
    pub scroll: Option<PaneSurfaceScrollMetrics>,
    pub focused: bool,
    pub mouse_reporting: bool,
    pub sgr_pixel_mouse: bool,
    pub alternate_screen_active: bool,
    pub pixel_width: u32,
    pub pixel_height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, bincode::Decode)]
pub struct PaneSurfaceScrollMetrics {
    pub offset_from_bottom: u64,
    pub max_offset_from_bottom: u64,
    pub viewport_rows: u64,
}

/// One draggable BSP split handle relative to a pane surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSurfaceSplit {
    pub direction: PaneSurfaceSplitDirection,
    pub pos: u16,
    pub area: SurfaceRect,
    pub hit_rect: SurfaceRect,
    pub path: Vec<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, bincode::Decode)]
pub enum PaneSurfaceSplitDirection {
    Horizontal,
    Vertical,
}

/// Wire-safe rectangle relative to a pane surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, bincode::Decode)]
pub struct SurfaceRect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl From<ratatui::layout::Rect> for SurfaceRect {
    fn from(rect: ratatui::layout::Rect) -> Self {
        Self {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
        }
    }
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize, bincode::Decode)]
pub enum SurfaceGraphicsTarget {
    Pane { pane_id: String },
    Popup { terminal_id: String },
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize, bincode::Decode)]
pub enum SurfaceGraphicsSource {
    Terminal {
        target: SurfaceGraphicsTarget,
        image_id: u32,
    },
    PaneLayer {
        pane_id: String,
        layer_id: String,
    },
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, Serialize, Deserialize, bincode::Decode)]
pub enum SurfaceGraphicsFormat {
    Rgb,
    Rgba,
    Png,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize, bincode::Decode)]
pub struct SurfaceGraphicsAssetKey {
    pub source: SurfaceGraphicsSource,
    pub image_width: u32,
    pub image_height: u32,
    pub format: SurfaceGraphicsFormat,
    pub data_len: u64,
    pub data_fingerprint: u64,
}

/// Image bytes newly needed by this connection's complete desired scene.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceGraphicsAsset {
    pub key: SurfaceGraphicsAssetKey,
    #[serde(
        serialize_with = "serialize_graphics_bytes",
        deserialize_with = "deserialize_graphics_bytes"
    )]
    pub data: Vec<u8>,
}

fn serialize_graphics_bytes<S: serde::Serializer>(
    data: &[u8],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    // Bincode's byte slice has the same length+bytes layout as Vec<u8>,
    // but avoids per-byte serialization. Keep human-readable codecs unchanged.
    if serializer.is_human_readable() {
        data.serialize(serializer)
    } else {
        serializer.serialize_bytes(data)
    }
}

fn deserialize_graphics_bytes<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<u8>, D::Error> {
    if deserializer.is_human_readable() {
        return Vec::<u8>::deserialize(deserializer);
    }

    struct BytesVisitor;
    impl<'de> serde::de::Visitor<'de> for BytesVisitor {
        type Value = Vec<u8>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("image bytes")
        }

        fn visit_bytes<E: serde::de::Error>(self, bytes: &[u8]) -> Result<Self::Value, E> {
            Ok(bytes.to_vec())
        }

        fn visit_byte_buf<E: serde::de::Error>(self, bytes: Vec<u8>) -> Result<Self::Value, E> {
            Ok(bytes)
        }
    }

    // The framed slice decoder checks the length against available input before
    // handing bytes to the visitor, so a forged length cannot cause an allocation.
    deserializer.deserialize_bytes(BytesVisitor)
}

/// One already-clipped desired placement relative to its target surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, bincode::Decode)]
pub struct SurfaceGraphicsPlacement {
    pub asset: SurfaceGraphicsAssetKey,
    pub logical_placement_id: u32,
    pub x: u16,
    pub y: u16,
    pub cols: u32,
    pub rows: u32,
    pub source_x: u32,
    pub source_y: u32,
    pub source_width: u32,
    pub source_height: u32,
    pub x_offset: u32,
    pub y_offset: u32,
    pub z: i32,
    pub scrollback_offset: u32,
}

/// Complete desired placements plus only the image bytes not already sent for
/// the current live scene on this connection.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceGraphicsScene {
    pub assets: Vec<SurfaceGraphicsAsset>,
    pub placements: Vec<SurfaceGraphicsPlacement>,
    /// Direct-uploaded assets that remain live for this client even while their
    /// pane is outside the selected scene.
    pub retained_assets: Vec<SurfaceGraphicsAssetKey>,
}

/// One server-rendered active-tab surface without sidebar, tab bar, or overlays.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSurfaceFrame {
    /// Endpoint process identity that produced this surface.
    pub boot_id: String,
    /// Projection revision whose focused IDs and topology produced this surface.
    pub projection_revision: u64,
    /// Monotonic revision for full surfaces and incremental patches on one connection.
    pub surface_revision: u64,
    pub frame: FrameData,
    pub panes: Vec<PaneSurfacePane>,
    pub splits: Vec<PaneSurfaceSplit>,
    pub popup: Option<Box<ClientShellPopupSurface>>,
    pub graphics: SurfaceGraphicsScene,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, bincode::Decode)]
pub enum ClientShellPopupSize {
    Cells(u16),
    Percent(u8),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSurfacePatchRow {
    /// Origin-relative surface column where this changed cell span starts.
    pub x: u16,
    /// Origin-relative surface row.
    pub y: u16,
    pub cells: Vec<CellData>,
}

/// Incremental terminal-cell update against one committed complete pane surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSurfacePatch {
    pub boot_id: String,
    pub projection_revision: u64,
    pub base_surface_revision: u64,
    pub surface_revision: u64,
    pub rows: Vec<PaneSurfacePatchRow>,
    /// Updated metadata for panes whose terminal content changed.
    pub panes: Vec<PaneSurfacePane>,
    /// Final cursor relative to the pane surface.
    pub cursor: Option<CursorState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientShellPopupSurface {
    pub terminal_id: String,
    pub title: String,
    pub width: Option<ClientShellPopupSize>,
    pub height: Option<ClientShellPopupSize>,
    pub frame: FrameData,
    pub mouse_reporting: bool,
    pub sgr_pixel_mouse: bool,
    pub pixel_width: u32,
    pub pixel_height: u32,
}

/// Terminal ANSI bytes encoded by the server for network-efficient clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalFrame {
    /// Monotonic per-client frame sequence.
    pub seq: u64,
    /// Frame width in columns.
    pub width: u16,
    /// Frame height in rows.
    pub height: u16,
    /// Whether bytes contain a full redraw rather than an incremental diff.
    pub full: bool,
    /// Terminal escape bytes ready to write directly to stdout.
    pub bytes: Vec<u8>,
}

/// Notification kind forwarded from server to client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NotifyKind {
    /// Play a sound (bell/agent-done, etc.).
    Sound,
    /// Display a toast message through the outer terminal.
    Toast,
    /// Display a toast message through the host OS notification service.
    SystemToast,
}

/// A client-rendered notification category. The server reports the semantic
/// event; each connected shell client chooses how (or whether) to present it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SemanticNotificationKind {
    NeedsAttention,
    Finished,
    UpdateInstalled,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SemanticNotificationSound {
    Done,
    Request,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticNotification {
    pub kind: SemanticNotificationKind,
    pub title: String,
    pub body: Option<String>,
    pub sound: Option<SemanticNotificationSound>,
    pub agent: Option<String>,
    pub workspace_id: Option<String>,
    pub tab_id: Option<String>,
    pub pane_id: Option<String>,
    pub position: Option<crate::config::ToastHerdrPosition>,
}

/// Messages sent from the server to the client over the client protocol socket.
///
/// Variant order is frozen for endpoint generation 1. Add compatible endpoint
/// behavior through `EndpointControl`, and ignore unrecognized named controls.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerMessage {
    /// Handshake response: server acknowledges (or rejects) the client.
    Welcome {
        /// Protocol version the server speaks.
        version: u32,
        /// Render encoding selected by the server for this connection.
        encoding: RenderEncoding,
        /// If present, the handshake failed and this describes why.
        /// The client should exit with a clear error message.
        error: Option<String>,
    },

    /// Terminal bytes to write directly for a terminal-ANSI client.
    Terminal(TerminalFrame),

    /// Client-local Kitty graphics bytes to write directly to the host terminal.
    Graphics {
        /// Raw Kitty graphics protocol bytes.
        bytes: Vec<u8>,
    },

    /// Server is shutting down. Clients should exit gracefully.
    ServerShutdown {
        /// Optional reason for the shutdown.
        reason: Option<String>,
    },

    /// A notification event (sound/toast) to be rendered locally by the client.
    Notify {
        /// What kind of notification.
        kind: NotifyKind,
        /// Human-readable title or sound label.
        message: String,
        /// Optional human-readable notification body.
        body: Option<String>,
    },

    /// OSC 52 clipboard data forwarded from a PTY through the server.
    Clipboard {
        /// Base64-encoded clipboard data.
        data: String,
    },

    /// Set the foreground client's outer terminal window title.
    WindowTitle {
        /// Sanitized title to write with OSC 0. `None` restores Herdr's default title.
        title: Option<String>,
    },

    /// Client-local runtime config changed on disk; refresh it without reconnecting.
    ReloadSoundConfig,

    /// Whether the client should currently capture host mouse input.
    MouseCapture {
        /// True when Herdr mouse UI is enabled or the focused pane app requests mouse reporting.
        enabled: bool,
        /// True only while the focused pane requests DEC SGR pixel mode 1016.
        sgr_pixels: bool,
    },

    /// Ring the foreground client's outer terminal for pane-originated BEL characters.
    TerminalBell {
        /// Number of BEL characters parsed from one PTY read.
        count: u16,
    },

    /// One validated Herdr-owned Kitty regular-file RGBA transmission.
    GraphicsFile {
        path: String,
        expected_len: u64,
        image_id: u32,
        transfer_id: u64,
        leading: Vec<u8>,
        control: String,
        /// ClientShell upload identity. `None` targets a direct terminal client.
        surface_asset: Option<SurfaceGraphicsAssetKey>,
    },

    /// Suppress a direct command that expired before terminal delivery.
    GraphicsTransmissionRetired { transfer_id: u64, image_id: u32 },

    /// Initial metadata for a client-owned shell.
    ClientShellSnapshot(Box<ClientShellSnapshot>),

    /// Active-tab pane content rendered at a client-requested origin-relative size.
    PaneSurface(PaneSurfaceFrame),

    /// Ephemeral semantic notification delivered over the private control lane.
    /// It is sent only to currently connected client-rendered shells.
    SemanticNotification(SemanticNotification),

    /// Immediate endpoint error that the client-rendered shell must show regardless of notification policy.
    ClientShellError { message: String },

    /// Exact Kitty keyboard flags requested by a directly attached terminal.
    /// Zero restores the host terminal's previous keyboard mode.
    DirectTerminalKeyboardProtocol {
        flags: u16,
        modify_other_keys_level: u8,
    },

    /// Whether the focused pane or popup needs the shell host to report every key.
    ClientShellKeyboardReportAll { enabled: bool },

    /// One ordered chunk of the final response to an endpoint operation.
    ClientShellEndpointResponseChunk {
        boot_id: String,
        request_id: String,
        final_chunk: bool,
        data: Vec<u8>,
    },

    /// Incremental terminal-cell update for a previously committed pane surface.
    PaneSurfacePatch(PaneSurfacePatch),

    /// Extensible named control message for the stable client-owned endpoint protocol.
    ///
    /// This variant is append-only. Its bincode tag and two-string payload are part
    /// of endpoint generation 1 and must not change.
    EndpointControl { kind: String, data: String },
}

// ---------------------------------------------------------------------------
// Color / Modifier conversion helpers
// ---------------------------------------------------------------------------

/// Converts a ratatui `Color` to a packed u32 for wire transport.
///
/// Encoding:
/// - Named colors (Reset, Black, …, White) → `0x00_00_00_XX` where XX is 0..=16
/// - Indexed palette → `0x01_00_00_XX` where XX is the palette index
/// - RGB → `0x02_RR_GG_BB` with components in the lower 3 bytes
pub(crate) fn color_to_u32(color: ratatui::style::Color) -> u32 {
    match color {
        ratatui::style::Color::Reset => 0x00_00_00_00,
        ratatui::style::Color::Black => 0x00_00_00_01,
        ratatui::style::Color::Red => 0x00_00_00_02,
        ratatui::style::Color::Green => 0x00_00_00_03,
        ratatui::style::Color::Yellow => 0x00_00_00_04,
        ratatui::style::Color::Blue => 0x00_00_00_05,
        ratatui::style::Color::Magenta => 0x00_00_00_06,
        ratatui::style::Color::Cyan => 0x00_00_00_07,
        ratatui::style::Color::Gray => 0x00_00_00_08,
        ratatui::style::Color::DarkGray => 0x00_00_00_09,
        ratatui::style::Color::LightRed => 0x00_00_00_0A,
        ratatui::style::Color::LightGreen => 0x00_00_00_0B,
        ratatui::style::Color::LightYellow => 0x00_00_00_0C,
        ratatui::style::Color::LightBlue => 0x00_00_00_0D,
        ratatui::style::Color::LightMagenta => 0x00_00_00_0E,
        ratatui::style::Color::LightCyan => 0x00_00_00_0F,
        ratatui::style::Color::White => 0x00_00_00_10,
        ratatui::style::Color::Indexed(i) => 0x01_00_00_00 | (i as u32),
        ratatui::style::Color::Rgb(r, g, b) => {
            0x02_00_00_00 | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
        }
    }
}

/// Converts a packed u32 back to a ratatui `Color`.
fn u32_to_color(val: u32) -> ratatui::style::Color {
    match val >> 24 {
        0x00 => match val & 0xFF {
            0x00 => ratatui::style::Color::Reset,
            0x01 => ratatui::style::Color::Black,
            0x02 => ratatui::style::Color::Red,
            0x03 => ratatui::style::Color::Green,
            0x04 => ratatui::style::Color::Yellow,
            0x05 => ratatui::style::Color::Blue,
            0x06 => ratatui::style::Color::Magenta,
            0x07 => ratatui::style::Color::Cyan,
            0x08 => ratatui::style::Color::Gray,
            0x09 => ratatui::style::Color::DarkGray,
            0x0A => ratatui::style::Color::LightRed,
            0x0B => ratatui::style::Color::LightGreen,
            0x0C => ratatui::style::Color::LightYellow,
            0x0D => ratatui::style::Color::LightBlue,
            0x0E => ratatui::style::Color::LightMagenta,
            0x0F => ratatui::style::Color::LightCyan,
            0x10 => ratatui::style::Color::White,
            _ => ratatui::style::Color::Reset, // unknown named → Reset
        },
        0x01 => ratatui::style::Color::Indexed((val & 0xFF) as u8),
        0x02 => {
            let r = ((val >> 16) & 0xFF) as u8;
            let g = ((val >> 8) & 0xFF) as u8;
            let b = (val & 0xFF) as u8;
            ratatui::style::Color::Rgb(r, g, b)
        }
        _ => ratatui::style::Color::Reset, // unknown tag → Reset
    }
}

const UNDERLINE_STYLE_SHIFT: u16 = 12;
const UNDERLINE_STYLE_MASK: u16 = 0xF000;

/// Converts a ratatui `Modifier` bitmask to a u16 for wire transport.
pub(crate) fn modifier_to_u16(modifier: ratatui::style::Modifier) -> u16 {
    modifier.bits()
}

pub(crate) fn underline_style_from_modifier(modifier: u16) -> u8 {
    ((modifier & UNDERLINE_STYLE_MASK) >> UNDERLINE_STYLE_SHIFT) as u8
}

pub(crate) fn modifier_with_underline_style(
    modifier: ratatui::style::Modifier,
    underline_style: u8,
) -> ratatui::style::Modifier {
    let bits = modifier.bits() | ((u16::from(underline_style) & 0x0F) << UNDERLINE_STYLE_SHIFT);
    ratatui::style::Modifier::from_bits_retain(bits)
}

/// Converts a u16 back to a ratatui `Modifier`.
fn u16_to_modifier(val: u16) -> ratatui::style::Modifier {
    ratatui::style::Modifier::from_bits_truncate(val & !UNDERLINE_STYLE_MASK)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{read_message, write_message};
    use ratatui::style::{Color, Modifier};
    use sha2::{Digest, Sha256};
    fn encoded_sha256(value: &impl Serialize) -> String {
        let encoded = bincode::serde::encode_to_vec(value, bincode::config::standard()).unwrap();
        format!("{:x}", Sha256::digest(encoded))
    }

    #[test]
    fn first_frame_pins_the_abi_without_retrying_malformed_controls() {
        use std::io::Cursor;
        let json = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/upstream-gen1-endpoint-hello-v1.json"
        ));
        let message = ClientMessage::EndpointControl {
            kind: super::super::endpoint::ENDPOINT_HELLO_KIND.into(),
            data: json.into(),
        };
        let mut bytes = Vec::new();
        write_message(&mut bytes, &message).expect("encode upstream-compatible hello");
        let InitialHello::Endpoint(hello) =
            read_initial_hello(&mut Cursor::new(bytes)).expect("endpoint")
        else {
            panic!("must choose stable endpoint codec");
        };
        assert!(hello.supports_required_codecs());
        assert_eq!(hello.min_generation, 1);
        let private = super::super::ClientMessage::Hello {
            version: super::super::PROTOCOL_VERSION,
            cols: 80,
            rows: 24,
            cell_width_px: 0,
            cell_height_px: 0,
            requested_encoding: super::super::RenderEncoding::SemanticFrame,
            keybindings: super::super::ClientKeybindings::Server,
            launch_mode: super::super::ClientLaunchMode::App,
        };
        let mut bytes = Vec::new();
        write_message(&mut bytes, &private).expect("encode unchanged fork hello");
        let InitialHello::Private(decoded) =
            read_initial_hello(&mut Cursor::new(bytes)).expect("private")
        else {
            panic!("must preserve private wire codec");
        };
        assert_eq!(decoded, private);
        for message in [
            ClientMessage::EndpointControl {
                kind: "future.optional".into(),
                data: json.into(),
            },
            ClientMessage::EndpointControl {
                kind: super::super::endpoint::ENDPOINT_HELLO_KIND.into(),
                data: "invalid-json".into(),
            },
            ClientMessage::Detach,
        ] {
            let mut bytes = Vec::new();
            write_message(&mut bytes, &message).expect("malformed handshake envelope");
            assert!(read_initial_hello(&mut Cursor::new(bytes)).is_err());
        }
    }

    #[test]
    fn graphics_bulk_codec_preserves_legacy_wire_and_json() {
        #[derive(Serialize, Deserialize)]
        struct LegacyAsset {
            key: SurfaceGraphicsAssetKey,
            data: Vec<u8>,
        }
        for len in [0, 1, 250, 251, 65535, 65536, 800 * 480 * 4] {
            let asset = SurfaceGraphicsAsset {
                key: SurfaceGraphicsAssetKey {
                    source: SurfaceGraphicsSource::Terminal {
                        target: SurfaceGraphicsTarget::Pane {
                            pane_id: "pane-1".into(),
                        },
                        image_id: 7,
                    },
                    image_width: 800,
                    image_height: 480,
                    format: SurfaceGraphicsFormat::Rgba,
                    data_len: len as u64,
                    data_fingerprint: 42,
                },
                data: (0..len).map(|i| (i % 256) as u8).collect(),
            };
            let legacy = LegacyAsset {
                key: asset.key.clone(),
                data: asset.data.clone(),
            };
            let config = bincode::config::standard();
            let before = bincode::serde::encode_to_vec(&legacy, config).unwrap();
            let after = bincode::serde::encode_to_vec(&asset, config).unwrap();
            assert_eq!(after, before);
            let (decoded, used): (SurfaceGraphicsAsset, _) =
                bincode::serde::decode_from_slice(&before, config).unwrap();
            assert_eq!(decoded, asset);
            assert_eq!(used, before.len());
            let (decoded, used): (LegacyAsset, _) =
                bincode::serde::decode_from_slice(&after, config).unwrap();
            assert_eq!(decoded.data, asset.data);
            assert_eq!(decoded.key, asset.key);
            assert_eq!(used, after.len());
            let json = serde_json::to_value(&legacy).unwrap();
            assert_eq!(serde_json::to_value(&asset).unwrap(), json);
            assert_eq!(
                serde_json::from_value::<SurfaceGraphicsAsset>(json).unwrap(),
                asset
            );
            assert!(
                bincode::serde::decode_from_slice::<SurfaceGraphicsAsset, _>(
                    &before[..before.len() - 1],
                    config
                )
                .is_err()
            );
        }
    }

    #[test]
    fn graphics_bulk_decode_rejects_truncated_and_forged_lengths() {
        #[derive(Debug, Deserialize)]
        struct Bytes(#[serde(deserialize_with = "deserialize_graphics_bytes")] Vec<u8>);
        let config = bincode::config::standard();
        let original: Vec<u8> = (0..=255).collect();
        let encoded = bincode::serde::encode_to_vec(&original, config).unwrap();
        for end in 0..encoded.len() {
            assert!(
                bincode::serde::decode_from_slice::<Bytes, _>(&encoded[..end], config).is_err()
            );
        }
        let (decoded, consumed): (Bytes, _) =
            bincode::serde::decode_from_slice(&encoded, config).unwrap();
        assert_eq!(decoded.0, original);
        assert_eq!(consumed, encoded.len());
        for length in [MAX_GRAPHICS_FRAME_SIZE as u64 + 1, u64::MAX] {
            let forged = bincode::serde::encode_to_vec(length, config).unwrap();
            assert!(bincode::serde::decode_from_slice::<Bytes, _>(&forged, config).is_err());
        }
    }

    #[test]
    fn endpoint_control_roundtrip() {
        let msg = ClientMessage::EndpointControl {
            kind: "endpoint.hello.v1".into(),
            data: r#"{"generation":1}"#.into(),
        };
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ClientMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
        assert_eq!(
            bincode::serde::encode_to_vec(
                ClientMessage::EndpointControl {
                    kind: String::new(),
                    data: String::new(),
                },
                bincode::config::standard(),
            )
            .unwrap(),
            [20, 0, 0]
        );
    }

    #[test]
    fn client_shell_resize_roundtrip() {
        let msg = ClientMessage::ClientShellResize {
            cell_width_px: 8,
            cell_height_px: 16,
            surface_size: ClientSurfaceSize { cols: 74, rows: 29 },
            pixel_mouse: true,
        };
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ClientMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
        assert_eq!(
            encoded_sha256(&msg),
            "676d6376202750e72c45ff511e256b6154d3d20c0ee088d3792fa3a69d9704b9"
        );
    }

    #[test]
    fn client_message_wire_tags_reflect_current_order() {
        fn tag(msg: &ClientMessage) -> u8 {
            *bincode::serde::encode_to_vec(msg, bincode::config::standard())
                .unwrap()
                .first()
                .expect("encoded client message should include enum tag")
        }

        assert_eq!(
            tag(&ClientMessage::TerminalHello {
                version: super::super::PROTOCOL_VERSION,
                cols: 80,
                rows: 24,
                cell_width_px: 8,
                cell_height_px: 16,
                pixel_mouse: false,
            }),
            0
        );
        assert_eq!(
            tag(&ClientMessage::ClientShellHello {
                version: super::super::PROTOCOL_VERSION,
                cell_width_px: 8,
                cell_height_px: 16,
                surface_size: ClientSurfaceSize { cols: 80, rows: 29 },
                pixel_mouse: false,
                direct_graphics: false,
                endpoint_keybindings: false,
                mouse_capture: false,
            }),
            11
        );
        assert_eq!(tag(&ClientMessage::Input { data: Vec::new() }), 1);
        assert_eq!(
            tag(&ClientMessage::ClipboardImage {
                target: ClientClipboardImageTarget::DirectTerminal,
                extension: "png".to_owned(),
                data: Vec::new(),
            }),
            2
        );
        assert_eq!(
            tag(&ClientMessage::Resize {
                cols: 80,
                rows: 24,
                cell_width_px: 8,
                cell_height_px: 16,
                pixel_mouse: false,
            }),
            3
        );
        assert_eq!(tag(&ClientMessage::Detach), 4);
        assert_eq!(
            tag(&ClientMessage::AttachTerminal {
                terminal_id: "term".to_owned(),
                takeover: false,
            }),
            5
        );
        assert_eq!(
            tag(&ClientMessage::AttachScroll {
                source: AttachScrollSource::Wheel,
                direction: AttachScrollDirection::Up,
                lines: 1,
                column: None,
                row: None,
                modifiers: 0,
            }),
            6
        );
        assert_eq!(
            tag(&ClientMessage::ObserveTerminal {
                target: "w1:p1".to_owned(),
            }),
            7
        );
        assert_eq!(
            tag(&ClientMessage::ControlTerminal {
                target: "w1:p1".to_owned(),
                takeover: false,
            }),
            8
        );
        assert_eq!(
            tag(&ClientMessage::GraphicsTransmissionResult {
                transfer_id: 1,
                image_id: 2,
                success: true,
            }),
            9
        );
        assert_eq!(
            tag(&ClientMessage::GraphicsTransmissionStarted {
                transfer_id: 1,
                image_id: 2,
            }),
            10
        );
        assert_eq!(
            tag(&ClientMessage::ClientShellResize {
                cell_width_px: 8,
                cell_height_px: 16,
                surface_size: ClientSurfaceSize { cols: 80, rows: 29 },
                pixel_mouse: false,
            }),
            12
        );
        assert_eq!(
            tag(&ClientMessage::ClientShellPaneInput {
                pane_id: "pane".into(),
                events: Vec::new(),
            }),
            13
        );
        assert_eq!(
            tag(&ClientMessage::ClientShellPopupInput {
                terminal_id: "popup".into(),
                events: Vec::new(),
            }),
            14
        );
        assert_eq!(
            tag(&ClientMessage::ClientShellEndpointRequest {
                boot_id: "boot".into(),
                request: "{}".into(),
            }),
            15
        );
        assert_eq!(
            tag(&ClientMessage::AttachMouse {
                kind: ClientMouseKind::Down(ClientMouseButton::Left),
                position: ClientMousePosition::Cell { column: 10, row: 5 },
                geometry: None,
                modifiers: 0,
                lines: 1,
            }),
            16
        );
        assert_eq!(
            tag(&ClientMessage::ClientShellHostTheme {
                update: ClientHostThemeUpdate::Appearance(ClientHostAppearance::Dark),
            }),
            17
        );
        assert_eq!(tag(&ClientMessage::ClientShellFocus { focused: true }), 18);
        assert_eq!(
            tag(&ClientMessage::ClientShellMouseCapture { enabled: true }),
            19
        );
        assert_eq!(
            tag(&ClientMessage::EndpointControl {
                kind: String::new(),
                data: String::new(),
            }),
            20
        );
    }

    #[test]
    fn client_shell_focus_roundtrip() {
        let message = ClientMessage::ClientShellFocus { focused: false };
        let encoded = bincode::serde::encode_to_vec(&message, bincode::config::standard())
            .expect("encode client shell focus");
        let (decoded, _): (ClientMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard())
                .expect("decode client shell focus");
        assert_eq!(decoded, message);
    }

    #[test]
    fn client_shell_mouse_capture_roundtrip() {
        let message = ClientMessage::ClientShellMouseCapture { enabled: false };
        let encoded = bincode::serde::encode_to_vec(&message, bincode::config::standard())
            .expect("encode client shell mouse capture");
        let (decoded, _): (ClientMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard())
                .expect("decode client shell mouse capture");
        assert_eq!(decoded, message);
    }

    #[test]
    fn client_shell_host_theme_roundtrip() {
        let message = ClientMessage::ClientShellHostTheme {
            update: ClientHostThemeUpdate::PaletteColors(vec![(
                4,
                ClientHostColor {
                    r: 10,
                    g: 20,
                    b: 30,
                },
            )]),
        };
        let encoded = bincode::serde::encode_to_vec(&message, bincode::config::standard())
            .expect("encode host theme update");
        let (decoded, _): (ClientMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard())
                .expect("decode host theme update");
        assert_eq!(decoded, message);
    }

    #[test]
    fn client_shell_endpoint_messages_roundtrip() {
        let request = ClientMessage::ClientShellEndpointRequest {
            boot_id: "boot-a".into(),
            request: r#"{"id":"request-a","method":"session.snapshot","params":{}}"#.into(),
        };
        let encoded = bincode::serde::encode_to_vec(&request, bincode::config::standard())
            .expect("encode endpoint request");
        let (decoded, _): (ClientMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard())
                .expect("decode endpoint request");
        assert_eq!(decoded, request);
        assert_eq!(
            encoded_sha256(&request),
            "de5693585a01f6b0d5ee07c51b6ddf79ee9f67dbf183255822d31f35210f5ffb"
        );

        let response = ServerMessage::ClientShellEndpointResponseChunk {
            boot_id: "boot-a".into(),
            request_id: "request-a".into(),
            final_chunk: true,
            data: br#"{"id":"request-a","result":{"type":"ok"}}"#.to_vec(),
        };
        let encoded = bincode::serde::encode_to_vec(&response, bincode::config::standard())
            .expect("encode endpoint response");
        let (decoded, _): (ServerMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard())
                .expect("decode endpoint response");
        assert_eq!(decoded, response);
        assert_eq!(
            encoded_sha256(&response),
            "bc14dbb5263d3097fe6d3e70a4b6d71aa9c2fa4ae3206d692a7182512bffdd1d"
        );
    }

    #[test]
    fn client_clipboard_image_roundtrip() {
        let msg = ClientMessage::ClipboardImage {
            target: ClientClipboardImageTarget::Pane("w1:p1".into()),
            extension: "png".to_owned(),
            data: vec![0x89, b'P', b'N', b'G'],
        };
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ClientMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
        assert_eq!(
            encoded_sha256(&msg),
            "1c02110be0671faf318b3f4d8f507748d5a0a98cd474832669c5290d97ef19ab"
        );
    }

    #[test]
    fn server_frame_roundtrip_nontrivial() {
        // Build a 3×2 frame with varied styles (≥2×2).
        let frame = FrameData {
            cells: vec![
                CellData {
                    symbol: "H".into(),
                    fg: color_to_u32(Color::Red),
                    bg: color_to_u32(Color::Black),
                    modifier: Modifier::BOLD.bits(),
                    skip: false,
                    hyperlink: None,
                },
                CellData {
                    symbol: "i".into(),
                    fg: color_to_u32(Color::Green),
                    bg: color_to_u32(Color::Reset),
                    modifier: Modifier::ITALIC.bits(),
                    skip: false,
                    hyperlink: None,
                },
                CellData {
                    symbol: "!".into(),
                    fg: color_to_u32(Color::Rgb(255, 128, 0)),
                    bg: color_to_u32(Color::Indexed(220)),
                    modifier: (Modifier::BOLD | Modifier::UNDERLINED).bits(),
                    skip: false,
                    hyperlink: Some(0),
                },
                CellData {
                    symbol: " ".into(),
                    fg: color_to_u32(Color::Reset),
                    bg: color_to_u32(Color::Reset),
                    modifier: Modifier::empty().bits(),
                    skip: true,
                    hyperlink: None,
                },
                CellData {
                    symbol: "→".into(), // multi-byte grapheme
                    fg: color_to_u32(Color::Cyan),
                    bg: color_to_u32(Color::Blue),
                    modifier: Modifier::REVERSED.bits(),
                    skip: false,
                    hyperlink: None,
                },
                CellData {
                    symbol: "🦀".into(), // emoji, wide grapheme cluster
                    fg: color_to_u32(Color::Yellow),
                    bg: color_to_u32(Color::Magenta),
                    modifier: Modifier::empty().bits(),
                    skip: false,
                    hyperlink: None,
                },
            ],
            width: 3,
            height: 2,
            cursor: Some(CursorState {
                x: 0,
                y: 0,
                visible: true,
                shape: 6,
            }),
            hyperlinks: vec!["https://example.com".to_owned()],
            graphics: Vec::new(),
        };
        let msg = ServerMessage::PaneSurface(PaneSurfaceFrame {
            boot_id: "boot-1".into(),
            projection_revision: 1,
            surface_revision: 1,
            frame: frame.clone(),
            panes: Vec::new(),
            splits: Vec::new(),
            popup: None,
            graphics: SurfaceGraphicsScene::default(),
        });
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ServerMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
        assert_eq!(
            encoded_sha256(&msg),
            "7c016f7b21ddb5ac79212cf65a968b93eb292b5305b941263e89ffaa40158ee3"
        );
        match decoded {
            ServerMessage::PaneSurface(surface) => {
                assert_eq!(surface.frame.cells[2].hyperlink, Some(0));
                assert_eq!(
                    surface.frame.hyperlinks,
                    vec!["https://example.com".to_owned()]
                );
            }
            other => panic!("expected pane surface, got {other:?}"),
        }
    }

    #[test]
    fn pane_surface_patch_roundtrip() {
        let msg = ServerMessage::PaneSurfacePatch(PaneSurfacePatch {
            boot_id: "boot-1".into(),
            projection_revision: 3,
            base_surface_revision: 7,
            surface_revision: 8,
            rows: vec![PaneSurfacePatchRow {
                x: 2,
                y: 4,
                cells: vec![CellData {
                    symbol: "x".into(),
                    fg: 1,
                    bg: 2,
                    modifier: 3,
                    skip: false,
                    hyperlink: None,
                }],
            }],
            panes: Vec::new(),
            cursor: Some(CursorState {
                x: 2,
                y: 4,
                visible: true,
                shape: 2,
            }),
        });
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ServerMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(decoded, msg);
        assert_eq!(
            encoded_sha256(&msg),
            "0814b99a1dc6eaf7918424aa416c066509cbfb73b72344a809c27cde78cb6dbd"
        );
    }

    #[test]
    fn client_shell_graphics_payload_codec_is_frozen() {
        let key = SurfaceGraphicsAssetKey {
            source: SurfaceGraphicsSource::Terminal {
                target: SurfaceGraphicsTarget::Pane {
                    pane_id: "w1:p1".into(),
                },
                image_id: 7,
            },
            image_width: 2,
            image_height: 1,
            format: SurfaceGraphicsFormat::Rgba,
            data_len: 8,
            data_fingerprint: 42,
        };
        let message = ServerMessage::PaneSurface(PaneSurfaceFrame {
            boot_id: "boot-1".into(),
            projection_revision: 2,
            surface_revision: 3,
            frame: FrameData {
                cells: Vec::new(),
                width: 0,
                height: 0,
                cursor: None,
                hyperlinks: Vec::new(),
                graphics: Vec::new(),
            },
            panes: Vec::new(),
            splits: Vec::new(),
            popup: None,
            graphics: SurfaceGraphicsScene {
                assets: vec![SurfaceGraphicsAsset {
                    key: key.clone(),
                    data: vec![255, 0, 0, 255, 0, 255, 0, 255],
                }],
                placements: vec![SurfaceGraphicsPlacement {
                    asset: key,
                    logical_placement_id: 9,
                    x: 1,
                    y: 2,
                    cols: 2,
                    rows: 1,
                    source_x: 0,
                    source_y: 0,
                    source_width: 2,
                    source_height: 1,
                    x_offset: 0,
                    y_offset: 0,
                    z: -1,
                    scrollback_offset: 0,
                }],
                retained_assets: Vec::new(),
            },
        });

        assert_eq!(
            encoded_sha256(&message),
            "49c4efec0f1456c8ca4112ddf6ead1ab75d0224007576c2ccc18c3fca55a69f0"
        );
    }

    #[test]
    fn server_endpoint_control_tag_is_frozen() {
        let message = ServerMessage::EndpointControl {
            kind: "endpoint.welcome.v1".into(),
            data: r#"{"generation":1}"#.into(),
        };
        let encoded = bincode::serde::encode_to_vec(&message, bincode::config::standard()).unwrap();
        assert_eq!(encoded.first(), Some(&20));
        assert_eq!(
            bincode::serde::encode_to_vec(
                ServerMessage::EndpointControl {
                    kind: String::new(),
                    data: String::new(),
                },
                bincode::config::standard(),
            )
            .unwrap(),
            [20, 0, 0]
        );
        let (decoded, _): (ServerMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(decoded, message);
    }

    #[test]
    fn client_shell_server_message_tags_are_frozen() {
        fn tag(message: &ServerMessage) -> u8 {
            *bincode::serde::encode_to_vec(message, bincode::config::standard())
                .unwrap()
                .first()
                .expect("encoded server message should include enum tag")
        }

        let empty_frame = || PaneSurfaceFrame {
            boot_id: "boot".into(),
            projection_revision: 1,
            surface_revision: 1,
            frame: FrameData {
                cells: Vec::new(),
                width: 0,
                height: 0,
                cursor: None,
                hyperlinks: Vec::new(),
                graphics: Vec::new(),
            },
            panes: Vec::new(),
            splits: Vec::new(),
            popup: None,
            graphics: SurfaceGraphicsScene::default(),
        };
        assert_eq!(tag(&ServerMessage::PaneSurface(empty_frame())), 13);
        assert_eq!(
            tag(&ServerMessage::ClientShellError {
                message: String::new(),
            }),
            15
        );
        assert_eq!(
            tag(&ServerMessage::ClientShellKeyboardReportAll { enabled: false }),
            17
        );
        assert_eq!(
            tag(&ServerMessage::ClientShellEndpointResponseChunk {
                boot_id: String::new(),
                request_id: String::new(),
                final_chunk: true,
                data: Vec::new(),
            }),
            18
        );
        assert_eq!(
            tag(&ServerMessage::PaneSurfacePatch(PaneSurfacePatch {
                boot_id: String::new(),
                projection_revision: 0,
                base_surface_revision: 0,
                surface_revision: 0,
                rows: Vec::new(),
                panes: Vec::new(),
                cursor: None,
            })),
            19
        );
        assert_eq!(
            tag(&ServerMessage::EndpointControl {
                kind: String::new(),
                data: String::new(),
            }),
            20
        );
    }

    #[test]
    fn client_shell_snapshot_roundtrip() {
        let msg = ServerMessage::ClientShellSnapshot(Box::new(ClientShellSnapshot {
            boot_id: "boot-1".into(),
            revision: 1,
            config_diagnostic: Some("endpoint config warning".into()),
            product_announcement: Some(ClientShellProductAnnouncement {
                version: "0.8.2".into(),
                id: "client-shell".into(),
                title: "Client shell".into(),
                body: "### New\n- Client-owned chrome".into(),
                preview: false,
            }),
            update_available: Some("0.8.3".into()),
            update_install_command: "herdr update".into(),
            server_keybindings_toml: Some("[keys]\nprefix = \"ctrl+a\"\n".into()),
            latest_release_notes_available: true,
            integration_updates_available: true,
            worktree_directory: "/tmp/herdr-worktrees".into(),
            release_notes: Some(ClientShellReleaseNotes {
                version: "0.8.3".into(),
                body: "### New\n- Update ready".into(),
                preview: true,
            }),
            focused_workspace_id: Some("w1".into()),
            focused_tab_id: Some("w1:t1".into()),
            focused_pane_id: Some("w1:p1".into()),
            tab_bar_right: vec![ClientShellTabStatusSegment {
                text: "host".into(),
                accent: false,
            }],
            tab_bar_right_separator: " · ".into(),
            agent_view_label: None,
            agent_order: Vec::new(),
            workspaces: vec![ClientShellWorkspace {
                workspace_id: "w1".into(),
                active_tab_id: "w1:t1".into(),
                new_workspace_cwd: "/tmp".into(),
                number: 1,
                label: "shell".into(),
                custom_label: false,
                branch: Some("main".into()),
                git_ahead_behind: None,
                tokens: Vec::new(),
                worktree: None,
                focused: true,
                agent_status: crate::api::schema::AgentStatus::Idle,
            }],
            tabs: vec![ClientShellTab {
                tab_id: "w1:t1".into(),
                workspace_id: "w1".into(),
                number: 1,
                label: "main".into(),
                custom_label: true,
                zoomed: false,
                focused: true,
                agent_status: crate::api::schema::AgentStatus::Idle,
            }],
            panes: vec![ClientShellPane {
                pane_id: "w1:p1".into(),
                workspace_id: "w1".into(),
                tab_id: "w1:t1".into(),
                label: None,
                cwd: Some("/repo".into()),
                foreground_cwd: Some("/repo".into()),
                focused: true,
                right_click_passthrough: false,
            }],
            agents: Vec::new(),
            commands: vec![ClientShellCommand {
                command_id: "cmd_0123456789abcdef0123456789abcdef".into(),
                binding_label: "prefix+z".into(),
                binding_labels: vec!["prefix+z".into()],
                action: ClientShellCommandAction::Shell,
                description: Some("deploy".into()),
            }],
        }));
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ServerMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn semantic_notification_roundtrip() {
        let msg = ServerMessage::SemanticNotification(SemanticNotification {
            kind: SemanticNotificationKind::NeedsAttention,
            title: "codex needs attention".into(),
            body: Some("repo · 1".into()),
            sound: Some(SemanticNotificationSound::Request),
            agent: Some("codex".into()),
            workspace_id: Some("w1".into()),
            tab_id: Some("w1:t1".into()),
            pane_id: Some("w1:p1".into()),
            position: Some(crate::config::ToastHerdrPosition::TopRight),
        });
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ServerMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn server_graphics_roundtrip() {
        let msg = ServerMessage::Graphics {
            bytes: b"\x1b_Ga=d,d=A,q=2;\x1b\\".to_vec(),
        };
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ServerMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
        assert_eq!(
            encoded_sha256(&msg),
            "28a420f92e0e05e6760a8c140baf307c360c6d1b1aa68027481b324f87e22c44"
        );
    }

    #[test]
    fn server_terminal_frame_roundtrip() {
        let msg = ServerMessage::Terminal(TerminalFrame {
            seq: 7,
            width: 120,
            height: 40,
            full: false,
            bytes: b"\x1b[1;1Hhello".to_vec(),
        });
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ServerMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn direct_graphics_messages_roundtrip() {
        let client = ClientMessage::GraphicsTransmissionResult {
            transfer_id: 7,
            image_id: 42,
            success: false,
        };
        let encoded = bincode::serde::encode_to_vec(&client, bincode::config::standard()).unwrap();
        let (decoded, _): (ClientMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(client, decoded);

        let server = ServerMessage::GraphicsFile {
            path: "/run/user/1000/herdr/source/frame".into(),
            expected_len: 4,
            image_id: 42,
            transfer_id: 7,
            leading: b"\x1b[2;3H".to_vec(),
            control: "a=T,f=32,i=42,q=0".into(),
            surface_asset: None,
        };
        let encoded = bincode::serde::encode_to_vec(&server, bincode::config::standard()).unwrap();
        let (decoded, _): (ServerMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(server, decoded);
    }

    #[cfg(unix)]
    #[test]
    fn framing_over_unix_socketpair() {
        use std::os::unix::net::UnixStream;

        let (mut a, mut b) = UnixStream::pair().expect("socketpair");

        let messages = vec![
            ClientMessage::TerminalHello {
                version: super::super::PROTOCOL_VERSION,
                cols: 200,
                rows: 60,
                cell_width_px: 8,
                cell_height_px: 16,
                pixel_mouse: true,
            },
            ClientMessage::Input {
                data: b"hello world".to_vec(),
            },
            ClientMessage::ClipboardImage {
                target: ClientClipboardImageTarget::DirectTerminal,
                extension: "png".to_owned(),
                data: vec![0x89, b'P', b'N', b'G'],
            },
            ClientMessage::Resize {
                cols: 100,
                rows: 30,
                cell_width_px: 8,
                cell_height_px: 16,
                pixel_mouse: true,
            },
            ClientMessage::Detach,
        ];

        // Set non-blocking so we can write and read in the same test.
        a.set_nonblocking(false).unwrap();
        b.set_nonblocking(false).unwrap();

        for msg in &messages {
            write_message(&mut a, msg).unwrap();
        }

        for expected in &messages {
            let decoded: ClientMessage = read_message(&mut b, MAX_FRAME_SIZE).unwrap();
            assert_eq!(*expected, decoded);
        }
    }

    #[test]
    fn client_shell_pane_input_roundtrips_semantic_and_windows_keys() {
        let windows_record = WindowsKeyRecord {
            key_down: true,
            repeat_count: 1,
            virtual_key_code: 0x37,
            virtual_scan_code: 0x08,
            unicode: 0,
            control_key_state: 0x0008,
        };
        let message = ClientMessage::ClientShellPaneInput {
            pane_id: "w1:p2".into(),
            events: vec![
                ClientPaneInputEvent::Key {
                    code: ClientKeyCode::Char('l'),
                    modifiers: crossterm::event::KeyModifiers::SHIFT.bits(),
                    kind: ClientKeyKind::Release,
                    repeat_count: 1,
                    shifted_codepoint: Some('L' as u32),
                    generated_text: None,
                    tracks_release: true,
                    physical_key_id: None,
                    windows_record: None,
                },
                ClientPaneInputEvent::Key {
                    code: ClientKeyCode::Char('7'),
                    modifiers: crossterm::event::KeyModifiers::CONTROL.bits(),
                    kind: ClientKeyKind::Press,
                    repeat_count: 1,
                    shifted_codepoint: None,
                    generated_text: None,
                    tracks_release: true,
                    physical_key_id: Some(0x08),
                    windows_record: Some(windows_record),
                },
            ],
        };
        let encoded = bincode::serde::encode_to_vec(&message, bincode::config::standard())
            .expect("encode targeted semantic input");
        let (decoded, _): (ClientMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard())
                .expect("decode targeted semantic input");
        assert_eq!(decoded, message);
        assert_eq!(
            encoded_sha256(&message),
            "f558384bb53dfd2baf1fa72e1709d88be6891da79e51f88513905afc085065e6"
        );
    }
}
