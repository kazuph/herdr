//! Screen prompt detection for blocked agent panes.
//!
//! Parses the numbered selection / permission prompts that agent TUIs draw
//! (Claude Code AskUserQuestion — including multi-question tab flows —, Claude
//! Code / Codex permission confirmations, Devin trust and permission prompts)
//! from a pane's *detection* screen snapshot. The extraction rules are a Rust
//! port of saberante's `parse_prompt`/`_parse_mq_tabs` (`saberante/sabera.py`)
//! extended with the option and footer shapes of Codex (`› 1. Yes (y)` rows,
//! "press enter to confirm" footer) and Devin (`❭ 1 Yes` rows, "esc cancel"
//! footer).

use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;

// `1. label` rows with an optional `❯` cursor (saberante parity), plus `›`.
static OPTION_DOT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:[❯›]\s*)?(\d+)\.\s+(.+?)\s*$").unwrap());
// Devin-style bare numbers that always carry a cursor/bullet marker:
// `❭ 1 Yes  (Approve once)`, `· 2 No`.
static OPTION_MARKED_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:❭\s*|·\s*)(\d+)\s+(.+?)\s*$").unwrap());
static PROMPT_END_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)esc\s+to\s+cancel|esc\s+cancel|enter\s+to\s+select|enter\s+to\s+confirm|press\s+enter\s+to\s+confirm|enter\s+to\s+submit|↵\s*confirm")
        .unwrap()
});
static RULE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*[─━-]{10,}\s*$").unwrap());
// Dashed borders Claude Code draws around permission command boxes (`╌`,
// `╍`). Unlike `RULE_RE` they sit inside the prompt region, so they are
// skipped rather than ending the upward question walk — a hard stop would
// drop the command and warning lines above them into the transcript.
static DASHED_RULE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*[╌╍]{8,}\s*$").unwrap());
// Claude Code's AFK guard on unattended sessions: `⚠ Claude Code will
// automatically deny this request in 1:48, to avoid blocking progress on an
// unattended session`. The countdown ticks once per second; it is lifted
// out of the question text (with the digits normalised) into `notice` so it
// cannot churn the prompt signature.
// The warning may render as one row or split across rows, e.g.
// `⚠ Claude Code will automatically deny this request` /
// `in 1:48, to avoid blocking progress on an unattended` / `session`.
static AUTO_DENY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"Claude Code will automatically deny this request|automatically deny this request|in \d+:\d{2},? to avoid blocking progress|to avoid blocking progress on an unattended|unattended session")
        .unwrap()
});
static AUTO_DENY_TIME_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"in \d+:\d{2}").unwrap());
// Volatile displays that must not change a prompt's identity: `M:SS`
// countdowns/clocks, `Ns` elapsed timers, and spinner glyphs.
static VOLATILE_TIME_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\d+:\d{2}(?::\d{2})?|\(\d+(?:\.\d+)?s\b").unwrap());
const SPINNER_CHARS: &[char] = &[
    '✳', '✽', '✢', '✣', '✤', '✥', '◐', '◑', '◒', '◓', '⠋', '⠙', '⠚', '⠞', '⠖', '⠦', '⠴', '⠲', '⠳',
    '⠓', '⠏', '⠹', '⠸', '⠼',
];
// Codex's question overlay starts here, independently of the transcript
// above it. The optional countdown is presentation, never prompt identity.
static CODEX_QUESTION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(Question \d+/\d+ \(\d+ unanswered\))(?:\s*·\s*auto-resolves in (?:\d+[hms]\s*)+)?\s*$")
        .expect("valid Codex question header regex")
});
static MQ_TAB_ROW_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*←\s+(.*?)\s*→\s*$").unwrap());
static MQ_TAB_ITEM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"([☐☒✔])\s*([^☐☒✔←→]+)").unwrap());
// Codex-style per-option shortcut hints: "… (y)", "… (a)", "… (esc)".
static SHORTCUT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\(([a-zA-Z0-9]|esc|ESC|Esc)\)\s*$").unwrap());
// Claude Code AskUserQuestion multiSelect checkbox inside the option label:
// `1. [ ] label`, `1. [✔] label`.
static CHECKBOX_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[( |x|X|✔)\]\s*(.*)$").unwrap());
// The advance pseudo-row multiSelect questions draw below the last option —
// `Next` between questions, `Submit` on the final one.
static ADVANCE_ROW_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:[❯›]\s*)?(Next|Submit)\s*$").unwrap());

const FREE_TEXT_LABELS: [&str; 2] = ["Type something", "Chat about this"];
/// saberante stops collecting question lines after 60 rows of prompt context.
const MAX_QUESTION_LINES: usize = 60;
/// Minimum space run treated as a wrap-glue boundary. Claude Code highlights
/// the selected option's rows to the full terminal width; those rows carry
/// the wrap flag, so `detection_text_unwrapped` glues the next physical row
/// onto them — an option label absorbs its first description row, or a
/// description row swallows the following numbered option.
const GLUE_GAP_MIN: usize = 4;

/// A single-character shortcut a prompt advertises for an option.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OptionShortcut {
    /// A single key to type (Codex `(y)` / `(a)` / `(p)` hints).
    Char(char),
    /// The Escape key (labels ending in `(esc)`).
    Escape,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MqInfo {
    /// Index of the first unanswered tab (saberante's guess; the caller tracks
    /// the real position through the answers it has sent).
    pub index: usize,
    pub total: usize,
    pub submit: bool,
    pub at_submit: bool,
    pub name: String,
    pub tabs: Vec<String>,
    /// Screen row of the `← … →` tab strip; question lines stop above it.
    pub row: usize,
}

/// The selectable row the `❯` cursor sits on: a numbered option or the
/// `Next`/`Submit` advance pseudo-row multiSelect questions draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Focused {
    Option(u32),
    Advance,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScreenPrompt {
    /// Prompt header: the `☐`/`☒` checkbox line above the options, or the
    /// multi-question progress label ("質問 n/N <tab>", "最終確認").
    pub header: String,
    pub mcp_server: Option<String>,
    /// Question/prompt text collected above the options.
    pub question: String,
    /// Volatile notice lifted out of the question — Claude Code's AFK
    /// auto-deny countdown, digits normalised to `M:SS`. Kept for the
    /// decision body; excluded from the signature so ticking cannot churn
    /// the pending decision.
    pub notice: Option<String>,
    /// Numbered options minus the free-text pseudo options.
    pub choices: BTreeMap<u32, String>,
    /// Per-option detail text drawn on indented rows below an option
    /// (Claude Code AskUserQuestion option descriptions). Soft-wrapped rows
    /// are joined back into a single sentence.
    pub descriptions: BTreeMap<u32, String>,
    /// Option number of the "Type something" free-text slot, when present.
    pub free_text: Option<u32>,
    /// Per-option shortcut hints (Codex `(y)`/`(a)`/`(esc)` suffixes).
    pub shortcuts: BTreeMap<u32, OptionShortcut>,
    /// Multi-question tab metadata when the `← ☐ … ✔ Submit →` row is present.
    pub mq: Option<MqInfo>,
    /// Options carry `[ ]`/`[✔]` checkboxes (AskUserQuestion multiSelect):
    /// numbers toggle and the prompt is submitted through the advance row.
    pub multi_select: bool,
    /// Option numbers currently checked on screen; excluded from the
    /// signature so toggling does not churn the pending decision.
    pub checked: std::collections::BTreeSet<u32>,
    /// Preview box contents keyed by option number. Claude Code only paints
    /// the focused option's preview, so at most one entry exists per frame.
    pub previews: BTreeMap<u32, String>,
    /// A preview box was drawn next to the options; options without a
    /// captured preview may still have one on screen.
    pub preview_box: bool,
    /// Options the preview scan already focused and saw draw no box —
    /// confirmed to have no preview rather than merely uncaptured.
    pub preview_none: std::collections::BTreeSet<u32>,
    /// A `Next`/`Submit` advance pseudo-row sits below the last option.
    pub advance: bool,
    /// Number of the last selectable item above the advance row — every
    /// real option plus the free-text slot ("Chat about this" sits below
    /// the advance row). Arrow-key plans for multiSelect delivery anchor
    /// on it.
    pub advance_after: Option<u32>,
    /// The `❯` cursor position, used to attribute the visible preview and to
    /// plan arrow keys for multiSelect delivery.
    pub focused: Option<Focused>,
    /// The footer offers `Tab to amend` / `Tab to add context` (Claude Code
    /// permission prompts): Tab on the focused option opens a note that
    /// travels with that option. Excluded from the signature — the footer
    /// drops the hint while a note is being typed.
    pub amend: bool,
    /// Codex's visible notes-editor footer, independent of option focus.
    pub codex_notes_open: bool,
}

impl ScreenPrompt {
    pub fn is_codex_question(&self) -> bool {
        CODEX_QUESTION_RE.is_match(&self.header)
    }

    /// A question header or free-text slot distinguishes questions from permissions.
    pub fn is_question(&self) -> bool {
        self.mcp_server.is_none() && (!self.header.is_empty() || self.free_text.is_some())
    }

    /// saberante's `prompt_key`: identity of the visible prompt so the same
    /// screen does not produce a second decision and tab moves are detected.
    /// The question text is part of the identity: permission prompts that
    /// share options ("Yes / don't ask again / No") are still different
    /// prompts when they ask about different commands.
    pub fn signature(&self) -> String {
        // Checked state, the cursor position and the visible preview change
        // as the user moves through the prompt on screen; keeping them out
        // of the signature prevents the watch from churning the decision.
        // The header is excluded too: the multi-question tab markers flip
        // `☐`→`☒` (and `質問 N/M` → `最終確認`) as options are toggled, which
        // is the same prompt answering itself, not a prompt change.
        // Displays that only change with time — countdowns (`M:SS`), elapsed
        // timers (`Ns`), spinner glyphs — are normalised away so a ticking
        // screen cannot manufacture prompt changes.
        serde_json::to_string(&(
            normalize_volatile(&self.question),
            self.choices
                .iter()
                .map(|(number, label)| (*number, normalize_volatile(label)))
                .collect::<BTreeMap<u32, String>>(),
            self.descriptions
                .iter()
                .map(|(number, text)| (*number, normalize_volatile(text)))
                .collect::<BTreeMap<u32, String>>(),
            self.free_text,
            self.multi_select,
            &self.mcp_server,
        ))
        .unwrap_or_default()
    }

    /// Decision title: the question text, or the header stripped of checkbox
    /// markers when the prompt carries no question lines.
    pub fn title(&self) -> String {
        let question = self.question.trim();
        if !question.is_empty() {
            return question.to_string();
        }
        self.header
            .trim_start_matches(['☐', '☒', '✔', ' '])
            .to_string()
    }

    /// Multi-question progress label for the decision body.
    pub fn progress(&self) -> Option<String> {
        if self.is_codex_question() {
            return Some(self.header.clone());
        }
        self.mq.as_ref().map(|mq| {
            if mq.at_submit {
                "最終確認".to_string()
            } else {
                format!("質問 {}/{} {}", mq.index + 1, mq.total, mq.name)
            }
        })
    }

    /// Volatile notice lifted out of the question, for the decision body.
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }
}

/// Displays that change with time get a stable form for identity purposes:
/// `M:SS` countdowns/clocks, `(Ns` elapsed timers, spinner glyphs.
pub(crate) fn normalize_volatile(text: &str) -> String {
    let text = VOLATILE_TIME_RE.replace_all(text, "M:SS");
    text.chars()
        .map(|c| if SPINNER_CHARS.contains(&c) { '✳' } else { c })
        .collect()
}

