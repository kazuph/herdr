//! Extended endpoint key records use the fixed generation-1 input semantics.
//! Existing private/local input continues through the fork's Ghostty encoder.

use crate::input::KeyboardProtocol;
use crate::protocol::endpoint_wire::{ClientKeyCode, ClientKeyKind, ClientPaneInputEvent};
use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};
use std::fmt::Write as _;

const KITTY_FLAG_REPORT_EVENT_TYPES: u16 = 0b0000_0010;
const KITTY_FLAG_REPORT_ALTERNATE_KEYS: u16 = 0b0000_0100;
pub(super) const KITTY_FLAG_REPORT_ALL_KEYS: u16 = 0b0000_1000;
const KITTY_FLAG_REPORT_ASSOCIATED_TEXT: u16 = 0b0001_0000;

#[derive(Clone)]
struct TerminalKey {
    code: KeyCode,
    modifiers: KeyModifiers,
    kind: KeyEventKind,
    shifted_codepoint: Option<u32>,
    generated_text: Option<String>,
    physical_identity_hint: bool,
    windows_dead_key: bool,
}
impl TerminalKey {
    fn with_modifiers(mut self, modifiers: KeyModifiers) -> Self {
        self.modifiers = modifiers;
        self
    }
    fn has_physical_identity(&self) -> bool {
        self.physical_identity_hint
    }
    fn is_windows_dead_key(&self) -> bool {
        self.windows_dead_key
    }
}
trait EndpointKeyboardProtocol {
    fn reports_all_keys(self) -> bool;
}
impl EndpointKeyboardProtocol for KeyboardProtocol {
    fn reports_all_keys(self) -> bool {
        matches!(self, Self::Kitty { flags } if flags & KITTY_FLAG_REPORT_ALL_KEYS != 0)
    }
}

pub(crate) fn encode(
    runtime: &crate::terminal::TerminalRuntime,
    event: &ClientPaneInputEvent,
) -> Result<Vec<u8>, String> {
    let ClientPaneInputEvent::Key {
        code,
        modifiers,
        kind,
        repeat_count,
        shifted_codepoint,
        generated_text,
        tracks_release,
        physical_key_id: _,
        windows_record,
    } = event
    else {
        return Err("expected endpoint key event".into());
    };
    let modifiers = KeyModifiers::from_bits_truncate(*modifiers);
    let release = *kind == ClientKeyKind::Release;
    let key = TerminalKey {
        code: key_code(code),
        modifiers,
        kind: match kind {
            ClientKeyKind::Press => KeyEventKind::Press,
            ClientKeyKind::Repeat => KeyEventKind::Repeat,
            ClientKeyKind::Release => KeyEventKind::Release,
        },
        shifted_codepoint: *shifted_codepoint,
        generated_text: if release {
            None
        } else {
            generated_text.clone()
        },
        // As in the upstream semantic adapter, physical_key_id is an identity field;
        // tracks_release plus committed text decides physical versus text encoding.
        physical_identity_hint: *tracks_release && generated_text.is_some(),
        windows_dead_key: matches!(code, ClientKeyCode::Char(_))
            && modifiers.difference(KeyModifiers::SHIFT).is_empty()
            && windows_record.is_some_and(|record| record.unicode == 0),
    };
    let protocol = runtime.keyboard_protocol();
    let encode = |key: TerminalKey| {
        if key.generated_text.is_some()
            || key.windows_dead_key
            || protocol.reports_all_keys()
            || matches!(protocol, KeyboardProtocol::Kitty { flags } if flags & KITTY_FLAG_REPORT_ASSOCIATED_TEXT != 0)
        {
            encode_once(key, protocol)
        } else {
            runtime.encode_terminal_key(crate::input::TerminalKey {
                code: key.code,
                modifiers: key.modifiers,
                kind: key.kind,
                shifted_codepoint: key.shifted_codepoint,
            })
        }
    };
    let mut bytes = encode(key.clone());
    if !release {
        let mut repeated = key;
        repeated.kind = KeyEventKind::Repeat;
        let repeated = encode(repeated);
        let copies = usize::from((*repeat_count).max(1) - 1);
        let extra = repeated
            .len()
            .checked_mul(copies)
            .ok_or("endpoint key expansion overflow")?;
        let total = bytes
            .len()
            .checked_add(extra)
            .ok_or("endpoint key expansion overflow")?;
        if total > super::client_transport::MAX_INPUT_PAYLOAD {
            return Err("endpoint key expansion exceeds the input limit".into());
        }
        bytes
            .try_reserve_exact(extra)
            .map_err(|error| format!("endpoint input allocation failed: {error}"))?;
        for _ in 0..copies {
            bytes.extend_from_slice(&repeated);
        }
    }
    Ok(bytes)
}

