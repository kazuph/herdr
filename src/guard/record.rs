//! JSONL decision record written by `herdr guard exec`/`guard line`.
//!
//! One line per judged command. The file is append-only; a record describes
//! the ai_guard verdict (`allow`/`ask`/`block`) without affecting execution —
//! in this phase the command always runs.

use std::io::Write;
use std::path::Path;

use serde::Serialize;

use crate::guard::judge::{Judgment, Verdict};

/// How the command reached the guard.
#[derive(Debug, Clone, Copy, Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Via {
    /// `herdr guard exec` invoked by a PATH shim.
    Shim,
    /// `herdr guard exec` invoked by a shell function (`rm() { ... }`) or the
    /// `command` passthrough.
    ShellFunc,
    /// `herdr guard line` invoked by the zsh accept-line hook for typed input.
    Line,
    /// `herdr guard check` invoked manually for testing.
    Check,
}

impl Via {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Via::Shim => "shim",
            Via::ShellFunc => "shell-func",
            Via::Line => "line",
            Via::Check => "check",
        }
    }
}

/// One judgment record. Field names are stable for `guard compare` and any
/// future decision plumbing.
#[derive(Debug, Serialize)]
pub(crate) struct DecisionRecord {
    /// Seconds since UNIX epoch.
    pub ts_epoch: i64,
    /// RFC 3339 UTC timestamp, e.g. `2026-10-12T03:45:00Z`.
    pub ts: String,
    pub verdict: Verdict,
    /// Stable rule id, e.g. `git-push-main`, `rm-paths`, `danger-word`.
    pub rule: String,
    /// Human-readable reason (mirrors the ai_guard wording). Empty for allow.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub reason: String,
    /// Full argv as executed.
    pub cmd: Vec<String>,
    /// `cmd` joined with single spaces (ai_guard log-compatible form).
    pub cmd_line: String,
    pub cwd: String,
    /// Agent launch dir used by the rm path rules.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launch_dir: Option<String>,
    /// `HERDR_PANE_ID` env id (`p_<n>`) when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    /// Agent label from the pane marker.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    pub via: Via,
    /// ai_guard temp-approval key this command maps to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_key: Option<String>,
    /// Set when verdict is `allow` only because a valid approval suppressed an
    /// `ask`; holds the matched approval key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suppressed_by: Option<String>,
}

impl DecisionRecord {
    pub(crate) fn from_judgment(
        judgment: &Judgment,
        argv: &[String],
        cwd: &Path,
        launch_dir: Option<&Path>,
        pane_id: Option<String>,
        workspace_id: Option<String>,
        tab_id: Option<String>,
        agent: Option<String>,
        via: Via,
        now: i64,
    ) -> Self {
        Self {
            ts_epoch: now,
            ts: rfc3339_utc(now),
            verdict: judgment.verdict,
            rule: judgment.rule.to_string(),
            reason: judgment.reason.clone(),
            cmd: argv.to_vec(),
            cmd_line: argv.join(" "),
            cwd: cwd.to_string_lossy().to_string(),
            launch_dir: launch_dir.map(|p| p.to_string_lossy().to_string()),
            pane_id,
            workspace_id,
            tab_id,
            agent,
            via,
            approval_key: judgment.approval_key.clone(),
            suppressed_by: judgment.suppressed_by.clone(),
        }
    }
}

/// Append one JSONL record. Best-effort: failures are swallowed because a
/// guard that cannot log must still run the wrapped command.
pub(crate) fn append(path: &Path, record: &DecisionRecord) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    else {
        return;
    };
    if let Ok(line) = serde_json::to_string(record) {
        let _ = writeln!(file, "{line}");
    }
}