/// Rows above the question walk's stop point that hold the `⏺`-marked call
/// the prompt is asking about (Claude Code permission dialogs). The call
/// head can be clipped off the visible box, so the call line is the last
/// place the full command survives.
const CALL_SCAN_ROWS: usize = 12;

/// A warning fragment row: short text continuing a sentence Claude Code
/// wrapped mid-phrase in a narrow pane — no box/rule/agent-message prefix.
fn is_afk_continuation(stripped: &str) -> bool {
    stripped.len() <= 48 && !stripped.starts_with(['│', '─', '╌', '╍', '⎿', '❯', '›', '❭', '⏺'])
}

/// Recover the `⏺` call line nearest to (or at) `stop` plus its wrapped
/// continuation rows. Returns the call text without the `⏺` marker.
fn recover_call_lines(lines: &[&str], stop: usize) -> Option<String> {
    if stop >= lines.len() {
        return None;
    }
    let is_call = |line: &str| line.trim().starts_with('⏺');
    let start = if is_call(lines[stop]) {
        Some(stop)
    } else {
        let lo = stop.saturating_sub(CALL_SCAN_ROWS);
        (lo..stop).rev().find(|&i| is_call(lines[i]))
    }?;
    let mut parts = vec![lines[start]
        .trim()
        .trim_start_matches('⏺')
        .trim()
        .to_string()];
    for line in &lines[start + 1..] {
        let stripped = line.trim().trim_start_matches('│').trim();
        if stripped.is_empty()
            || stripped.starts_with(['⎿', '⏺', '☐', '☒', '❯', '›', '❭'])
            || RULE_RE.is_match(line)
            || DASHED_RULE_RE.is_match(stripped)
        {
            break;
        }
        parts.push(stripped.to_string());
    }
    let call = join_description_lines(parts.into_iter());
    (!call.is_empty()).then_some(call)
}

/// The head fragment used to decide whether the question already contains
/// the recovered call — skips the `Tool(` prefix so a `│`-boxed copy of the
/// same command still counts as present.
fn call_head(call: &str) -> &str {
    let head = call
        .find('(')
        .map(|at| &call[at + 1..])
        .unwrap_or(call)
        .trim();
    &head[..head.floor_char_boundary(32)]
}

fn option_match(line: &str) -> Option<(u32, String)> {
    for re in [&OPTION_DOT_RE, &OPTION_MARKED_RE] {
        if let Some(caps) = re.captures(line) {
            let number = caps.get(1)?.as_str().parse::<u32>().ok()?;
            let label = caps.get(2)?.as_str().to_string();
            return Some((number, label));
        }
    }
    None
}

/// Single-key shortcut an option label advertises, for answer delivery.
pub(crate) fn shortcut_hint(text: &str) -> Option<OptionShortcut> {
    shortcut_for(text)
}

fn shortcut_for(text: &str) -> Option<OptionShortcut> {
    let hint = SHORTCUT_RE.captures(text)?.get(1)?.as_str();
    if hint.eq_ignore_ascii_case("esc") {
        return Some(OptionShortcut::Escape);
    }
    hint.chars().next().map(OptionShortcut::Char)
}

/// Split a logical line from the unwrapped detection view back into physical
/// rows. A wrap-glued row keeps its end-of-line padding, so the join shows up
/// as a run of `GLUE_GAP_MIN` or more spaces between two content spans: the
/// upper row's padding plus the lower row's own indent, which cannot be told
/// apart. Returns segments in screen order; segment 0 keeps its leading
/// whitespace while later segments have theirs absorbed into the gap.
fn split_glued_rows(line: &str) -> Vec<&str> {
    let bytes = line.as_bytes();
    let mut segments = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b' ' && !line[..i].trim().is_empty() {
            let run_end = (i..bytes.len())
                .find(|&j| bytes[j] != b' ')
                .unwrap_or(bytes.len());
            if run_end - i >= GLUE_GAP_MIN && run_end < bytes.len() {
                segments.push(&line[start..i]);
                start = run_end;
            }
            i = run_end;
        } else {
            i += 1;
        }
    }
    segments.push(&line[start..]);
    segments
}

/// MultiSelect / focus state gathered while walking the option block.
#[derive(Default)]
struct Extras {
    multi_select: bool,
    checked: std::collections::BTreeSet<u32>,
    advance: bool,
    /// First option number met after (above) the advance row while walking
    /// bottom-up — the last selectable item above `Next`/`Submit`.
    advance_next: Option<u32>,
    focused: Option<Focused>,
}

/// Register a numbered option and attach the continuation rows that were
/// buffered below it while walking upward.
#[allow(clippy::too_many_arguments)]
fn collect_option(
    options: &mut BTreeMap<u32, String>,
    descriptions: &mut BTreeMap<u32, String>,
    shortcuts: &mut BTreeMap<u32, OptionShortcut>,
    pending_lines: &mut Vec<String>,
    extras: &mut Extras,
    number: u32,
    mut label: String,
    cursor: bool,
) {
    if let Some(caps) = CHECKBOX_RE.captures(&label) {
        // AskUserQuestion multiSelect: `[ ]`/`[✔]` inside the option label.
        extras.multi_select = true;
        if caps.get(1).map(|m| m.as_str()) != Some(" ") {
            extras.checked.insert(number);
        }
        label = caps
            .get(2)
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();
    }
    if cursor {
        extras.focused = Some(Focused::Option(number));
    }
    if let Some(shortcut) = shortcut_for(&label) {
        shortcuts.insert(number, shortcut);
    } else {
        shortcuts.remove(&number);
    }
    attach_option_continuations(pending_lines, number, descriptions, shortcuts);
    options.insert(number, label);
}

/// The character sitting on terminal cell column `col`, counting CJK
/// wide chars as two cells (preview-box borders are column-aligned in
/// cells, not chars).
fn char_at_cell(line: &str, col: usize) -> Option<char> {
    let mut width = 0usize;
    for ch in line.chars() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + w > col {
            return Some(ch);
        }
        width += w;
    }
    None
}

/// The row content left of cell column `col` (the option area beside a
/// preview box).
fn left_of_cell(line: &str, col: usize) -> String {
    let mut width = 0usize;
    let mut out = String::new();
    for ch in line.chars() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + w > col {
            break;
        }
        width += w;
        out.push(ch);
    }
    out
}

/// Claude Code draws the focused option's `preview` in a bordered box to the
/// right of the options (`┌─┐` / `│` / `└─┘`). Returns `(box rows: first/last
/// line indexes + border cell column, inner text)`; the box region is then
/// stripped from the lines the option walk sees. `├── ✂ ─ N lines hidden ┤`
/// separators keep their inner text so a truncated preview still reads
/// truthfully.
fn extract_preview_boxes(lines: &[&str]) -> Vec<(usize, usize, usize, Vec<String>)> {
    let mut boxes = Vec::new();
    for top in 0..lines.len() {
        let Some(box_col) = lines[top]
            .chars()
            .scan(0usize, |width, ch| {
                let at = *width;
                *width += unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                Some((ch, at))
            })
            .find(|(ch, _)| *ch == '┌')
            .map(|(_, at)| at)
        else {
            continue;
        };
        // A real box has a `│` side row and a `└` bottom at the same column.
        let mut side = false;
        let mut bottom = None;
        for (row, later) in lines.iter().enumerate().skip(top + 1) {
            match char_at_cell(later, box_col) {
                Some('│') | Some('├') => side = true,
                Some('└') if side => {
                    bottom = Some(row);
                    break;
                }
                _ => {}
            }
        }
        let Some(bottom) = bottom else {
            continue;
        };
        let mut inner = Vec::new();
        for row in &lines[top + 1..bottom] {
            if !matches!(char_at_cell(row, box_col), Some('│') | Some('├')) {
                continue;
            }
            inner.push(preview_inner(row, box_col));
        }
        boxes.push((top, bottom, box_col, inner));
    }
    boxes
}

/// Inner text of one preview-box row: strip the left border at `box_col`
/// and the right border char.
fn preview_inner(row: &str, box_col: usize) -> String {
    let mut width = 0usize;
    let mut start = row.len();
    for (byte, ch) in row.char_indices() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + w > box_col {
            start = byte + ch.len_utf8();
            break;
        }
        width += w;
    }
    // Strip trailing blanks, then the single right-border char, then blanks
    // again — inner `┌─┐`/`└─┘` corners of a drawn diagram must survive.
    let inner = row[start..].trim_end();
    let inner = inner.strip_suffix(['│', '┤']).unwrap_or(inner).trim_end();
    inner.to_string()
}

/// Claude Code's permission footer advertises a per-option note on Tab.
/// The footer may wrap, so the hint is matched across the footer row and the
/// row below it.
fn footer_offers_amend(footer: &[&str]) -> bool {
    let joined = footer
        .iter()
        .take(2)
        .flat_map(|line| line.split_whitespace())
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    joined.contains("tab to amend") || joined.contains("tab to add context")
}

pub(crate) fn option_row(line: &str) -> bool {
    option_match(line).is_some()
}

pub(crate) fn prompt_continuation(screen: &str) -> bool {
    screen
        .lines()
        .find(|line| !line.trim().is_empty())
        .is_some_and(|line| option_match(line).is_some() || FREE_TEXT_LABELS.contains(&line.trim()))
}

pub(crate) fn codex_question_region(screen: &str) -> Option<&str> {
    let mut offset = 0;
    let mut start = None;
    for line in screen.lines() {
        if CODEX_QUESTION_RE.is_match(line) {
            start = Some(offset);
        }
        offset += line.len() + 1;
    }
    start.map(|start| &screen[start..])
}

/// Port of saberante's `parse_prompt`. Returns `None` when the screen holds no
/// numbered option block anchored by a prompt footer or by the screen bottom.
pub(crate) fn parse_screen_prompt(screen: &str) -> Option<ScreenPrompt> {
    let mut raw_lines: Vec<&str> = screen.lines().collect();
    if let Some(header) = raw_lines
        .iter()
        .rposition(|line| line.trim_start().starts_with("MCP server "))
    {
        let mcp_end = raw_lines
            .iter()
            .enumerate()
            .skip(header)
            .find(|(_, line)| PROMPT_END_RE.is_match(line))
            .map(|(row, _)| row);
        let last_end = raw_lines
            .iter()
            .rposition(|line| PROMPT_END_RE.is_match(line));
        // A historical MCP confirmation must not hide a later numbered question.
        if let Some(prompt) = parse_mcp_confirmation(screen) {
            return Some(prompt);
        }
        let mcp_end = mcp_end?;
        if Some(mcp_end) == last_end
            && !raw_lines[mcp_end + 1..]
                .iter()
                .any(|line| option_match(line).is_some())
        {
            return None;
        }
        raw_lines.drain(..=mcp_end);
    }
    for preview in extract_preview_boxes(&raw_lines) {
        if let Some(prompt) = parse_prompt_lines(&raw_lines, Some(preview)) {
            return Some(prompt);
        }
    }
    parse_prompt_lines(&raw_lines, None)
}