fn key_code(code: &ClientKeyCode) -> KeyCode {
    match code {
        ClientKeyCode::Backspace => KeyCode::Backspace,
        ClientKeyCode::Enter => KeyCode::Enter,
        ClientKeyCode::Left => KeyCode::Left,
        ClientKeyCode::Right => KeyCode::Right,
        ClientKeyCode::Up => KeyCode::Up,
        ClientKeyCode::Down => KeyCode::Down,
        ClientKeyCode::Home => KeyCode::Home,
        ClientKeyCode::End => KeyCode::End,
        ClientKeyCode::PageUp => KeyCode::PageUp,
        ClientKeyCode::PageDown => KeyCode::PageDown,
        ClientKeyCode::Tab => KeyCode::Tab,
        ClientKeyCode::BackTab => KeyCode::BackTab,
        ClientKeyCode::Delete => KeyCode::Delete,
        ClientKeyCode::Insert => KeyCode::Insert,
        ClientKeyCode::Esc => KeyCode::Esc,
        ClientKeyCode::Null => KeyCode::Null,
        ClientKeyCode::Char(ch) => KeyCode::Char(*ch),
        ClientKeyCode::F(n) => KeyCode::F(*n),
    }
}

fn encode_once(key: TerminalKey, protocol: KeyboardProtocol) -> Vec<u8> {
    // The host layout has not committed text for this Windows dead key. Neither
    // legacy nor Kitty panes should receive its physical character fallback.
    // Legacy Windows panes take the native ConPTY fallback before reaching this encoder.
    if key.is_windows_dead_key() {
        return Vec::new();
    }

    // Super has no legacy character encoding, and Ctrl+Shift+letter would
    // collapse into the same C0 byte as Ctrl+letter. Preserve both chords with
    // CSI-u, matching Ghostty's legacy encoder.
    if matches!(protocol, KeyboardProtocol::Legacy)
        && key.kind != crossterm::event::KeyEventKind::Release
        && legacy_chord_needs_csi_u(&key)
    {
        if let Some(bytes) = try_encode_csi_u(&key, 0) {
            return bytes;
        }
    }

    // REPORT_ALL_KEYS must retain physical press/repeat/release semantics instead of
    // reducing a native key to its layout-generated text.
    let preserve_physical_key = key.has_physical_identity() && protocol.reports_all_keys();
    if !preserve_physical_key && key.kind != crossterm::event::KeyEventKind::Release {
        if let Some(text) = &key.generated_text {
            return text.as_bytes().to_vec();
        }
    }

    // A release event only produces bytes when the pane protocol reports event
    // types (Kitty REPORT_EVENT_TYPES). Otherwise the child expects a single
    // legacy byte per keystroke, so re-emitting it on release would double keys
    // like Enter/Backspace. The Ghostty wrapper can route release events through
    // this fallback, so guard the fallback encoder too.
    if key.kind == crossterm::event::KeyEventKind::Release && !protocol.reports_event_types() {
        return Vec::new();
    }

    let kitty_first = protocol.reports_all_keys()
        || (key.kind == crossterm::event::KeyEventKind::Release && protocol.reports_event_types());

    if kitty_first {
        if let KeyboardProtocol::Kitty { flags } = protocol {
            if let Some(bytes) = try_encode_csi_u(&key, flags) {
                return bytes;
            }
        }
    }

    if let Some(bytes) = encode_text_input(&key) {
        return bytes;
    }

    if !kitty_first {
        if let KeyboardProtocol::Kitty { flags } = protocol {
            if let Some(bytes) = try_encode_csi_u(&key, flags) {
                return bytes;
            }
        }
    }
    if key.kind == crossterm::event::KeyEventKind::Release && protocol.reports_event_types() {
        return Vec::new();
    }
    encode_legacy(key)
}

