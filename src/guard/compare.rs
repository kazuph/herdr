//! `herdr guard compare`: match herdr guard records against ai_guard log
//! entries for the same command and report agreement.
//!
//! Matching is done on the normalized command text (whitespace-collapsed
//! `cmd_line`). When several ai_guard entries share the command, the entry
//! closest in time to the herdr record wins; `--window` additionally rejects
//! matches farther apart than the given seconds.

use std::collections::BTreeMap;
use std::path::Path;

use crate::guard::judge::Verdict;
use crate::guard::record::{
    ai_guard_action_verdict, read_ai_guard_log, AiGuardEntry, StoredRecord,
};

fn normalize_cmd(cmd: &str) -> String {
    cmd.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn verdict_str(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Allow => "allow",
        Verdict::Ask => "ask",
        Verdict::Block => "block",
    }
}

#[derive(Debug)]
enum MatchStatus {
    Agree,
    /// herdr judged it differently from ai_guard.
    Diverge,
    /// No ai_guard entry for this command.
    NoAiGuard,
    /// The nearest ai_guard entry is outside `--window`.
    OutsideWindow,
    /// ai_guard saw the command but herdr did not record it.
    AiGuardOnly,
}

struct Row {
    status: MatchStatus,
    herdr: Option<(Verdict, String)>,
    ai_guard: Option<(String, String)>,
    cmd: String,
}

pub(crate) fn run(
    guard_log: &Path,
    ai_guard_logs: &[std::path::PathBuf],
    window_secs: Option<i64>,
    since_epoch: Option<i64>,
) -> i32 {
    let herdr_records: Vec<StoredRecord> = std::fs::read_to_string(guard_log)
        .map(|content| {
            content
                .lines()
                .filter_map(|line| serde_json::from_str::<StoredRecord>(line).ok())
                .collect()
        })
        .unwrap_or_default();
    let mut ai_entries: Vec<AiGuardEntry> = Vec::new();
    for path in ai_guard_logs {
        ai_entries.extend(read_ai_guard_log(path));
    }

    let herdr_records: Vec<&StoredRecord> = herdr_records
        .iter()
        .filter(|rec| since_epoch.is_none_or(|since| rec.ts_epoch >= since))
        .collect();
    let ai_entries: Vec<&AiGuardEntry> = ai_entries
        .iter()
        .filter(|entry| since_epoch.is_none_or(|since| entry.ts_epoch.is_none_or(|ts| ts >= since)))
        .collect();

    // Index ai_guard entries by normalized command.
    let mut ai_by_cmd: BTreeMap<String, Vec<&AiGuardEntry>> = BTreeMap::new();
    for entry in &ai_entries {
        ai_by_cmd
            .entry(normalize_cmd(&entry.cmd_line))
            .or_default()
            .push(entry);
    }

    let mut rows: Vec<Row> = Vec::new();
    let mut matched_ai: Vec<bool> = vec![false; ai_entries.len()];

    for rec in &herdr_records {
        let key = normalize_cmd(&rec.cmd_line);
        let Some(candidates) = ai_by_cmd.get(&key) else {
            rows.push(Row {
                status: MatchStatus::NoAiGuard,
                herdr: Some((rec.verdict, rec.rule.clone())),
                ai_guard: None,
                cmd: rec.cmd_line.clone(),
            });
            continue;
        };
        // Nearest in time wins.
        let best = candidates
            .iter()
            .enumerate()
            .filter_map(|(idx, entry)| entry.ts_epoch.map(|ts| (idx, (ts - rec.ts_epoch).abs())))
            .min_by_key(|(_, dist)| *dist)
            .map(|(idx, _)| idx);
        let Some(best_idx) = best else {
            rows.push(Row {
                status: MatchStatus::NoAiGuard,
                herdr: Some((rec.verdict, rec.rule.clone())),
                ai_guard: None,
                cmd: rec.cmd_line.clone(),
            });
            continue;
        };
        let entry = candidates[best_idx];
        let dist = entry
            .ts_epoch
            .map(|ts| (ts - rec.ts_epoch).abs())
            .unwrap_or(i64::MAX);
        if window_secs.is_some_and(|w| dist > w) {
            rows.push(Row {
                status: MatchStatus::OutsideWindow,
                herdr: Some((rec.verdict, rec.rule.clone())),
                ai_guard: Some((entry.action.clone(), String::new())),
                cmd: rec.cmd_line.clone(),
            });
            continue;
        }
        if let Some(pos) = ai_entries.iter().position(|e| std::ptr::eq(*e, entry)) {
            matched_ai[pos] = true;
        }
        let ai_verdict = ai_guard_action_verdict(&entry.action);
        let herdr_verdict = verdict_str(rec.verdict);
        // A suppressed ask (`allow` + suppressed_by) behaves like ai_guard's
        // silent run after an approved prompt: count it as `ask` agreement.
        let status = match ai_verdict {
            Some(v) if v == herdr_verdict => MatchStatus::Agree,
            Some(_) => MatchStatus::Diverge,
            None => MatchStatus::NoAiGuard,
        };
        rows.push(Row {
            status,
            herdr: Some((rec.verdict, rec.rule.clone())),
            ai_guard: Some((entry.action.clone(), String::new())),
            cmd: rec.cmd_line.clone(),
        });
    }

    // ai_guard entries never matched against a herdr record.
    for (idx, entry) in ai_entries.iter().enumerate() {
        if matched_ai[idx] || since_epoch.is_some() && entry.ts_epoch.is_none() {
            continue;
        }
        let key = normalize_cmd(&entry.cmd_line);
        if herdr_records
            .iter()
            .any(|rec| normalize_cmd(&rec.cmd_line) == key)
        {
            continue; // command matched but the specific entry didn't pair
        }
        rows.push(Row {
            status: MatchStatus::AiGuardOnly,
            herdr: None,
            ai_guard: Some((entry.action.clone(), String::new())),
            cmd: entry.cmd_line.clone(),
        });
    }

    print_report(&rows)
}