fn parse_mcp_confirmation(screen: &str) -> Option<ScreenPrompt> {
    let lines: Vec<_> = screen
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let footer = lines.len().checked_sub(1)?;
    if lines[footer] != "Esc to cancel · ↑/↓ to navigate" {
        return None;
    }
    let options = footer.checked_sub(1)?;
    let focused = match lines[options]
        .split_whitespace()
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["❯", "Accept", "Decline"] => 1,
        ["Accept", "❯", "Decline"] => 2,
        _ => return None,
    };
    let header = lines[..options]
        .iter()
        .rposition(|line| line.starts_with("MCP server “"))?;
    let server = lines[header]
        .strip_prefix("MCP server “")?
        .strip_suffix("” requests your input")?;
    // Only the captured confirmation form has a single question and no fields.
    if server.is_empty() || options != header + 2 {
        return None;
    }
    Some(ScreenPrompt {
        header: String::new(),
        mcp_server: Some(server.to_string()),
        question: lines[header + 1].to_string(),
        notice: None,
        choices: [(1, "Accept".to_string()), (2, "Decline".to_string())].into(),
        descriptions: BTreeMap::new(),
        free_text: None,
        shortcuts: BTreeMap::new(),
        mq: None,
        multi_select: false,
        checked: Default::default(),
        previews: BTreeMap::new(),
        preview_box: false,
        preview_none: Default::default(),
        advance: false,
        advance_after: None,
        focused: Some(Focused::Option(focused)),
        amend: false,
        codex_notes_open: false,
    })
}

type PreviewBox = (usize, usize, usize, Vec<String>);

fn parse_prompt_lines(raw_lines: &[&str], preview: Option<PreviewBox>) -> Option<ScreenPrompt> {
    // Lift the option-side preview box out of the text before option
    // matching — its top border would otherwise land inside the first
    // option's label and its side borders inside descriptions.
    let mut display: Vec<String> = raw_lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            preview
                .as_ref()
                .filter(|(top, bottom, _, _)| i >= *top && i <= *bottom)
                .map(|(_, _, box_col, _)| left_of_cell(line, *box_col))
                .unwrap_or_else(|| line.to_string())
        })
        .collect();
    // Claude wraps a label at the same four-cell indent as its text after
    // the number. Description rows are indented further. Preserve the label
    // as one logical row before the bottom-up option walk.
    if let Some((top, bottom, _, _)) = &preview {
        let mut option_row = None;
        for row in *top..=*bottom {
            if option_match(&display[row]).is_some() {
                option_row = Some(row);
            } else if display[row].starts_with("    ")
                && display[row]
                    .chars()
                    .nth(4)
                    .is_some_and(|ch| !ch.is_whitespace())
            {
                if let Some(previous) = option_row {
                    let tail = display[row].trim().to_string();
                    display[previous] = format!("{} {}", display[previous].trim_end(), tail);
                    display[row].clear();
                }
            }
        }
    }
    let lines: Vec<&str> = display.iter().map(String::as_str).collect();
    // saberante anchors on the LAST footer line so a stale prompt in
    // scrollback cannot shadow the active one.
    let end = lines.iter().rposition(|line| PROMPT_END_RE.is_match(line));
    let mut options: BTreeMap<u32, String> = BTreeMap::new();
    let mut descriptions: BTreeMap<u32, String> = BTreeMap::new();
    let mut shortcuts: BTreeMap<u32, OptionShortcut> = BTreeMap::new();
    // Row of each option and of every rule segment inside the block:
    // Claude Code's free-text slot sits directly above the divider that
    // splits off the "Chat about this" row, and typed text replaces the
    // slot's placeholder label — position is the only stable marker.
    let mut option_rows: BTreeMap<u32, usize> = BTreeMap::new();
    let mut rule_rows: Vec<usize> = Vec::new();
    let mut extras = Extras::default();
    let mut first: Option<usize> = None;
    let mq: Option<MqInfo>;

    if let Some(end) = end {
        // Footer present: collect numbered rows walking upward; a non-option,
        // non-blank, non-continuation, non-rule line stops the block.
        // Indented continuation rows under an option (Claude Code's
        // AskUserQuestion descriptions, Codex's wrapped option tail carrying
        // the `(p)` hint) buffer until the option row above them is found.
        let mut pending_lines: Vec<String> = Vec::new();
        let mut i = end;
        'walk: while i > 0 {
            i -= 1;
            // A wrap-glued logical line holds several physical rows; walk the
            // segments bottom-up so the lowest row is classified first, the
            // same order as unglued lines.
            let segments = split_glued_rows(lines[i]);
            for (segment_index, segment) in segments.iter().enumerate().rev() {
                let glued = segment_index > 0;
                if let Some((number, label)) = option_match(segment) {
                    let cursor = segment.trim_start().starts_with(['❯', '›']);
                    collect_option(
                        &mut options,
                        &mut descriptions,
                        &mut shortcuts,
                        &mut pending_lines,
                        &mut extras,
                        number,
                        label,
                        cursor,
                    );
                    if extras.advance && extras.advance_next.is_none() {
                        extras.advance_next = Some(number);
                    }
                    option_rows.insert(number, i);
                    first = Some(i);
                } else if ADVANCE_ROW_RE.is_match(segment) {
                    // multiSelect `Next`/`Submit` pseudo-row: selectable but
                    // not a numbered option, never description text.
                    extras.advance = true;
                    if segment.trim_start().starts_with(['❯', '›']) {
                        extras.focused = Some(Focused::Advance);
                    }
                } else if segment.trim().is_empty() {
                    continue;
                } else if RULE_RE.is_match(segment) {
                    rule_rows.push(i);
                } else if first.is_some() && (segment.starts_with("     ") || glued) {
                    pending_lines.push(segment.to_string());
                } else if first.is_some() {
                    break 'walk;
                }
            }
        }
        mq = parse_mq_tabs(&lines, first.unwrap_or(end));
        if options.is_empty() || first.is_none() {
            return None;
        }
    } else {
        // No footer. saberante only accepts this when a multi-question tab row
        // is present (the review tab carries no footer); the Devin trust
        // prompt has neither, so the scan also proceeds for screens that end
        // in a bare numbered option block — any content below the options
        // (other than blank/rule lines) rejects the parse.
        let mut pending_lines: Vec<String> = Vec::new();
        let mut i = lines.len();
        'walk: while i > 0 {
            i -= 1;
            let segments = split_glued_rows(lines[i]);
            for (segment_index, segment) in segments.iter().enumerate().rev() {
                let glued = segment_index > 0;
                if let Some((number, label)) = option_match(segment) {
                    let cursor = segment.trim_start().starts_with(['❯', '›']);
                    collect_option(
                        &mut options,
                        &mut descriptions,
                        &mut shortcuts,
                        &mut pending_lines,
                        &mut extras,
                        number,
                        label,
                        cursor,
                    );
                    if extras.advance && extras.advance_next.is_none() {
                        extras.advance_next = Some(number);
                    }
                    option_rows.insert(number, i);
                    first = Some(i);
                } else if ADVANCE_ROW_RE.is_match(segment) {
                    extras.advance = true;
                    if segment.trim_start().starts_with(['❯', '›']) {
                        extras.focused = Some(Focused::Advance);
                    }
                } else if first.is_some() {
                    if (segment.starts_with("     ") || glued) && !segment.trim().is_empty() {
                        pending_lines.push(segment.to_string());
                    } else if RULE_RE.is_match(segment) {
                        rule_rows.push(i);
                    } else if !segment.trim().is_empty() {
                        break 'walk;
                    }
                } else if !segment.trim().is_empty() && !RULE_RE.is_match(segment) {
                    return None;
                } else if RULE_RE.is_match(segment) {
                    rule_rows.push(i);
                }
            }
        }
        if options.is_empty() || first.is_none() {
            return None;
        }
        mq = parse_mq_tabs(&lines, lines.len());
    }

    // A box is a preview only when it actually sits beside a numbered row
    // of the active block selected by the last footer. Historical tables and
    // boxes belonging to an earlier question cannot pass this check.
    if let Some((top, bottom, col, _)) = &preview {
        if !option_rows.values().any(|row| {
            row >= top
                && row <= bottom
                && option_match(&left_of_cell(raw_lines[*row], *col)).is_some()
        }) {
            return None;
        }
    }

    let codex_header = end
        .filter(|end| {
            lines[*end]
                .to_lowercase()
                .contains("enter to submit answer")
        })
        .and(first)
        .and_then(|first| {
            lines[..first]
                .iter()
                .enumerate()
                .rev()
                .find_map(|(row, line)| {
                    CODEX_QUESTION_RE
                        .captures(line)
                        .map(|caps| (row, caps[1].to_string(), line.contains("auto-resolves in")))
                })
        });

    // Free-text slots by label first, then by position. Claude Code draws
    // "Type something." as the last option above a divider with "Chat about
    // this" alone below it; once text is typed the placeholder labels are
    // replaced, so a slot still parses as a numbered option and only its
    // position against that divider identifies it.
    let label_free = options
        .iter()
        .find(|(_, label)| label.starts_with(FREE_TEXT_LABELS[0]))
        .map(|(number, _)| *number);
    let chat_slot: Option<u32> = options
        .iter()
        .filter(|(_, label)| label.starts_with(FREE_TEXT_LABELS[1]))
        .map(|(number, _)| *number)
        .max_by_key(|number| option_rows.get(number).copied().unwrap_or(0))
        .or_else(|| {
            // The label was replaced by typed text: the lone option below
            // the innermost divider inside the option block is the chat
            // slot.
            let divider = rule_rows
                .iter()
                .copied()
                .filter(|row| {
                    option_rows.values().any(|or| *or < *row)
                        && option_rows.values().any(|or| *or > *row)
                })
                .max()?;
            let below: Vec<u32> = option_rows
                .iter()
                .filter(|(_, row)| **row > divider)
                .map(|(number, _)| *number)
                .collect();
            match below.as_slice() {
                [number] => Some(*number),
                _ => None,
            }
        });
    let codex_free = codex_header.as_ref().and_then(|_| {
        options
            .iter()
            .find(|(_, label)| label.starts_with("None of the above"))
            .map(|(number, _)| *number)
    });
    let free = codex_free.or(label_free).or_else(|| {
        // The option directly above — or glued onto — the divider that
        // closes the option block above the chat slot.
        let chat_row = option_rows.get(&chat_slot?).copied()?;
        let divider = rule_rows
            .iter()
            .copied()
            .filter(|row| *row < chat_row)
            .filter(|row| !option_rows.values().any(|or| *row < *or && *or < chat_row))
            .max()?;
        option_rows
            .iter()
            .filter(|(_, row)| **row <= divider)
            .max_by_key(|(_, row)| *row)
            .map(|(number, _)| *number)
    });
    let mut free_like: std::collections::BTreeSet<u32> = options
        .iter()
        .filter(|(_, label)| FREE_TEXT_LABELS.iter().any(|f| label.starts_with(f)))
        .map(|(number, _)| *number)
        .collect();
    free_like.extend(chat_slot);
    free_like.extend(free);
    // Draft text sitting in a free-text slot is never an option: not a
    // choice, not a description, not a shortcut target.
    for number in &free_like {
        descriptions.remove(number);
        shortcuts.remove(number);
    }
    let choices: BTreeMap<u32, String> = options
        .into_iter()
        .filter(|(number, _)| !free_like.contains(number))
        .collect();
    if choices.is_empty() && free.is_none() {
        return None;
    }
    let first = first?;

    // Prompt header: nearest `☐`/`☒` checkbox row above the options.
    let header = codex_header
        .as_ref()
        .map(|(_, label, _)| label.clone())
        .unwrap_or_else(|| {
            lines[..first]
                .iter()
                .rev()
                .map(|line| line.trim())
                .find(|line| line.starts_with(['☐', '☒']))
                .unwrap_or("")
                .to_string()
        });

    // Question text: everything between the checkbox/tab rows and the options,
    // top-down, stopping at agent-message (`⏺`) or rule lines. Dashed
    // permission-box borders are skipped without stopping; the AFK countdown
    // is lifted into `notice` so its ticking digits cannot churn identity.
    let mut question_lines: Vec<&str> = Vec::new();
    let mut notice_lines: Vec<String> = codex_header
        .as_ref()
        .filter(|(_, _, timed)| *timed)
        .map(|_| vec!["auto-resolves in …".to_string()])
        .unwrap_or_default();
    let mut in_afk = false;
    let mut j = first;
    let mut stop_at = 0usize;
    while j > 0 && question_lines.len() < MAX_QUESTION_LINES {
        j -= 1;
        if codex_header.as_ref().is_some_and(|(row, _, _)| j <= *row) {
            break;
        }
        if let Some(mq) = &mq {
            if j <= mq.row {
                break;
            }
        }
        let stripped = lines[j].trim().trim_start_matches('│').trim();
        if RULE_RE.is_match(lines[j]) || stripped.starts_with(['⏺', '❯', '›', '❭']) {
            stop_at = j;
            break;
        }
        if stripped.is_empty() {
            // A blank row inside the question is a paragraph break; keep one
            // marker so the join can restore it.
            if question_lines.first().is_some_and(|line| !line.is_empty()) {
                question_lines.insert(0, "");
            }
            continue;
        }
        if !stripped.starts_with(['☐', '☒', '✔']) {
            if AUTO_DENY_RE.is_match(stripped) {
                // The warning may wrap so its tail word (`session`) lands on
                // its own row below; that fragment was already collected into
                // the question — move it to the notice.
                if question_lines.first().is_some_and(|line| {
                    line.len() <= 24 && line.chars().next().is_some_and(|c| c.is_lowercase())
                }) {
                    let tail = question_lines.remove(0);
                    notice_lines.push(tail.to_string());
                }
                notice_lines.insert(
                    0,
                    AUTO_DENY_TIME_RE
                        .replace_all(stripped, "in M:SS")
                        .to_string(),
                );
                in_afk = true;
                continue;
            }
            // In narrow panes the warning splits into short fragment rows
            // that no single pattern recognises; absorb the contiguous run
            // above a matched row. The raw row is checked — `stripped` has
            // already lost the `│` box prefix that marks real box content.
            if in_afk && is_afk_continuation(lines[j].trim()) {
                notice_lines.insert(
                    0,
                    AUTO_DENY_TIME_RE
                        .replace_all(stripped, "in M:SS")
                        .to_string(),
                );
                continue;
            }
            in_afk = false;
            if DASHED_RULE_RE.is_match(stripped) {
                continue;
            }
            question_lines.insert(0, stripped);
        }
    }
    // Claude Code anchors a permission dialog to a `⏺ Tool(...)` call line
    // above the question box. When the box's top rows are clipped, the
    // command head only survives there; recover it unless the question
    // already carries the same call text.
    // Only permission dialogs ask about a call; an AskUserQuestion (checkbox
    // header or free-text slot) never does, and the `⏺` line of whatever tool
    // ran just before it must not leak into its title — that changed the
    // prompt's identity mid-answer and re-issued an answered question.
    let mut question_lines: Vec<String> = question_lines.into_iter().map(str::to_string).collect();
    let asks_about_call = header.is_empty() && free.is_none() && mq.is_none();
    if let Some(call) = asks_about_call
        .then(|| recover_call_lines(&lines, stop_at))
        .flatten()
    {
        let head = call_head(&call).to_string();
        if !question_lines.iter().any(|line| line.contains(&head)) {
            question_lines.insert(0, call);
        }
    }
    let question = join_wrapped_question(&question_lines);
    let notice = (!notice_lines.is_empty()).then(|| notice_lines.join(" "));

    // The visible preview belongs to the option under the `❯` cursor.
    // A cursor on a free-text/chat slot leaves the previously focused
    // option's box on screen — that box belongs to no visible option, so
    // it attaches to nothing rather than to the slot's draft text.
    let previews = preview
        .as_ref()
        .filter(|(_, _, _, inner)| {
            let text = inner
                .iter()
                .map(|line| line.trim())
                .filter(|line| !line.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            !text.is_empty() && text != "No preview available"
        })
        .and_then(|(_, _, _, inner)| {
            extras
                .focused
                .and_then(|focused| match focused {
                    Focused::Option(number) if choices.contains_key(&number) => Some(number),
                    _ => None,
                })
                .map(|number| (number, inner.join("\n")))
        })
        .into_iter()
        .collect();

    let advance_after = extras.advance.then(|| {
        extras.advance_next.unwrap_or_else(|| {
            choices
                .keys()
                .chain(free.iter())
                .copied()
                .max()
                .unwrap_or(0)
        })
    });
    let preview_none = preview
        .as_ref()
        .filter(|(_, _, _, inner)| {
            inner
                .iter()
                .map(|line| line.trim())
                .filter(|line| !line.is_empty())
                .collect::<Vec<_>>()
                == ["No preview available"]
        })
        .and_then(|_| match extras.focused {
            Some(Focused::Option(number)) if choices.contains_key(&number) => Some(number),
            _ => None,
        })
        .into_iter()
        .collect();
    let mut prompt = ScreenPrompt {
        header,
        mcp_server: None,
        question,
        notice,
        choices,
        descriptions,
        free_text: free,
        shortcuts,
        mq: None,
        multi_select: extras.multi_select,
        checked: extras.checked,
        previews,
        preview_box: preview.is_some(),
        preview_none,
        advance: extras.advance,
        advance_after,
        focused: extras.focused,
        amend: end.is_some_and(|end| footer_offers_amend(&lines[end..])),
        codex_notes_open: codex_header.is_some()
            && end.is_some_and(|end| {
                lines[end]
                    .to_lowercase()
                    .contains("tab or esc to clear notes")
            }),
    };
    if let Some(mq) = mq {
        // Progress + tab name become the header so the review tab (no
        // free-text slot) still counts as a question and the signature
        // differs per tab even when two questions share identical options.
        prompt.header = mq_progress(&mq);
        prompt.mq = Some(mq);
    }
    Some(prompt)
}