pub(crate) fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// Minimal RFC 3339 (UTC) formatting — the crate has no datetime dependency.
fn rfc3339_utc(epoch: i64) -> String {
    let days = epoch.div_euclid(86_400);
    let secs = epoch.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

/// Days since epoch -> (year, month, day), civil-from-days algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

/// Parse an ai_guard log timestamp (`2026-10-11T10:27:31+09:00`) to epoch.
/// Returns `None` for malformed input; `guard compare` treats such entries as
/// unmatchable by time (still matched by command).
pub(crate) fn parse_iso8601_epoch(ts: &str) -> Option<i64> {
    let bytes = ts.as_bytes();
    if bytes.len() < 19 {
        return None;
    }
    let year: i64 = ts.get(0..4)?.parse().ok()?;
    let month: i64 = ts.get(5..7)?.parse().ok()?;
    let day: i64 = ts.get(8..10)?.parse().ok()?;
    let hour: i64 = ts.get(11..13)?.parse().ok()?;
    let min: i64 = ts.get(14..16)?.parse().ok()?;
    let sec: i64 = ts.get(17..19)?.parse().ok()?;
    // UTC offset: `+HH:MM`, `-HH:MM`, or `Z`.
    let rest = ts.get(19..).unwrap_or("");
    let (sign, off_h, off_m) = if rest.starts_with('Z') || rest.is_empty() {
        (0i64, 0i64, 0i64)
    } else {
        let sign = if rest.starts_with('-') { -1 } else { 1 };
        let off_h: i64 = rest.get(1..3)?.parse().ok()?;
        let off_m: i64 = rest.get(4..6).unwrap_or("00").parse().ok()?;
        (sign, off_h, off_m)
    };
    let naive =
        days_from_civil(year, month as u32, day as u32) * 86_400 + hour * 3600 + min * 60 + sec;
    Some(naive - sign * (off_h * 3600 + off_m * 60))
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// One parsed ai_guard log entry (`<ts>\t<VERDICT>\t<cmd>\t<context>`).
#[derive(Debug, Clone)]
pub(crate) struct AiGuardEntry {
    pub ts_epoch: Option<i64>,
    /// Raw ai_guard action: ALLOW / REJECT / BLOCK / DAY_ALLOW /
    /// PROMPT_SHOWN / BLOCKED_PROTECTED / BLOCKED_CHECKOUT_B /
    /// BLOCKED_GIT_WT_LIST / ...
    pub action: String,
    /// Raw command text as logged.
    pub cmd_line: String,
}

/// Parse one ai_guard log line. Lines that do not look like entries (blank,
/// banner) return `None`.
pub(crate) fn parse_ai_guard_line(line: &str) -> Option<AiGuardEntry> {
    let mut parts = line.splitn(4, '\t');
    let ts = parts.next()?;
    let action = parts.next()?;
    let cmd_line = parts.next().unwrap_or("");
    if ts.len() < 15 || action.is_empty() || cmd_line.is_empty() {
        return None;
    }
    Some(AiGuardEntry {
        ts_epoch: parse_iso8601_epoch(ts),
        action: action.to_string(),
        cmd_line: cmd_line.to_string(),
    })
}

/// Map an ai_guard action string to the coarse verdict the guard produces.
pub(crate) fn ai_guard_action_verdict(action: &str) -> Option<&'static str> {
    match action {
        // Commands that ran without a prompt.
        "ALLOW" => Some("allow"),
        // ai_guard showed a confirmation request (user rejected = REJECT,
        // approved = ALLOW, day-approval granted = DAY_ALLOW; all of them
        // correspond to our `ask` classification).
        "REJECT" | "DAY_ALLOW" | "PROMPT_SHOWN" => Some("ask"),
        "BLOCK" | "BLOCKED_PROTECTED" | "BLOCKED_CHECKOUT_B" | "BLOCKED_GIT_WT_LIST" => {
            Some("block")
        }
        _ => None,
    }
}

/// Read a whole ai_guard-format log file (missing file -> empty vec).
pub(crate) fn read_ai_guard_log(path: &Path) -> Vec<AiGuardEntry> {
    std::fs::read_to_string(path)
        .map(|content| {
            content
                .lines()
                .filter_map(parse_ai_guard_line)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

/// Parse a herdr guard JSONL line back (for `guard compare`/`guard log`).
#[derive(Debug, serde::Deserialize)]
pub(crate) struct StoredRecord {
    pub ts_epoch: i64,
    pub verdict: Verdict,
    pub rule: String,
    #[serde(default)]
    pub cmd_line: String,
    #[serde(default)]
    pub pane_id: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub via: Option<Via>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_formats_utc() {
        assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_utc(1_780_000_000), "2026-05-28T20:26:40Z");
    }

    #[test]
    fn parses_ai_guard_timestamp_with_offset() {
        // 2026-10-11T10:27:31+09:00 == 01:27:31 UTC
        let epoch = parse_iso8601_epoch("2026-10-11T10:27:31+09:00").unwrap();
        let z = parse_iso8601_epoch("2026-10-11T01:27:31Z").unwrap();
        assert_eq!(epoch, z);
    }

    #[test]
    fn parses_ai_guard_log_line() {
        let entry = parse_ai_guard_line(
            "2026-10-11T10:27:31+09:00\tREJECT\trmdir /tmp/x\t申請文なし [cwd:/x]",
        )
        .unwrap();
        assert_eq!(entry.action, "REJECT");
        assert_eq!(entry.cmd_line, "rmdir /tmp/x");
    }

    #[test]
    fn ai_guard_action_mapping() {
        assert_eq!(ai_guard_action_verdict("ALLOW"), Some("allow"));
        assert_eq!(ai_guard_action_verdict("REJECT"), Some("ask"));
        assert_eq!(ai_guard_action_verdict("BLOCK"), Some("block"));
        assert_eq!(ai_guard_action_verdict("BLOCKED_PROTECTED"), Some("block"));
    }
}