fn legacy_chord_needs_csi_u(key: &TerminalKey) -> bool {
    let KeyCode::Char(ch) = key.code else {
        return false;
    };
    key.modifiers.contains(KeyModifiers::SUPER)
        || (ch.is_ascii_alphabetic()
            && key
                .modifiers
                .contains(KeyModifiers::CONTROL | KeyModifiers::SHIFT))
}

fn try_encode_csi_u(key: &TerminalKey, flags: u16) -> Option<Vec<u8>> {
    let mods = key.modifiers;
    let event_suffix = kitty_event_suffix(key, flags);
    let report_all_keys = flags & KITTY_FLAG_REPORT_ALL_KEYS != 0;

    if !report_all_keys
        && key.modifiers.is_empty()
        && matches!(key.code, KeyCode::Enter | KeyCode::Tab | KeyCode::Backspace)
    {
        return None;
    }

    // Unmodified keys use legacy encoding (more compatible)
    if mods.is_empty() && event_suffix.is_none() && !report_all_keys {
        return None;
    }

    // Special keys (arrows, F-keys, etc.) have well-established legacy
    // xterm modified formats (\x1b[1;3A for Alt+Up, etc.) that are universally
    // understood. Even Ghostty sends these in legacy format with kitty mode on.
    // Only use CSI u for character keys and keys without legacy representations.
    match key.code {
        KeyCode::Up
        | KeyCode::Down
        | KeyCode::Left
        | KeyCode::Right
        | KeyCode::Home
        | KeyCode::End
        | KeyCode::PageUp
        | KeyCode::PageDown
        | KeyCode::Insert
        | KeyCode::Delete
        | KeyCode::F(_)
            if event_suffix.is_none() && !report_all_keys =>
        {
            return None; // let legacy handle these
        }
        _ => {}
    }

    let (codepoint, alternate_shifted) = match key.code {
        KeyCode::Char(c) => {
            let base = canonical_kitty_char(c, mods);
            let shifted = alternate_shifted_codepoint(key, flags);
            (base as u32, shifted)
        }
        KeyCode::Enter => (13, None),
        KeyCode::Tab => (9, None),
        KeyCode::Backspace => (127, None),
        KeyCode::Esc => (27, None),
        KeyCode::Left => (57417, None),
        KeyCode::Right => (57418, None),
        KeyCode::Up => (57419, None),
        KeyCode::Down => (57420, None),
        KeyCode::PageUp => (57421, None),
        KeyCode::PageDown => (57422, None),
        KeyCode::Home => (57423, None),
        KeyCode::End => (57424, None),
        KeyCode::Insert => (57425, None),
        KeyCode::Delete => (57426, None),
        _ => return None, // fall back to legacy for unhandled keys
    };

    let modifier = kitty_modifier(mods);

    let mut sequence = String::with_capacity(32);
    sequence.push_str("\x1b[");
    write!(&mut sequence, "{codepoint}").ok()?;
    if let Some(shifted) = alternate_shifted {
        write!(&mut sequence, ":{shifted}").ok()?;
    }
    write!(&mut sequence, ";{modifier}").ok()?;
    if let Some(event) = event_suffix {
        write!(&mut sequence, ":{event}").ok()?;
    }
    if flags & KITTY_FLAG_REPORT_ASSOCIATED_TEXT != 0 {
        if let Some(text) = text_codepoint_for_key(key) {
            write!(&mut sequence, ";{text}").ok()?;
        }
    }
    sequence.push('u');

    Some(sequence.into_bytes())
}