/// Continuation rows buffered while walking upward belong to the option row
/// directly above them. They become that option's description — joined back
/// into one sentence — and a shortcut hint riding on a wrapped row (Codex's
/// `(p)` tail) attaches to the same option.
fn attach_option_continuations(
    pending: &mut Vec<String>,
    number: u32,
    descriptions: &mut BTreeMap<u32, String>,
    shortcuts: &mut BTreeMap<u32, OptionShortcut>,
) {
    if pending.is_empty() {
        return;
    }
    for line in pending.iter() {
        if let Some(shortcut) = shortcut_for(line.trim_end()) {
            shortcuts.entry(number).or_insert(shortcut);
        }
    }
    // `pending` collects bottom-up; reverse to restore screen order.
    let description = join_description_lines(pending.drain(..).rev());
    if !description.is_empty() {
        descriptions.insert(number, description);
    }
}

/// Join description rows into one sentence. A break inside CJK text rejoins
/// without inserting a space; ASCII-to-ASCII boundaries get one.
/// Rejoin the question rows Claude Code wrapped to the pane width. Japanese
/// text has no spaces, and a long token (a path, a file name) is broken
/// mid-word when it reaches the edge — joining every row with a space put
/// spaces inside words and file names ("report_neck4ey e.png", "かけ ら").
/// A space goes back only between two ASCII rows where an English word wrap
/// removed it. A row that ended while the next row's first word would still
/// have fitted ended at a line break the agent wrote, and blank rows are
/// paragraph breaks; both stay as `\n` so the question keeps its layout.
fn join_wrapped_question(lines: &[String]) -> String {
    let width = |line: &str| unicode_width::UnicodeWidthStr::width(line);
    let wrap_width = lines.iter().map(|line| width(line)).max().unwrap_or(0);
    let mut out = String::new();
    let mut prev_width = 0;
    let mut prev_wide = false;
    let mut paragraph_break = false;
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            paragraph_break = !out.is_empty();
            continue;
        }
        if paragraph_break {
            // A blank row is a paragraph break the agent wrote.
            out.push_str("\n\n");
            paragraph_break = false;
        } else if let (Some(last), Some(first)) = (out.chars().last(), line.chars().next()) {
            let both_ascii = last.is_ascii() && first.is_ascii();
            // Claude Code wraps at spaces: a space-free run (a Japanese
            // sentence included) moves to the next row whole, and one wider
            // than a row is then broken at the edge. When the next row's
            // first run would have fitted on the upper row, that row ended at
            // a line break the agent wrote: keep it.
            let head = line.split(' ').next().unwrap_or("");
            let gap = usize::from(both_ascii);
            if prev_width + gap + width(head) <= wrap_width {
                out.push('\n');
                out.push_str(line);
                prev_width = width(line);
                prev_wide = !line.is_ascii();
                continue;
            }
            let word_char = |c: char| c.is_ascii_alphanumeric() || "_-./=;:".contains(c);
            // A row ends mid-word only where the renderer had no space to
            // break at: the word is wider than a row, or the row is a run
            // of Japanese text filled to the edge (one cell short when a
            // wide character did not fit). An ordinary English word wrap
            // removed a space, which goes back.
            let tail = out.rsplit(' ').next().unwrap_or("");
            let head = line.split(' ').next().unwrap_or("");
            let long_word = width(tail) + width(head) > wrap_width;
            let full_japanese_row = prev_wide && prev_width + 1 >= wrap_width;
            let broke_mid_word =
                word_char(last) && word_char(first) && (long_word || full_japanese_row);
            if both_ascii && !broke_mid_word {
                out.push(' ');
            }
        }
        out.push_str(line);
        prev_width = width(line);
        prev_wide = !line.is_ascii();
    }
    out
}

fn join_description_lines(lines: impl Iterator<Item = String>) -> String {
    let mut out = String::new();
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if !out.is_empty()
            && out.chars().last().is_some_and(|c| c.is_ascii())
            && line.chars().next().is_some_and(|c| c.is_ascii())
        {
            out.push(' ');
        }
        out.push_str(line);
    }
    out
}

fn mq_progress(mq: &MqInfo) -> String {
    if mq.at_submit {
        "最終確認".to_string()
    } else {
        format!("質問 {}/{} {}", mq.index + 1, mq.total, mq.name)
    }
}

/// Port of saberante's `_parse_mq_tabs`: the `← ☐ q ☐ q ✔ Submit →` row that
/// Claude Code draws for multi-question AskUserQuestion flows. The last tab
/// row above the options wins; `☐` unanswered, `☒` answered, `✔` submit.
fn parse_mq_tabs(lines: &[&str], before: usize) -> Option<MqInfo> {
    let mut found: Option<MqInfo> = None;
    for (row, line) in lines.iter().enumerate().take(before) {
        let Some(caps) = MQ_TAB_ROW_RE.captures(line) else {
            continue;
        };
        let inner = caps.get(1).map(|m| m.as_str()).unwrap_or_default();
        let items: Vec<(char, &str)> = MQ_TAB_ITEM_RE
            .captures_iter(inner)
            .filter_map(|caps| {
                let marker = caps.get(1)?.as_str().chars().next()?;
                let name = caps.get(2)?.as_str().trim();
                Some((marker, name))
            })
            .collect();
        let qtabs: Vec<(char, String)> = items
            .iter()
            .filter(|(marker, name)| matches!(marker, '☐' | '☒') && !name.is_empty())
            .map(|(marker, name)| (*marker, (*name).to_string()))
            .collect();
        if qtabs.is_empty() {
            continue;
        }
        let submit = items.iter().any(|(marker, _)| *marker == '✔');
        let index = qtabs
            .iter()
            .position(|(marker, _)| *marker == '☐')
            .unwrap_or(qtabs.len());
        found = Some(MqInfo {
            index,
            total: qtabs.len(),
            submit,
            at_submit: index >= qtabs.len() && submit,
            name: qtabs
                .get(index)
                .map(|(_, name)| name.clone())
                .unwrap_or_else(|| "Submit".to_string()),
            tabs: qtabs.into_iter().map(|(_, name)| name).collect(),
            row,
        });
    }
    found
}