fn print_report(rows: &[Row]) -> i32 {
    let mut agree = 0usize;
    let mut diverge = 0usize;
    let mut herdr_only = 0usize;
    let mut outside = 0usize;
    let mut ai_only = 0usize;

    for row in rows {
        match row.status {
            MatchStatus::Agree => {
                agree += 1;
                println!(
                    "  AGREE    {}  (herdr={} ai_guard={})",
                    row.cmd,
                    row.herdr
                        .as_ref()
                        .map(|(v, _)| verdict_str(*v))
                        .unwrap_or("-"),
                    row.ai_guard
                        .as_ref()
                        .map(|(a, _)| a.as_str())
                        .unwrap_or("-"),
                );
            }
            MatchStatus::Diverge => {
                diverge += 1;
                println!(
                    "  DIVERGE  {}  (herdr={}:{} ai_guard={})",
                    row.cmd,
                    row.herdr
                        .as_ref()
                        .map(|(v, _)| verdict_str(*v))
                        .unwrap_or("-"),
                    row.herdr.as_ref().map(|(_, r)| r.as_str()).unwrap_or("-"),
                    row.ai_guard
                        .as_ref()
                        .map(|(a, _)| a.as_str())
                        .unwrap_or("-"),
                );
            }
            MatchStatus::NoAiGuard => {
                herdr_only += 1;
                println!(
                    "  HERDR-ONLY  {}  (herdr={}:{})",
                    row.cmd,
                    row.herdr
                        .as_ref()
                        .map(|(v, _)| verdict_str(*v))
                        .unwrap_or("-"),
                    row.herdr.as_ref().map(|(_, r)| r.as_str()).unwrap_or("-"),
                );
            }
            MatchStatus::OutsideWindow => {
                outside += 1;
                println!("  OUT-OF-WINDOW  {}", row.cmd);
            }
            MatchStatus::AiGuardOnly => {
                ai_only += 1;
                println!(
                    "  AI-GUARD-ONLY  {}  (ai_guard={})",
                    row.cmd,
                    row.ai_guard
                        .as_ref()
                        .map(|(a, _)| a.as_str())
                        .unwrap_or("-"),
                );
            }
        }
    }

    println!();
    println!(
        "summary: agree={agree} diverge={diverge} herdr-only={herdr_only} \
         ai-guard-only={ai_only} out-of-window={outside}"
    );
    if diverge > 0 {
        1
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(status: MatchStatus) -> Row {
        Row {
            status,
            herdr: Some((Verdict::Ask, "rule".to_string())),
            ai_guard: Some(("REJECT".to_string(), "ctx".to_string())),
            cmd: "git push".to_string(),
        }
    }

    #[test]
    fn print_report_exits_nonzero_only_on_divergence() {
        assert_eq!(print_report(&[row(MatchStatus::Diverge)]), 1);
        assert_eq!(
            print_report(&[
                row(MatchStatus::Agree),
                row(MatchStatus::NoAiGuard),
                row(MatchStatus::AiGuardOnly),
                row(MatchStatus::OutsideWindow),
            ]),
            0
        );
    }
}