fn text_codepoint_for_key(key: &TerminalKey) -> Option<u32> {
    let ch = text_char_for_key(key)?;
    (!ch.is_control()).then_some(ch as u32)
}

/// Legacy terminal encoding (standard escape sequences).
fn encode_legacy(key: TerminalKey) -> Vec<u8> {
    let mods = key.modifiers;

    // Modified special keys (arrows, home, end, etc.) use xterm format:
    //   \x1b[1;{modifier}A  for arrows/home/end
    //   \x1b[{n};{modifier}~ for insert/delete/pgup/pgdn
    // The ESC-prefix hack doesn't work for these since they're already escape sequences.
    if !mods.is_empty() {
        if let Some(bytes) = encode_modified_special(key.code, mods) {
            return bytes;
        }
    }

    // Alt modifier on character keys: prefix with ESC
    if mods.contains(KeyModifiers::ALT) {
        let inner = key.with_modifiers(mods.difference(KeyModifiers::ALT));
        let mut bytes = vec![0x1b];
        bytes.extend(encode_legacy_inner(inner));
        return bytes;
    }
    encode_legacy_inner(key)
}

/// xterm-style encoding for modified special keys.
/// Modifier value: 1 + (shift?1:0) + (alt?2:0) + (ctrl?4:0)
fn encode_modified_special(code: KeyCode, mods: KeyModifiers) -> Option<Vec<u8>> {
    let modifier = xterm_modifier(mods);
    if modifier <= 1 {
        return None; // no modifiers to encode
    }

    match code {
        // CSI 1;{mod}{letter} format
        KeyCode::Up => Some(format!("\x1b[1;{modifier}A").into_bytes()),
        KeyCode::Down => Some(format!("\x1b[1;{modifier}B").into_bytes()),
        KeyCode::Right => Some(format!("\x1b[1;{modifier}C").into_bytes()),
        KeyCode::Left => Some(format!("\x1b[1;{modifier}D").into_bytes()),
        KeyCode::Home => Some(format!("\x1b[1;{modifier}H").into_bytes()),
        KeyCode::End => Some(format!("\x1b[1;{modifier}F").into_bytes()),
        // CSI {n};{mod}~ format
        KeyCode::Insert => Some(format!("\x1b[2;{modifier}~").into_bytes()),
        KeyCode::Delete => Some(format!("\x1b[3;{modifier}~").into_bytes()),
        KeyCode::PageUp => Some(format!("\x1b[5;{modifier}~").into_bytes()),
        KeyCode::PageDown => Some(format!("\x1b[6;{modifier}~").into_bytes()),
        // F1-F4: CSI 1;{mod}{P-S}
        KeyCode::F(1) => Some(format!("\x1b[1;{modifier}P").into_bytes()),
        KeyCode::F(2) => Some(format!("\x1b[1;{modifier}Q").into_bytes()),
        KeyCode::F(3) => Some(format!("\x1b[1;{modifier}R").into_bytes()),
        KeyCode::F(4) => Some(format!("\x1b[1;{modifier}S").into_bytes()),
        // F5-F12: CSI {n};{mod}~
        KeyCode::F(n @ 5..=12) => {
            let code = match n {
                5 => 15,
                6 => 17,
                7 => 18,
                8 => 19,
                9 => 20,
                10 => 21,
                11 => 23,
                12 => 24,
                _ => unreachable!(),
            };
            Some(format!("\x1b[{code};{modifier}~").into_bytes())
        }
        _ => None,
    }
}