#[cfg(test)]
mod tests {

    #[test]
    fn historical_mcp_keeps_footerless_numbered_questions_readable() {
        let history = include_str!("../tests/fixtures/screen_prompts/claude_mcp_accept.txt");
        let current = "次の質問です\n\n❯ 1. Yes\n  2. No";
        let expected = super::parse_screen_prompt(current).expect("existing footerless form");
        let actual = super::parse_screen_prompt(&format!("{history}\n{current}"))
            .expect("historical MCP must not hide the bottom block");
        assert_eq!(actual.signature(), expected.signature());
    }

    #[test]
    fn historical_mcp_does_not_hide_the_new_numbered_question() {
        let history = include_str!("../tests/fixtures/screen_prompts/claude_mcp_accept.txt");
        let current = "次の質問です\n\n❯ 1. Yes\n  2. No\n\nEnter to select · Esc to cancel";
        let prompt = super::parse_screen_prompt(&format!("{history}\n{current}")).unwrap();
        assert!(prompt.mcp_server.is_none());
        assert_eq!(prompt.title(), "次の質問です");
        assert!(super::parse_screen_prompt(&format!("{current}\n{history}"))
            .unwrap()
            .mcp_server
            .is_some());
    }

    #[test]
    fn mcp_real_accept_decline_screens_and_identity() {
        let accept = include_str!("../tests/fixtures/screen_prompts/claude_mcp_accept.txt");
        let decline = include_str!("../tests/fixtures/screen_prompts/claude_mcp_decline.txt");
        let a = super::parse_screen_prompt(accept).expect("actual MCP confirmation");
        let d = super::parse_screen_prompt(decline).expect("actual Decline focus");
        assert_eq!(a.title(), "Allow Computer Use to use \"TextEdit\"?");
        assert_eq!(a.mcp_server.as_deref(), Some("codex-cu"));
        assert!(!a.is_question());
        assert_eq!(a.focused, Some(super::Focused::Option(1)));
        assert_eq!(d.focused, Some(super::Focused::Option(2)));
        assert_eq!(a.signature(), d.signature());
        assert_ne!(
            a.signature(),
            super::parse_screen_prompt(&accept.replace("codex-cu", "another-server"))
                .unwrap()
                .signature()
        );
        for screen in [
            accept.replace("❯ Accept    Decline", "Name: [         ]"),
            accept.replace("❯ Accept    Decline", "❯ Accept    Decline    Other"),
            accept.replace("Allow Computer", "Field: [ ]\n  Allow Computer"),
            accept.replace("Esc to cancel · ↑/↓ to navigate", ""),
        ] {
            assert!(super::parse_screen_prompt(&screen).is_none());
        }
    }
    use super::*;

    const DIVIDER: &str = "────────────────────────────────────────────────────────────";

    // Verbatim bottom of a real Claude Code AskUserQuestion pane read
    // (single question, `│`-prefixed context lines, divider-separated
    // "Chat about this" slot).
    const CC_ASK_SINGLE: &str = "\
│ まだ確かめてないこと: 本物の Tailscale での承認待ち（取り込ん
│ だあと店長に試してもらう約束）。サイドバーをたたんだ状態での
│ スクロール。幅の設定をつなぎ直したあとに保てるか。
│
│ 取り込んだら、店長の Mac の gtx（~/.local/bin/gtx）も入れ直す
│ 。画面とサーバーのつなぎ方が変わったから、今動いてる古い gtx
│ のサーバーには新しい画面からつながらない。新しい gtx
│ を使うときは、一度サーバーを立ち上げ直してね（私からは店長の
│ gtx サーバーを止めない）。push はしないよ。

❯ 1. 取り込んで入れ直す（おすすめ）
     main にそのまま続けて取り込んで（fast-forward）、店長の
     Mac の gtx を入れ直す。店長が動かしてる gtx
     のサーバーは止めない。
  2. 取り込むだけ
     main に取り込むだけ。店長の Mac の gtx
     は今のままにしておく。
  3. まだ取り込まない
     店長が自分で触って確かめてから決める。確かめ方は続きで案内
     する。
  4. Type something.
───────────────────────────────────────────────────────────────
  5. Chat about this

Enter to select · ↑/↓ to navigate · Esc to cancel

";

    const CC_ASK_CHECKBOX: &str = "\
☐ これでいい？
1. はい
2. いいえ

Esc to cancel
";

    const CC_PERMISSION: &str = "\
⏺ Bash(git push origin main)
  ⎿  Waiting…

❯ 1. Yes
  2. Yes, and don't ask again for git commands
  3. No, and tell Claude what to do differently (esc)

Esc to cancel · ↑/↓ to navigate · Tab to add context
";

    // Verbatim prompt regions of the real multi-question captures kept in
    // saberante's test_sabera.py (frames-run2).
    fn mq_frame_q1() -> String {
        format!(
            "\
{DIVIDER}
←  ☐ 果物  ☐ 動物  ☐ 色  ✔ Submit  →

複数質問の確認（1つ目）。グラスで「りんご」を選んでください

❯ 1. りんご
     1つ目の答え
  2. みかん
     選ばない
  3. Type something.
{DIVIDER}
  4. Chat about this

Enter to select · Tab/Arrow keys to navigate · Esc to cancel
"
        )
    }

    fn mq_frame_q2() -> String {
        format!(
            "\
{DIVIDER}
←  ☒ 果物  ☐ 動物  ☐ 色  ✔ Submit  →

│ 複数質問の確認（2つ目）。一度「前の質問へ」で戻ってから、ま
│ たここで「いぬ」を選んでください

❯ 1. ねこ
     選ばない
  2. いぬ
     2つ目の答え
  3. Type something.
{DIVIDER}
  4. Chat about this

Enter to select · Tab/Arrow keys to navigate · Esc to cancel
"
        )
    }

    fn mq_frame_q1_back() -> String {
        mq_frame_q1()
            .replace("☐ 果物", "☒ 果物")
            .replace("❯ 1. りんご\n", "❯ 1. りんご ✔\n")
    }

    fn mq_frame_submit() -> String {
        format!(
            "\
{DIVIDER}
←  ☒ 果物  ☒ 動物  ☒ 色  ✔ Submit  →

Review your answers

 ● 複数質問の確認（1つ目）。グラスで「りんご」を選んでくださ
   い
   → りんご
 │ ● 複数質問の確認（2つ目）。一度「前の質問へ」で戻ってから
 │   、またここで「いぬ」を選んでください
   → ねこ
 ● 複数質問の確認（3つ目）。「あお」を選び、最後の確認画面で
   送信してください
   → あお

Ready to submit your answers?

❯ 1. Submit answers
  2. Cancel
"
        )
    }

    #[test]
    fn codex_question_overlay_has_stable_identity_and_free_text() {
        let first = parse_screen_prompt(include_str!(
            "../tests/fixtures/screen_prompt/codex-question.txt"
        ))
        .expect("live Codex question");
        let cursor = parse_screen_prompt(include_str!(
            "../tests/fixtures/screen_prompt/codex-cursor.txt"
        ))
        .expect("live cursor");
        let notes = parse_screen_prompt(include_str!(
            "../tests/fixtures/screen_prompt/codex-notes.txt"
        ))
        .expect("live notes");
        assert!(first.is_question());
        assert!(first.is_codex_question());
        assert!(!first.codex_notes_open);
        assert!(notes.codex_notes_open);
        assert_eq!(first.title(), "検証用コーヒーはどちらにしますか？");
        assert_eq!(
            first.progress().as_deref(),
            Some("Question 1/1 (1 unanswered)")
        );
        assert_eq!(first.free_text, Some(3));
        assert_eq!(first.choices.len(), 2);
        assert_eq!(first.signature(), cursor.signature());
        assert_eq!(first.signature(), notes.signature());
        for screen in [
            include_str!("../tests/fixtures/screen_prompt/codex-countdown-1.txt"),
            include_str!("../tests/fixtures/screen_prompt/codex-countdown-2.txt"),
            include_str!("../tests/fixtures/screen_prompt/codex-countdown-cursor.txt"),
        ] {
            let live = parse_screen_prompt(screen).expect("live countdown");
            assert_eq!(first.signature(), live.signature());
            assert_eq!(first.title(), live.title());
        }
        for timer in ["1m 0s", "59s", "1s"] {
            let screen = include_str!("../tests/fixtures/screen_prompt/codex-question.txt")
                .replace(
                    "Question 1/1 (1 unanswered)",
                    &format!("Question 1/1 (1 unanswered) · auto-resolves in {timer}"),
                );
            let timed = parse_screen_prompt(&screen).expect("timed Codex question");
            assert_eq!(first.signature(), timed.signature());
            assert_eq!(timed.title(), first.title());
            assert_eq!(timed.notice(), Some("auto-resolves in …"));
        }
    }

    // Codex approval overlay (codex-rs TUI snapshot shape): `›` cursor,
    // `(y)`/`(a)`/`(esc)` shortcut hints, lowercase footer.
    const CODEX_PERMISSION: &str = "\
  Would you like to run the following command?

  Reason: clean generated files before publishing

  $ rm -rf target/dist

› 1. Yes, proceed (y)
  2. Yes, and don't ask again for `rm` commands (p)
  3. No, and tell Codex what to do differently (esc)

Press enter to confirm or esc to cancel
";

    // Devin permission prompt, verbatim from src/detect/manifest/tests.rs.
    const DEVIN_PERMISSION: &str = "\
⏺ Running command
  └ $ sleep 30

❭ 1 Yes  (Approve once)
· 2 Yes, allow `sleep` commands
· 3 Yes, always allow `sleep` commands
· 4 No
↑↓ select · ↵ confirm · esc cancel
";

    // Devin workspace trust prompt: no footer at all, options close the screen.
    const DEVIN_TRUST: &str = "\
Do you trust the authors of this directory?
For security, devin should not be run in directories
with untrusted content.
❭ 1 Yes, trust /private/tmp/devin-hook-probe
· 2 No, exit
";

    // Verbatim multiSelect frame from a live Claude Code AskUserQuestion:
    // `[ ]`/`[✔]` checkboxes, the `Next` advance row under the free-text
    // slot, and the multi-question tab strip on top.
    const CC_ASK_MULTISELECT: &str = "\
←  ☐ 機能  ☐ 方式  ✔ Submit  →

入れる機能は？

❯ 1. [✔] 横断検索
         全スペースを検索
  2. [✔] 履歴保存
         セッションを保存
  3. [ ] Type something
     Next
";

