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
static MQ_TAB_ROW_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*←\s+(.*?)\s*→\s*$").unwrap());
static MQ_TAB_ITEM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"([☐☒✔])\s*([^☐☒✔←→]+)").unwrap());
// Codex-style per-option shortcut hints: "… (y)", "… (a)", "… (esc)".
static SHORTCUT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\(([a-zA-Z0-9]|esc|ESC|Esc)\)\s*$").unwrap());

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScreenPrompt {
    /// Prompt header: the `☐`/`☒` checkbox line above the options, or the
    /// multi-question progress label ("質問 n/N <tab>", "最終確認").
    pub header: String,
    /// Question/prompt text collected above the options.
    pub question: String,
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
}

impl ScreenPrompt {
    /// saberante's `is_question`: checkbox header or a free-text slot marks a
    /// question worth a decision; bare numbered menus are confirmations.
    pub fn is_question(&self) -> bool {
        !self.header.is_empty() || self.free_text.is_some()
    }

    /// saberante's `prompt_key`: identity of the visible prompt so the same
    /// screen does not produce a second decision and tab moves are detected.
    /// The question text is part of the identity: permission prompts that
    /// share options ("Yes / don't ask again / No") are still different
    /// prompts when they ask about different commands.
    pub fn signature(&self) -> String {
        serde_json::to_string(&(
            self.header.as_str(),
            self.question.as_str(),
            &self.choices,
            &self.descriptions,
            self.free_text,
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
        self.mq.as_ref().map(|mq| {
            if mq.at_submit {
                "最終確認".to_string()
            } else {
                format!("質問 {}/{} {}", mq.index + 1, mq.total, mq.name)
            }
        })
    }
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

/// Register a numbered option and attach the continuation rows that were
/// buffered below it while walking upward.
fn collect_option(
    options: &mut BTreeMap<u32, String>,
    descriptions: &mut BTreeMap<u32, String>,
    shortcuts: &mut BTreeMap<u32, OptionShortcut>,
    pending_lines: &mut Vec<String>,
    number: u32,
    label: String,
) {
    if let Some(shortcut) = shortcut_for(&label) {
        shortcuts.insert(number, shortcut);
    } else {
        shortcuts.remove(&number);
    }
    attach_option_continuations(pending_lines, number, descriptions, shortcuts);
    options.insert(number, label);
}

/// Port of saberante's `parse_prompt`. Returns `None` when the screen holds no
/// numbered option block anchored by a prompt footer or by the screen bottom.
pub(crate) fn parse_screen_prompt(screen: &str) -> Option<ScreenPrompt> {
    let lines: Vec<&str> = screen.lines().collect();
    // saberante anchors on the LAST footer line so a stale prompt in
    // scrollback cannot shadow the active one.
    let end = lines.iter().rposition(|line| PROMPT_END_RE.is_match(line));
    let mut options: BTreeMap<u32, String> = BTreeMap::new();
    let mut descriptions: BTreeMap<u32, String> = BTreeMap::new();
    let mut shortcuts: BTreeMap<u32, OptionShortcut> = BTreeMap::new();
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
                    collect_option(
                        &mut options,
                        &mut descriptions,
                        &mut shortcuts,
                        &mut pending_lines,
                        number,
                        label,
                    );
                    first = Some(i);
                } else if segment.trim().is_empty() || RULE_RE.is_match(segment) {
                    continue;
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
                    collect_option(
                        &mut options,
                        &mut descriptions,
                        &mut shortcuts,
                        &mut pending_lines,
                        number,
                        label,
                    );
                    first = Some(i);
                } else if first.is_some() {
                    if (segment.starts_with("     ") || glued) && !segment.trim().is_empty() {
                        pending_lines.push(segment.to_string());
                    } else if !segment.trim().is_empty() && !RULE_RE.is_match(segment) {
                        break 'walk;
                    }
                } else if !segment.trim().is_empty() && !RULE_RE.is_match(segment) {
                    return None;
                }
            }
        }
        if options.is_empty() || first.is_none() {
            return None;
        }
        mq = parse_mq_tabs(&lines, lines.len());
    }

    let free = options
        .iter()
        .find(|(_, label)| label.starts_with(FREE_TEXT_LABELS[0]))
        .map(|(number, _)| *number);
    let choices: BTreeMap<u32, String> = options
        .into_iter()
        .filter(|(_, label)| !FREE_TEXT_LABELS.iter().any(|f| label.starts_with(f)))
        .collect();
    if choices.is_empty() && free.is_none() {
        return None;
    }
    let first = first?;

    // Prompt header: nearest `☐`/`☒` checkbox row above the options.
    let header = lines[..first]
        .iter()
        .rev()
        .map(|line| line.trim())
        .find(|line| line.starts_with(['☐', '☒']))
        .unwrap_or("")
        .to_string();

    // Question text: everything between the checkbox/tab rows and the options,
    // top-down, stopping at agent-message (`⏺`) or rule lines.
    let mut question_lines: Vec<&str> = Vec::new();
    let mut j = first;
    while j > 0 && question_lines.len() < MAX_QUESTION_LINES {
        j -= 1;
        if let Some(mq) = &mq {
            if j <= mq.row {
                break;
            }
        }
        let stripped = lines[j].trim().trim_start_matches('│').trim();
        if RULE_RE.is_match(lines[j]) || stripped.starts_with(['⏺', '❯', '›', '❭']) {
            break;
        }
        if !stripped.is_empty() && !stripped.starts_with(['☐', '☒', '✔']) {
            question_lines.insert(0, stripped);
        }
    }
    let question = question_lines.join(" ");

    let mut prompt = ScreenPrompt {
        header,
        question,
        choices,
        descriptions,
        free_text: free,
        shortcuts,
        mq: None,
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
        let typing = mq_frame_q1().replace("  3. Type something.", "❯ 3. これ");
        let prompt = parse_screen_prompt(&typing).expect("typing");
        assert_eq!(prompt.free_text, None);
        assert_eq!(prompt.choices.get(&3).map(String::as_str), Some("これ"));
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
            "Do you trust the authors of this directory? For security, devin should not be run in directories with untrusted content."
        );
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
}