/// xterm modifier encoding: 1 + shift(1) + alt(2) + ctrl(4)
/// Used for legacy modified special keys (arrows, function keys, etc.)
fn xterm_modifier(mods: KeyModifiers) -> u32 {
    let mut m = 1u32;
    if mods.contains(KeyModifiers::SHIFT) {
        m += 1;
    }
    if mods.contains(KeyModifiers::ALT) {
        m += 2;
    }
    if mods.contains(KeyModifiers::CONTROL) {
        m += 4;
    }
    m
}

/// Kitty protocol modifier encoding: 1 + shift(1) + alt(2) + ctrl(4) + super(8) + hyper(16) + meta(32)
/// Superset of xterm — adds Super/Hyper/Meta bits.
fn kitty_modifier(mods: KeyModifiers) -> u32 {
    let mut m = xterm_modifier(mods);
    if mods.contains(KeyModifiers::SUPER) {
        m += 8;
    }
    if mods.contains(KeyModifiers::HYPER) {
        m += 16;
    }
    if mods.contains(KeyModifiers::META) {
        m += 32;
    }
    m
}

fn encode_text_input(key: &TerminalKey) -> Option<Vec<u8>> {
    let ch = text_char_for_key(key)?;
    let mut buf = [0u8; 4];
    Some(ch.encode_utf8(&mut buf).as_bytes().to_vec())
}

fn text_char_for_key(key: &TerminalKey) -> Option<char> {
    if key.kind == crossterm::event::KeyEventKind::Release {
        return None;
    }

    let KeyCode::Char(ch) = key.code else {
        return None;
    };

    if key.modifiers.is_empty() {
        return Some(ch);
    }
    if key.modifiers == KeyModifiers::SHIFT {
        return shifted_text_char(key, ch);
    }
    None
}

fn shifted_text_char(key: &TerminalKey, ch: char) -> Option<char> {
    if let Some(shifted) = key.shifted_codepoint.and_then(char::from_u32) {
        return Some(shifted);
    }

    if ch.is_ascii_uppercase() {
        return Some(ch);
    }

    if ch.is_ascii_lowercase() {
        return Some(ch.to_ascii_uppercase());
    }

    if is_shifted_ascii_punctuation(ch) {
        return Some(ch);
    }

    None
}

fn is_shifted_ascii_punctuation(ch: char) -> bool {
    matches!(
        ch,
        '!' | '@'
            | '#'
            | '$'
            | '%'
            | '^'
            | '&'
            | '*'
            | '('
            | ')'
            | '_'
            | '+'
            | '{'
            | '}'
            | '|'
            | ':'
            | '"'
            | '<'
            | '>'
            | '?'
            | '~'
    )
}

fn canonical_kitty_char(ch: char, mods: KeyModifiers) -> char {
    if mods.contains(KeyModifiers::SHIFT) && ch.is_ascii_uppercase() {
        ch.to_ascii_lowercase()
    } else {
        ch
    }
}

fn alternate_shifted_codepoint(key: &TerminalKey, flags: u16) -> Option<u32> {
    if flags & KITTY_FLAG_REPORT_ALTERNATE_KEYS == 0 {
        return None;
    }

    if let Some(shifted) = key.shifted_codepoint {
        return Some(shifted);
    }

    match key.code {
        KeyCode::Char(ch)
            if key.modifiers.contains(KeyModifiers::SHIFT) && ch.is_ascii_uppercase() =>
        {
            Some(ch as u32)
        }
        _ => None,
    }
}

fn kitty_event_suffix(key: &TerminalKey, flags: u16) -> Option<u8> {
    if flags & KITTY_FLAG_REPORT_EVENT_TYPES == 0 {
        return None;
    }

    Some(match key.kind {
        crossterm::event::KeyEventKind::Press => 1,
        crossterm::event::KeyEventKind::Repeat => 2,
        crossterm::event::KeyEventKind::Release => 3,
    })
}