    #[test]
    fn swift_preview_scalar_widths_match_reader_for_every_unicode_scalar() {
        let source = include_str!("../apps/mac-dialog-decisions/dialog.swift");
        let regex = regex::Regex::new(r"\(0x([0-9A-F]+), 0x([0-9A-F]+), ([0-9]+)\)")
            .expect("width table regex");
        let ranges: Vec<(u32, u32, usize)> = regex
            .captures_iter(source)
            .map(|caps| {
                (
                    u32::from_str_radix(&caps[1], 16).expect("start"),
                    u32::from_str_radix(&caps[2], 16).expect("end"),
                    caps[3].parse().expect("width"),
                )
            })
            .collect();
        assert!(!ranges.is_empty());
        for value in 0..=0x10ffff {
            if let Some(ch) = char::from_u32(value) {
                let at = ranges.partition_point(|(_, end, _)| *end < value);
                let swift = ranges
                    .get(at)
                    .filter(|(start, _, _)| value >= *start)
                    .map(|(_, _, width)| *width)
                    .unwrap_or(1);
                assert_eq!(
                    swift,
                    unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0),
                    "U+{value:04X}"
                );
            }
        }
    }

    #[test]
    fn real_preview_frames_ignore_transcript_tables_and_keep_wrapped_labels() {
        let table = parse_screen_prompt(include_str!(
            "../tests/fixtures/screen_prompts/claude_preview_table.txt"
        ))
        .expect("table frame");
        assert_eq!(table.choices.len(), 3);
        assert_eq!(table.choices[&1], "案A");
        assert!(table.previews[&2].contains("[店長] --依頼--> [親 Opus]"));
        assert!(!table.previews[&2].contains("作業中"));
        let multi = parse_screen_prompt(include_str!(
            "../tests/fixtures/screen_prompts/claude_preview_multi.txt"
        ))
        .expect("multi frame");
        assert_eq!(multi.mq.as_ref().expect("tabs").total, 2);
        assert!(multi.previews[&1].contains("│ Review │ Pending"));
        assert!(multi.previews[&1].contains("└────────┴"));
        let wrapped = parse_screen_prompt(include_str!(
            "../tests/fixtures/screen_prompts/claude_preview_wrapped.txt"
        ))
        .expect("wrapped frame");
        assert_eq!(wrapped.choices.len(), 3);
        assert_eq!(wrapped.choices[&1], "A. 1アプリにまとめる（推奨）");
        assert!(wrapped.question.contains("どれにする？"));
        assert!(!wrapped.question.contains("1アプリにまとめる"));
        assert!(wrapped.previews[&1].contains("Dock:"));
    }

    #[test]
    fn current_preview_survives_old_boxes_resize_nested_tables_and_hidden_lines() {
        let current = include_str!("../tests/fixtures/screen_prompts/claude_preview_multi.txt");
        let old = include_str!("../tests/fixtures/screen_prompts/claude_preview_table.txt");
        let baseline = parse_screen_prompt(current).expect("baseline");
        for prefix in [
            old.to_string(),
            format!(
                "{old}{}",
                old.replace(
                    "                                  ┌",
                    "                                              ┌"
                )
            ),
        ] {
            let prompt = parse_screen_prompt(&format!("{prefix}{current}")).expect("current");
            assert_eq!(prompt.signature(), baseline.signature());
            assert_eq!(prompt.previews, baseline.previews);
        }
        let hidden = current.replace(
            "│ │ CI     │ Passed   │ GitHub │ 全部緑   │    │",
            "├── ✂ ─ 32 lines hidden ────────────────────────┤",
        );
        let prompt = parse_screen_prompt(&hidden).expect("hidden lines");
        assert!(prompt.previews[&1].contains("32 lines hidden"));
        assert_eq!(prompt.signature(), baseline.signature());
        assert!(!prompt.choices.values().any(|label| label.contains('│')));
        assert!(!prompt
            .descriptions
            .values()
            .any(|label| label.contains('│')));
    }

    #[test]
    fn preview_box_belongs_only_to_current_question_and_none_keeps_format() {
        let old = include_str!("../tests/fixtures/screen_prompts/claude_preview_multi.txt");
        let current = "\n────────────────────\n☐ 新しい質問\n\n次の質問です\n\n❯ 1. はい\n  2. いいえ\n\nEnter to select · Esc to cancel\n";
        let prompt = parse_screen_prompt(&format!("{old}{current}")).expect("new question");
        assert!(!prompt.preview_box);
        assert!(prompt.previews.is_empty());
        assert_eq!(prompt.choices[&1], "はい");
        let absent = "☐ 無し\n\n質問です\n\n❯ 1. A      ┌──────────────────────┐\n  2. B      │ No preview available │\n            └──────────────────────┘\n\nEnter to select · Esc to cancel\n";
        let prompt = parse_screen_prompt(absent).expect("absent preview");
        assert!(prompt.preview_box);
        assert!(prompt.previews.is_empty());
        assert!(prompt.preview_none.contains(&1));
    }

    // Verbatim preview frame: Claude Code draws the focused option's
    // `preview` as a box to the right of the options.
    const CC_ASK_PREVIEW: &str = "\
←  ☐ 機能  ☒ 方式  ✔ Submit  →

どの方式にする？

❯ 1. 方式A                        ┌──────────────────────────────────────────┐
  2. 方式B                        │ ┌─ 方式Aの図 ─┐                          │
                                  │ │ input → A → out │                      │
                                  │ └────────────┘                           │
                                  └──────────────────────────────────────────┘

                                  Notes: press n to add notes