fn encode_legacy_inner(key: TerminalKey) -> Vec<u8> {
    match key.code {
        KeyCode::Char(ch) => {
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                let upper = ch.to_ascii_uppercase();
                match upper {
                    'A'..='Z' => vec![upper as u8 - 64],
                    ' ' | '@' | '2' => vec![0],
                    '[' | '3' => vec![27],
                    '\\' | '4' => vec![28],
                    ']' | '5' => vec![29],
                    '^' | '6' => vec![30],
                    '_' | '/' | '7' | '-' => vec![31],
                    _ => ch.to_string().into_bytes(),
                }
            } else {
                let ch = if key.modifiers == KeyModifiers::SHIFT {
                    shifted_text_char(&key, ch).unwrap_or(ch)
                } else {
                    ch
                };
                let mut buf = [0u8; 4];
                ch.encode_utf8(&mut buf).as_bytes().to_vec()
            }
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Backspace => vec![127],
        KeyCode::Tab => vec![9],
        KeyCode::BackTab => vec![27, 91, 90],
        KeyCode::Esc => vec![27],
        KeyCode::Left => vec![27, 91, 68],
        KeyCode::Right => vec![27, 91, 67],
        KeyCode::Up => vec![27, 91, 65],
        KeyCode::Down => vec![27, 91, 66],
        KeyCode::Home => vec![27, 91, 72],
        KeyCode::End => vec![27, 91, 70],
        KeyCode::PageUp => vec![27, 91, 53, 126],
        KeyCode::PageDown => vec![27, 91, 54, 126],
        KeyCode::Delete => vec![27, 91, 51, 126],
        KeyCode::Insert => vec![27, 91, 50, 126],
        KeyCode::F(n) => encode_f_key(n),
        _ => vec![],
    }
}

fn encode_f_key(n: u8) -> Vec<u8> {
    match n {
        1 => vec![27, 79, 80],
        2 => vec![27, 79, 81],
        3 => vec![27, 79, 82],
        4 => vec![27, 79, 83],
        5 => vec![27, 91, 49, 53, 126],
        6 => vec![27, 91, 49, 55, 126],
        7 => vec![27, 91, 49, 56, 126],
        8 => vec![27, 91, 49, 57, 126],
        9 => vec![27, 91, 50, 48, 126],
        10 => vec![27, 91, 50, 49, 126],
        11 => vec![27, 91, 50, 51, 126],
        12 => vec![27, 91, 50, 52, 126],
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key() -> TerminalKey {
        TerminalKey {
            code: KeyCode::Char('a'),
            modifiers: KeyModifiers::empty(),
            kind: KeyEventKind::Press,
            shifted_codepoint: None,
            generated_text: Some("a".into()),
            physical_identity_hint: true,
            windows_dead_key: false,
        }
    }
    #[test]
    fn fixed_source_generated_text_and_physical_event_semantics() {
        assert_eq!(encode_once(key(), KeyboardProtocol::Legacy), b"a");
        assert_eq!(
            encode_once(key(), KeyboardProtocol::Kitty { flags: 27 }),
            b"\x1b[97;1:1;97u"
        );
        let mut release = key();
        release.kind = KeyEventKind::Release;
        release.generated_text = None;
        assert_eq!(encode_once(release.clone(), KeyboardProtocol::Legacy), b"");
        assert_eq!(
            encode_once(release, KeyboardProtocol::Kitty { flags: 11 }),
            b"\x1b[97;1:3u"
        );
        let mut commit = key();
        commit.generated_text = Some("日本語".into());
        commit.physical_identity_hint = false;
        assert_eq!(
            encode_once(commit, KeyboardProtocol::Kitty { flags: 3 }),
            "日本語".as_bytes()
        );
        let mut dead = key();
        dead.windows_dead_key = true;
        assert_eq!(encode_once(dead, KeyboardProtocol::Legacy), b"");
    }
}