Enter to select · ↑/↓ to navigate · Esc to cancel
";

    #[test]
    fn parses_multiselect_checkboxes_and_advance_row() {
        let prompt = parse_screen_prompt(CC_ASK_MULTISELECT).expect("prompt");
        assert!(prompt.multi_select);
        assert_eq!(prompt.checked, [1, 2].into_iter().collect());
        assert_eq!(prompt.free_text, Some(3));
        assert!(prompt.advance);
        assert_eq!(prompt.advance_after, Some(3));
        assert_eq!(prompt.focused, Some(Focused::Option(1)));
        assert_eq!(
            prompt.choices.values().cloned().collect::<Vec<_>>(),
            vec!["横断検索".to_string(), "履歴保存".to_string()]
        );
        assert_eq!(
            prompt.descriptions.get(&1).map(String::as_str),
            Some("全スペースを検索")
        );
        // The tab strip gives the question label + progress.
        assert_eq!(prompt.progress().as_deref(), Some("質問 1/2 機能"));
        // Checking a different option must not change the signature.
        let toggled = CC_ASK_MULTISELECT.replace("[✔] 横断検索", "[ ] 横断検索");
        assert_eq!(
            parse_screen_prompt(&toggled).unwrap().signature(),
            prompt.signature()
        );
    }

    #[test]
    fn parses_preview_box_beside_focused_option() {
        let prompt = parse_screen_prompt(CC_ASK_PREVIEW).expect("prompt");
        assert!(prompt.preview_box);
        assert_eq!(prompt.focused, Some(Focused::Option(1)));
        let preview = prompt.previews.get(&1).expect("preview for option 1");
        assert!(preview.contains("方式Aの図"), "{preview:?}");
        assert!(preview.contains("input → A → out"), "{preview:?}");
        assert!(!prompt.previews.contains_key(&2));
        // The box is stripped from option rows: labels stay clean.
        assert_eq!(
            prompt.choices.values().cloned().collect::<Vec<_>>(),
            vec!["方式A".to_string(), "方式B".to_string()]
        );
        assert!(prompt.descriptions.is_empty());
    }

    #[test]
    fn parses_claude_code_single_question_with_free_text() {
        let prompt = parse_screen_prompt(CC_ASK_SINGLE).expect("prompt");
        assert!(prompt.is_question());
        assert!(prompt.mq.is_none());
        assert_eq!(prompt.free_text, Some(4));
        assert_eq!(
            prompt.choices.values().cloned().collect::<Vec<_>>(),
            vec![
                "取り込んで入れ直す（おすすめ）".to_string(),
                "取り込むだけ".to_string(),
                "まだ取り込まない".to_string(),
            ]
        );
        assert!(prompt.title().contains("gtx"));
        // Same prompt, cursor moved: identical signature.
        let moved = CC_ASK_SINGLE
            .replacen("❯ 1.", "  1.", 1)
            .replacen("  2.", "❯ 2.", 1);
        assert_eq!(
            parse_screen_prompt(&moved).unwrap().signature(),
            prompt.signature()
        );
    }

    #[test]
    fn parses_option_description_rows() {
        let prompt = parse_screen_prompt(CC_ASK_SINGLE).expect("prompt");
        assert_eq!(
            prompt.descriptions.get(&1).map(String::as_str),
            Some(
                "main にそのまま続けて取り込んで（fast-forward）、店長のMac の gtx \
                 を入れ直す。店長が動かしてる gtxのサーバーは止めない。"
            )
        );
        assert_eq!(
            prompt.descriptions.get(&2).map(String::as_str),
            Some("main に取り込むだけ。店長の Mac の gtxは今のままにしておく。")
        );
        assert_eq!(
            prompt.descriptions.get(&3).map(String::as_str),
            Some("店長が自分で触って確かめてから決める。確かめ方は続きで案内する。")
        );
        // Options without description rows keep none.
        assert!(!prompt.descriptions.contains_key(&4));
    }

    #[test]
    fn option_descriptions_join_soft_wrapped_ascii_with_space() {
        let screen = "\
Do you want to proceed?

❯ 1. Yes
     Proceed and write the
     changes to disk
  2. No

Esc to cancel
";
        let prompt = parse_screen_prompt(screen).expect("prompt");
        assert_eq!(
            prompt.descriptions.get(&1).map(String::as_str),
            Some("Proceed and write the changes to disk")
        );
        assert!(!prompt.descriptions.contains_key(&2));
    }

    #[test]
    fn permission_prompt_option_descriptions_parse() {
        let screen = "\
⏺ Bash(git push origin main)
  ⎿  Waiting…

❯ 1. Yes
     Run the push once
  2. Yes, and don't ask again for git commands
     Push is always fine on this machine
  3. No, and tell Claude what to do differently (esc)

Esc to cancel · ↑/↓ to navigate · Tab to add context
";
        let prompt = parse_screen_prompt(screen).expect("prompt");
        assert!(!prompt.is_question());
        assert_eq!(
            prompt.descriptions.get(&1).map(String::as_str),
            Some("Run the push once")
        );
        assert_eq!(
            prompt.descriptions.get(&2).map(String::as_str),
            Some("Push is always fine on this machine")
        );
        assert!(!prompt.descriptions.contains_key(&3));
        assert_eq!(prompt.shortcuts.get(&3), Some(&OptionShortcut::Escape));
    }

    #[test]
    fn description_only_change_updates_signature() {
        let prompt = parse_screen_prompt(CC_ASK_SINGLE).expect("prompt");
        let changed = CC_ASK_SINGLE.replacen("店長の\n     Mac", "別の\n     Mac", 1);
        let moved = parse_screen_prompt(&changed).expect("changed");
        assert_ne!(prompt.signature(), moved.signature());
        // A moved cursor keeps the same signature even with descriptions.
        let cursor = CC_ASK_SINGLE
            .replacen("❯ 1.", "  1.", 1)
            .replacen("  2.", "❯ 2.", 1);
        assert_eq!(
            parse_screen_prompt(&cursor).unwrap().signature(),
            prompt.signature()
        );
    }

    #[test]
    fn wrap_glued_option_rows_split_back_into_physical_rows() {
        // Claude Code highlights the selected option's rows to the full
        // terminal width; the unwrapped detection view then glues the next
        // physical row on after the padding. Without the split, the label
        // absorbs its first description row and option 2 disappears into
        // option 1's description.
        let screen = "\
 ☐ デプロイ先

どの環境にデプロイしますか？

❯ 1. 本番環境                                                        本番環境へ直ちにデプロイし、すべての監視アラートを有効化し
     たうえで、問題が起きたときにすぐ前の版へ戻せるようロールバ
     ック手順も事前に準備しておきます                           ❯ 2. ステージング環境
     本番相当の設定で動作確認するためにステージングへデプロイし
     ます
  3. 開発環境
     開発者向けの検証用環境へデプロイします
  4. Type something.                                            ────────────────
  5. Chat about this

Enter to select · ↑/↓ to navigate · Esc to cancel
";
        let prompt = parse_screen_prompt(screen).expect("prompt");
        assert_eq!(
            prompt.choices,
            BTreeMap::from([
                (1, "本番環境".to_string()),
                (2, "ステージング環境".to_string()),
                (3, "開発環境".to_string()),
            ])
        );
        assert_eq!(prompt.free_text, Some(4));
        assert_eq!(
            prompt.descriptions.get(&1).map(String::as_str),
            Some(
                "本番環境へ直ちにデプロイし、すべての監視アラートを有効化したうえで、問題が起きたときにすぐ前の版へ戻せるようロールバック手順も事前に準備しておきます"
            )
        );
        assert_eq!(
            prompt.descriptions.get(&2).map(String::as_str),
            Some("本番相当の設定で動作確認するためにステージングへデプロイします")
        );
        assert_eq!(
            prompt.descriptions.get(&3).map(String::as_str),
            Some("開発者向けの検証用環境へデプロイします")
        );
    }

    #[test]
    fn wrapped_option_shortcut_attaches_to_the_option_above() {
        let screen = "\
  Would you like to run the following command?

› 1. Yes, proceed (y)
  2. Yes, and don't ask again for `rm`
     commands (p)
  3. No, and tell Codex what to do differently (esc)

Press enter to confirm or esc to cancel
";
        let prompt = parse_screen_prompt(screen).expect("prompt");
        assert_eq!(prompt.shortcuts.get(&1), Some(&OptionShortcut::Char('y')));
        // The `(p)` hint on option 2's wrapped second line belongs to option 2.
        assert_eq!(prompt.shortcuts.get(&2), Some(&OptionShortcut::Char('p')));
        assert_eq!(prompt.shortcuts.get(&3), Some(&OptionShortcut::Escape));
        assert_eq!(
            prompt.descriptions.get(&2).map(String::as_str),
            Some("commands (p)")
        );
    }

    #[test]
    fn parses_claude_code_checkbox_question() {
        let prompt = parse_screen_prompt(CC_ASK_CHECKBOX).expect("prompt");
        assert_eq!(prompt.header, "☐ これでいい？");
        assert!(prompt.is_question());
        assert_eq!(prompt.title(), "これでいい？");
        assert_eq!(prompt.choices.len(), 2);
        assert!(prompt.descriptions.is_empty());
    }

    #[test]
    fn parses_claude_code_permission_as_tool_permission() {
        let prompt = parse_screen_prompt(CC_PERMISSION).expect("prompt");
        assert!(!prompt.is_question());
        assert_eq!(prompt.choices.len(), 3);
        assert_eq!(prompt.shortcuts.get(&3), Some(&OptionShortcut::Escape));
    }

    #[test]
    fn parses_multi_question_tabs_with_progress() {
        let p1 = parse_screen_prompt(&mq_frame_q1()).expect("q1");
        assert_eq!(p1.header, "質問 1/3 果物");
        assert_eq!(
            p1.question,
            "複数質問の確認（1つ目）。グラスで「りんご」を選んでください"
        );
        assert!(!p1.question.contains('☐') && !p1.question.contains("Submit"));
        assert_eq!(p1.choices.len(), 2);
        assert_eq!(p1.free_text, Some(3));
        let mq = p1.mq.as_ref().unwrap();
        assert_eq!((mq.index, mq.total, mq.submit), (0, 3, true));

        let p2 = parse_screen_prompt(&mq_frame_q2()).expect("q2");
        assert_eq!(p2.header, "質問 2/3 動物");
        assert_eq!(p2.mq.as_ref().unwrap().index, 1);
        assert_ne!(p1.signature(), p2.signature());

        let back = parse_screen_prompt(&mq_frame_q1_back()).expect("back");
        assert_eq!(back.mq.as_ref().unwrap().index, 1);
        assert_eq!(back.choices.get(&1).map(String::as_str), Some("りんご ✔"));

        let submit = parse_screen_prompt(&mq_frame_submit()).expect("submit");
        assert_eq!(submit.header, "最終確認");
        assert!(submit.mq.as_ref().unwrap().at_submit);
        assert_eq!(
            submit.choices.values().cloned().collect::<Vec<_>>(),
            vec!["Submit answers".to_string(), "Cancel".to_string()]
        );
        // Cursor on the second option: still the same prompt.
        let cursor2 = mq_frame_submit().replace(
            "❯ 1. Submit answers\n  2. Cancel",
            "  1. Submit answers\n❯ 2. Cancel",
        );
        assert_eq!(
            parse_screen_prompt(&cursor2).unwrap().signature(),
            submit.signature()
        );
    }

    #[test]
    fn multi_question_typing_in_free_text_slot_parses() {
        // The churn report: typing into "Type something" replaces its
        // placeholder label, and each keystroke used to mint a new
        // option — a new signature, a new decision. The slot is the
        // option directly above the divider that closes the box, so
        // draft text is still free text, never an option.
        let idle = parse_screen_prompt(&mq_frame_q1()).expect("idle");
        for typed in ["❯ 3. そ", "  3. それは君が判断できるでしょ"] {
            let screen = mq_frame_q1().replace("  3. Type something.", typed);
            let prompt = parse_screen_prompt(&screen).expect("typing");
            assert_eq!(prompt.free_text, Some(3), "{typed:?}");
            assert!(!prompt.choices.contains_key(&3));
            assert!(!prompt
                .choices
                .values()
                .any(|label| label.contains('そ') || label.contains("それは")));
            assert_eq!(prompt.choices, idle.choices);
            assert_eq!(prompt.signature(), idle.signature());
            assert!(!prompt.descriptions.contains_key(&3));
        }
        // The label heuristic still wins when the slot is untouched.
        assert_eq!(idle.free_text, Some(3));
    }

    #[test]
    fn typing_in_chat_about_this_slot_keeps_it_out_of_options() {
        // "Chat about this" typed into drops its placeholder too: the
        // lone option below the divider inside the block is still the
        // chat slot, not a new choice.
        let typing = mq_frame_q1().replace("  4. Chat about this", "  4. これについて話そう");
        let prompt = parse_screen_prompt(&typing).expect("typing");
        assert_eq!(prompt.free_text, Some(3));
        assert!(!prompt.choices.contains_key(&4));
        assert_eq!(prompt.choices.len(), 2);
    }

    #[test]
    fn free_text_draft_preview_attaches_to_no_option() {
        // The preview box left behind while the cursor sits on the
        // free-text slot belongs to an option the walk did not see —
        // it must not land on the slot's draft text.
        let screen = format!(
            "\
{DIVIDER}
←  ☐ 果物  ✔ Submit  →

どの果物？

  1. りんご                        ┌──────────────┐
  2. みかん                        │ 図: 果物の箱  │
❯ 3. これは下書き                  └──────────────┘
{DIVIDER}
  4. Chat about this

Enter to select · ↑/↓ to navigate · Esc to cancel
"
        );
        let prompt = parse_screen_prompt(&screen).expect("prompt");
        assert_eq!(prompt.free_text, Some(3));
        assert!(!prompt.choices.contains_key(&3));
        assert!(prompt.previews.is_empty());
        assert!(prompt.preview_box);
    }

    #[test]
    fn parses_codex_permission_with_letter_shortcuts() {
        let prompt = parse_screen_prompt(CODEX_PERMISSION).expect("prompt");
        assert!(!prompt.is_question());
        assert_eq!(prompt.choices.len(), 3);
        assert_eq!(prompt.shortcuts.get(&1), Some(&OptionShortcut::Char('y')));
        assert_eq!(prompt.shortcuts.get(&2), Some(&OptionShortcut::Char('p')));
        assert_eq!(prompt.shortcuts.get(&3), Some(&OptionShortcut::Escape));
    }

    #[test]
    fn parses_devin_permission_and_trust() {
        let prompt = parse_screen_prompt(DEVIN_PERMISSION).expect("permission");
        assert!(!prompt.is_question());
        assert_eq!(prompt.choices.len(), 4);
        assert_eq!(
            prompt.choices.get(&1).map(String::as_str),
            Some("Yes  (Approve once)")
        );
        assert_eq!(prompt.choices.get(&4).map(String::as_str), Some("No"));

        let trust = parse_screen_prompt(DEVIN_TRUST).expect("trust");
        assert!(!trust.is_question());
        assert_eq!(trust.choices.len(), 2);
        assert!(trust
            .choices
            .get(&1)
            .is_some_and(|label| label.starts_with("Yes, trust")));
        assert_eq!(
            trust.title(),
            "Do you trust the authors of this directory?\nFor security, devin should not be run in directories with untrusted content."
        );
    }

    // Real Claude Code 2.1.296 AFK permission dialog (auto mode, dangerous
    // `rm` on a variable path) — captured from a live pane. The `in M:SS`
    // countdown ticks once per second.
    fn afk_permission(countdown: &str) -> String {
        format!(
            "\
⏺ Bash(B=/tmp/hdr-churn/bench; for r in light-nano-C mini-C; do printf \"%s\\n\" \"$r\"; /bin/rm -rf $B/$r/frames; done)
  ⎿  Waiting…

──────────────────────────────────────────────────────
 Bash command
 Delete frames directories for two bench runs
╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
 │ B=/tmp/hdr-churn/bench; for r in light-nano-C mini-C; do printf \"%s\\n\" \"$r\"; /bin/rm -rf $B/$r/frames; done
╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
 │ Dangerous rm operation on possibly-empty variable path: $B/$r/frames in `/bin/rm -rf $B/$r/frames` (rewrite it as \"${{B:?}}\"/\"${{r:?}}\"/frames or use a literal path)
 ⚠ Claude Code will automatically deny this request
 in {countdown}, to avoid blocking progress on an unattended
 session


 Do you want to proceed?
 ❯ 1. Yes
   2. No

 Esc to cancel · Tab to amend
"
        )
    }

    #[test]
    fn tab_to_amend_footer_marks_the_prompt() {
        let prompt = parse_screen_prompt(&afk_permission("1:48")).expect("prompt");
        assert!(prompt.amend);
        assert!(!prompt.is_question());
        // The hint wrapped onto the next row still counts.
        let wrapped =
            parse_screen_prompt(&afk_permission("1:48").replace("Tab to amend", "Tab to\n amend"))
                .expect("wrapped");
        assert!(wrapped.amend);
        // Older Claude Code and Codex footers offer no note.
        assert!(!parse_screen_prompt(CODEX_PERMISSION).expect("codex").amend);
        // While the note is open the footer drops the hint.
        let open = afk_permission("1:48").replace("Esc to cancel · Tab to amend", "Esc to cancel");
        assert!(!parse_screen_prompt(&open).expect("open").amend);
    }

    #[test]
    fn afk_countdown_lifts_notice_and_holds_signature() {
        let first = parse_screen_prompt(&afk_permission("1:48")).expect("first");
        let later = parse_screen_prompt(&afk_permission("0:03")).expect("later");
        // The ticking countdown must not manufacture a new prompt.
        assert_eq!(first.signature(), later.signature());
        assert!(!first.question.contains("automatically deny"));
        assert_eq!(
            first.notice(),
            Some(
                "⚠ Claude Code will automatically deny this request in M:SS, \
                 to avoid blocking progress on an unattended session"
            )
        );
        // Full command + its description + the warning survive in the title.
        assert!(first.title().contains("Bash command"));
        assert!(first
            .title()
            .contains("Delete frames directories for two bench runs"));
        assert!(first.title().contains(
            "B=/tmp/hdr-churn/bench; for r in light-nano-C mini-C; \
             do printf \"%s\\n\" \"$r\"; /bin/rm -rf $B/$r/frames; done"
        ));
        assert!(first.title().contains("Dangerous rm operation"));
        assert!(first.title().ends_with("Do you want to proceed?"));
        // Dashed box borders stay out of the title.
        assert!(!first.title().contains('╌'));
        assert!(!first.title().contains('│'));
    }

    // Same AFK dialog in a narrow (~30 col) pane — the warning splits into
    // six fragment rows.
    fn afk_permission_narrow(countdown: &str) -> String {
        format!(
            "\
 Bash command
 Delete frames dirs for two
 bench runs
 ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
 │ B=/tmp/hdr-churn/bench; for
 │ r in light-nano-C mini-C;
 │ do printf \"%s\\n\" \"$r\";
 │ /bin/rm -rf $B/$r/frames;
 │ done
 ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
 │ Dangerous rm operation on
 │ possibly-empty variable
 │ path: $B/$r/frames
 ⚠ Claude Code will
 automatically deny
 this request in {countdown},
 to avoid blocking
 progress on an
 unattended session


 Do you want to proceed?
 ❯ 1. Yes
   2. No

 Esc to cancel · Tab to amend
"
        )
    }

    #[test]
    fn afk_countdown_narrow_wrap_lifts_all_fragments() {
        let prompt = parse_screen_prompt(&afk_permission_narrow("1:46")).expect("prompt");
        assert_eq!(
            prompt.notice(),
            Some(
                "⚠ Claude Code will automatically deny this request in M:SS, \
                 to avoid blocking progress on an unattended session"
            )
        );
        assert!(!prompt.question.contains("automatically deny"));
        assert!(!prompt.question.contains("unattended"));
        // The command is fully preserved across box wraps.
        assert!(prompt.title().contains("/bin/rm -rf $B/$r/frames; done"));
        assert!(prompt.title().contains("Dangerous rm operation"));
        // And the ticking digits still cannot churn identity.
        let later = parse_screen_prompt(&afk_permission_narrow("0:59")).expect("later");
        assert_eq!(prompt.signature(), later.signature());
    }

    #[test]
    fn clipped_permission_box_recovers_call_head() {
        // The `│` box top is clipped (scrolled/dialog taller than rows): the
        // command head only survives on the `⏺` call line.
        let screen = "\
⏺ Bash(B=/tmp/hdr-churn/bench; for r in light-nano-C mini-C; do printf \"%s\\n\" \"$r\"; /bin/rm -rf $B/$r/frames; done)
  ⎿  Waiting…

──────────────────────────────────────────────────────
 Bash command
 Delete frames directories for two bench runs
╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
 │ light-nano-C mini-C; do printf \"%s\\n\" \"$r\"; /bin/rm -rf $B/$r/frames; done
╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌

 Do you want to proceed?
 ❯ 1. Yes
   2. No

 Esc to cancel
";
        let prompt = parse_screen_prompt(screen).expect("prompt");
        assert!(
            prompt
                .title()
                .contains("Bash(B=/tmp/hdr-churn/bench; for r in light-nano-C mini-C"),
            "call head missing: {}",
            prompt.title()
        );
        // And the question does not start mid-command anymore.
        assert!(prompt.title().starts_with("Bash("));
    }

    /// Verbatim Claude Code 2.1.300 frame (54-column pane) of a question
    /// written with line breaks and blank lines.
    const CC_MULTILINE_QUESTION: &str = "\
 ☐ 改行

│ gtx で tb (terminal-browser)
│ が本当に動くかを確かめますか？ 今の状態: gtx
│ には画像を外の端末へ中継する仕組みが入っています。
│ ① 右に案の欄がある形の質問で止まる件。
│
│ ② 選択肢を選んだまま自由入力に書いても送られない件:
│ 原因は herdr 側だったの。ダイアログは両方送ってたけ
│ ど、ペインに打ち込むときに文を捨ててた。
│
│ ③ short line
│ This is an English sentence that should wrap across
│ several rows of the pane nicely.

❯ 1. はい
  2. いいえ
  3. Type something.
──────────────────────────────────────────────────────
  4. Chat about this

Enter to select · ↑/↓ to navigate · Esc to cancel
";

    #[test]
    fn question_keeps_line_breaks_the_agent_wrote() {
        let prompt = parse_screen_prompt(CC_MULTILINE_QUESTION).expect("prompt");
        let title = prompt.title();
        // Paragraph breaks (blank rows) and a short row followed by a word
        // that would have fitted stay as line breaks.
        assert!(
            title.contains("① 右に案の欄がある形の質問で止まる件。\n\n② 選択肢"),
            "{title:?}"
        );
        assert!(
            title.contains("文を捨ててた。\n\n③ short line\nThis is an English"),
            "{title:?}"
        );
        // Rows the renderer wrapped still join back into one line.
        assert!(
            title.contains("gtx で tb (terminal-browser)が本当に動くか"),
            "{title:?}"
        );
        assert!(title.contains("送ってたけど、ペインに"), "{title:?}");
        assert!(
            title.contains("should wrap across several rows"),
            "{title:?}"
        );
    }

    #[test]
    fn wrapped_question_rejoins_without_spaces_inside_words() {
        // Claude Code hard-wraps a long Japanese question at the pane edge,
        // even in the middle of a file name or path.
        let text = "役者01の直し（ほおの角ばった影とこめかみの赤茶色のかけらを消す）が反映されたよ。画像は work/neck-fix/report_neck4eye.png と work/neckfix-vrm-render-front.png（新しい顔）。進んでいい？";
        let mut rows = Vec::new();
        let mut row = String::new();
        for ch in text.chars() {
            let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if unicode_width::UnicodeWidthStr::width(row.as_str()) + w > 40 {
                rows.push(std::mem::take(&mut row));
            }
            row.push(ch);
        }
        rows.push(row);
        let screen = format!(
            " ☐ 進め方\n\n{}\n\n ❯ 1. 進む\n   2. 止める\n   3. Type something.\n───────────────────────────────────────────────────────────────\n   4. Chat about this\n\n Enter to select · ↑/↓ to navigate · Esc to cancel\n",
            rows.iter().map(|r| format!(" {r}")).collect::<Vec<_>>().join("\n")
        );
        assert!(rows.len() >= 4, "{rows:?}");
        let title = parse_screen_prompt(&screen).expect("prompt").title();
        // Words and file names split across rows come back whole. (A space
        // the renderer trimmed at a row start between a Latin word and
        // Japanese cannot be recovered and is not asserted.)
        for whole in [
            "こめかみの赤茶色のかけらを消す",
            "画像は work/neck-fix/report_neck4eye.png",
            "work/neckfix-vrm-render-front.png（新しい顔）",
        ] {
            assert!(
                title.contains(whole),
                "missing {whole} in {title}; rows: {rows:?}"
            );
        }
        // A short row ending a word keeps its space to the next English word.
        let english = "\
 ☐ Plan

 Should we refresh the cache before the deploy starts and
 keeps everything consistent?

 ❯ 1. Yes
   2. No
   3. Type something.
───────────────────────────────────────────────────────────────
   4. Chat about this

 Enter to select · ↑/↓ to navigate · Esc to cancel
";
        let title = parse_screen_prompt(english).expect("prompt").title();
        assert_eq!(
            title,
            "Should we refresh the cache before the deploy starts and keeps everything consistent?"
        );
    }

    #[test]
    fn question_below_a_tool_call_keeps_its_own_title() {
        // A Bash call ran right before AskUserQuestion; its `⏺` line sits a
        // few rows above the question. The question must not borrow it.
        let screen = "\
⏺ Bash(H=/tmp/x/restart-hub.sh; sed -i '' 's/job-1/job-2/' $H; bash $H)
  ⎿  {\"id\":null}
     job-17919 slack-decisions

───────────────────────────────────────────────────────────────
 ☐ Tailscale承認

 仕事 Mac の承認ページを開いて承認してほしいな

 ❯ 1. 承認した
      続きで仕事 Mac と Mac mini を差し替える
   2. 今はできない
      仕事 Mac を飛ばす
   3. Type something.
───────────────────────────────────────────────────────────────
   4. Chat about this

 Enter to select · ↑/↓ to navigate · Esc to cancel
";
        let prompt = parse_screen_prompt(screen).expect("prompt");
        assert!(prompt.is_question());
        assert!(
            !prompt.title().contains("Bash("),
            "tool call leaked into the question: {}",
            prompt.title()
        );
        assert!(prompt.title().contains("承認ページを開いて"));
    }

    #[test]
    fn unclipped_permission_box_does_not_duplicate_call() {
        let prompt = parse_screen_prompt(&afk_permission("1:48")).expect("prompt");
        // The box already carries the full command — the `⏺` line must not
        // be prepended as a duplicate `Bash(` call.
        assert!(
            !prompt.question.contains("Bash("),
            "call duplicated: {}",
            prompt.question
        );
    }

    #[test]
    fn volatile_displays_do_not_change_signature() {
        let screen = |timer: &str, spinner: char| {
            format!(
                "\
{spinner} 作業中 ({timer}経過中) — 続行しますか？

❯ 1. Yes
  2. No

Esc to cancel
"
            )
        };
        let a = parse_screen_prompt(&screen("0:42", '✢')).expect("a");
        let b = parse_screen_prompt(&screen("1:09", '◐')).expect("b");
        assert_eq!(a.signature(), b.signature());
        // Content differences still change the signature.
        let other =
            parse_screen_prompt(&screen("0:42", '✢').replace("続行しますか？", "停止しますか？"))
                .expect("other");
        assert_ne!(a.signature(), other.signature());
    }

    #[test]
    fn non_prompt_screens_return_none() {
        assert_eq!(
            parse_screen_prompt(
                "⏺ User answered Claude's questions:\n  ⎿  · question → answer\n\n❯\n"
            ),
            None
        );
        assert_eq!(
            parse_screen_prompt("compiling half the world\nthis may take a while\n"),
            None
        );
        // Options followed by more content are not a waiting prompt.
        assert_eq!(
            parse_screen_prompt("1. Yes\n2. No\n\nthe user picked 2\n❯ \n"),
            None
        );
        assert_eq!(parse_screen_prompt(""), None);
    }
    // Verbatim unwrapped capture of the real narrow-pane AFK dialog:
    // box rows beyond the pane height survive in scrollback, and the title
    // must still carry the whole command + warning + question.
    #[test]
    fn narrow_pane_permission_keeps_full_command_in_title() {
        let screen = "\
────────────────────────
 Bash command
 Delete frames
 directories for two
 bench runs
╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
 │ B=/tmp/hdr-churn/ben
 │ ch; for r in
 │ light-nano-C mini-C;
 │ do printf \"%s\\n\"
 │ \"$r\"; /bin/rm -rf
 │ $B/$r/frames; done
╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
 │ Dangerous rm
 │ operation on
 │ possibly-empty
 │ variable path:
 │ $B/$r/frames in
 │ `/bin/rm -rf
 │ $B/$r/frames`
 │ (rewrite it as \"${B:
 │ ?}\"/\"${r:?}\"/frames
 │ or use a literal
 │ path)
 ⚠ Claude Code will
 automatically deny
 this request in 0:08,
 to avoid blocking
 progress on an
 unattended session


 Do you want to
 proceed?
 ❯ 1. Yes
   2. No

 Esc to cancel · Tab to
 amend
";
        let p = parse_screen_prompt(screen).expect("prompt");
        let title = p.title();
        assert!(title.contains("Bash command"));
        assert!(title.contains("Delete frames directories for two bench runs"));
        assert!(title.contains(
            "B=/tmp/hdr-churn/bench; for r in light-nano-C mini-C; \
             do printf \"%s\\n\" \"$r\"; /bin/rm -rf $B/$r/frames; done"
        ));
        assert!(title.contains("Dangerous rm operation on possibly-empty"));
        assert!(title.contains("or use a literal path)\n\nDo you want to proceed?"));
        assert!(!title.contains("automatically deny"));
        assert!(!title.contains("unattended"));
        assert_eq!(
            p.notice(),
            Some(
                "\u{26a0} Claude Code will automatically deny this request in M:SS, \
                 to avoid blocking progress on an unattended session"
            )
        );
    }
}
