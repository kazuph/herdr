//! Enforce-mode helpers for `herdr guard exec`.
//!
//! When `[guard] mode = "enforce"` (the default) an `ask` verdict becomes a
//! `command_guard` decision wait on the pane's own server, and a `block`
//! verdict stops the command. This module holds the pure pieces: the stderr
//! wording (mirroring ai_guard), the request-reason validation, and the
//! decision title/body/options construction.

use std::path::Path;

use crate::api::schema::{DecisionOption, DecisionOptionRole};

/// Option id for the single-execution approval choice.
pub(crate) const OPTION_APPROVE: &str = "approve";
/// Option id for the day-scoped approval choice (`1日許可`).
pub(crate) const OPTION_APPROVE_DAY: &str = "approve_day";
/// Option id for the rejection choice.
pub(crate) const OPTION_REJECT: &str = "reject";

/// ai_guard `AI_GUARD_REQUEST_ENV_NAME`: env var that names the request
/// reason variable.
pub(crate) const REQUEST_REASON_ENV_NAME_VAR: &str = "AI_GUARD_REQUEST_ENV_NAME";
/// Default request-reason variable name (ai_guard `AI_GUARD_REQUEST_REASON`).
pub(crate) const DEFAULT_REQUEST_REASON_ENV: &str = "AI_GUARD_REQUEST_REASON";
/// ai_guard `AI_GUARD_TEMP_APPROVAL_SECONDS` default (seconds).
pub(crate) const DEFAULT_TEMP_APPROVAL_SECONDS: i64 = 180;
/// Env var overriding the temp-approval TTL, mirroring ai_guard.
pub(crate) const TEMP_APPROVAL_SECONDS_ENV: &str = "AI_GUARD_TEMP_APPROVAL_SECONDS";

/// The choices every `command_guard` decision wait offers, in display order.
pub(crate) fn decision_options() -> Vec<DecisionOption> {
    vec![
        DecisionOption {
            id: OPTION_APPROVE.to_string(),
            label: "承認".to_string(),
            role: DecisionOptionRole::Approve,
            description: None,
        },
        DecisionOption {
            id: OPTION_APPROVE_DAY.to_string(),
            label: "1日許可".to_string(),
            role: DecisionOptionRole::Approve,
            description: None,
        },
        DecisionOption {
            id: OPTION_REJECT.to_string(),
            label: "却下".to_string(),
            role: DecisionOptionRole::Reject,
            description: None,
        },
    ]
}

/// Name of the env var the request reason is read from, honoring
/// `AI_GUARD_REQUEST_ENV_NAME` exactly like ai_guard's `${(P)...}` indirection.
pub(crate) fn request_reason_env_name() -> String {
    std::env::var(REQUEST_REASON_ENV_NAME_VAR)
        .ok()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_REQUEST_REASON_ENV.to_string())
}

/// Normalize a request reason the way ai_guard does (`\n`/`\t` -> space).
/// Returns `None` when nothing but whitespace remains.
pub(crate) fn validated_request_reason(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .chars()
        .map(|ch| if ch == '\n' || ch == '\t' { ' ' } else { ch })
        .collect();
    let trimmed = cleaned.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// ai_guard's Japanese check `[ぁ-んァ-ヶ一-龠々〆〤ー]`: a request reason must
/// contain at least one Japanese character so it is a real explanation.
pub(crate) fn contains_japanese(text: &str) -> bool {
    text.chars().any(|ch| {
        ('\u{3041}'..='\u{3093}').contains(&ch)
            || ('\u{30A1}'..='\u{30F6}').contains(&ch)
            || ('\u{4E00}'..='\u{9FA0}').contains(&ch)
            || matches!(ch, '\u{3005}' | '\u{3006}' | '\u{3024}' | '\u{30FC}')
    })
}

/// Stderr shown when an `ask` command runs without a request reason.
/// Mirrors ai_guard's missing-request-text rejection.
pub(crate) fn missing_reason_message(cmd_line: &str, env_name: &str) -> String {
    format!(
        "❌ Command cancelled: {cmd_line}\n\
           この guard メッセージはユーザー本人の指示です。\n\
           理由: 要承認コマンドには、対象プロジェクト・必要な理由・実施内容を説明する申請文が必要です。\n\
           例: {env_name}='対象プロジェクト: <名称>。必要な理由: <解決したい課題>。実施内容: <行う変更や操作>。確認済み: <確認内容>。戻し方: <戻す手順>。' <command>\n\
           英語の申請文は禁止です。\n"
    )
}

/// Stderr shown when the request reason contains no Japanese characters.
/// Mirrors ai_guard's non-Japanese rejection.
pub(crate) fn non_japanese_reason_message(cmd_line: &str, env_name: &str) -> String {
    format!(
        "❌ Command cancelled: {cmd_line}\n\
           この guard メッセージはユーザー本人の指示です。\n\
           理由: 要承認コマンドの申請文は日本語で書いてください。英語の申請文は禁止です。\n\
           例: {env_name}='なぜ必要か、何を確認済みか、戻し方は何かを日本語で書く' <command>\n"
    )
}

/// Stderr shown for a `block` verdict. Mirrors ai_guard's block output: the
/// command, the rule reason, and that approval cannot unlock it.
pub(crate) fn block_message(rule: &str, reason: &str, cmd_line: &str) -> String {
    if rule == "herdr-wildcard-send" {
        return format!(
            "❌ ブロック（承認不可）: {cmd_line}\n\
               この guard メッセージはユーザー本人の指示です。\n\
               理由: herdr の宛先 '*' は --room を付けても room 内に限定されず、別workspace・別project を含む全agentへ一斉配信されます（誤爆事故の原因）。\n\
               対処: 宛先は herdr pane current で確定した親pane ID、または検証済みの単一pane/agent名を指定してください。複数宛先が必要でも検証済みIDを有限個列挙して個別送信します。\n\
               禁止: 別の書き方（--room の位置変更、agent send / msg send への切替、command 経由、絶対パス実行）で再試行しないでください。宛先を確定できないときは送信せず、親paneへ「宛先不明」と報告してください。\n\
               注意: これは承認では解除できません。ユーザーがワイルドカード配信を本当に必要とする場合はユーザー自身が実行します。\n"
        );
    }
    format!(
        "❌ ブロック（承認不可）: {cmd_line}\n\
           この guard メッセージはユーザー本人の指示です。\n\
           理由: {reason}\n\
           注意: これは承認では解除できません。必要な場合はユーザー自身が実行してください。\n"
    )
}

/// Stderr shown when the command does not run because the decision came back
/// negative (rejected, expired, cancelled, or text-only). `reason` is the
/// free-text answer when one was given.
pub(crate) fn deny_message(cmd_line: &str, reason: &str) -> String {
    format!(
        "❌ Command cancelled: {cmd_line}\n\
           この guard メッセージはユーザー本人の指示です。\n\
           理由: {reason}\n"
    )
}

/// Stderr shown when an approval lets the command run (ai_guard's `✅ 承認`).
pub(crate) fn approved_message(cmd_line: &str, note: &str) -> String {
    format!("✅ 承認: {cmd_line} (理由: {note})\n")
}

/// ai_guard's `cwd_short`: the last two path components (`parent/leaf`),
/// `~` when the cwd is the user's home, `/` for the root.
pub(crate) fn cwd_short(cwd: &Path, home: Option<&Path>) -> String {
    if home.is_some_and(|home| home == cwd) {
        return "~".to_string();
    }
    let file_name = cwd
        .file_name()
        .map(|name| name.to_string_lossy().to_string());
    match (cwd.parent().and_then(Path::file_name), file_name) {
        (Some(parent), Some(leaf)) => format!("{}/{leaf}", parent.to_string_lossy()),
        (None, Some(leaf)) => leaf,
        _ => cwd.to_string_lossy().to_string(),
    }
}

/// Decision title: `<command> @ <cwd末尾2階層>` like ai_guard's dialog title
/// (`<cmd> @ <cwd_short>`); the agent label prefixes it the way ai_guard
/// prefixes the caller process.
pub(crate) fn decision_title(cmd: &str, cwd: &Path, home: Option<&Path>) -> String {
    format!("{cmd} @ {}", cwd_short(cwd, home))
}

/// Decision body: request reason first, then the full command, cwd, branch
/// and agent, matching the fields ai_guard puts in its dialog/Slack detail.
pub(crate) fn decision_body(
    request_reason: &str,
    cmd_line: &str,
    cwd: &Path,
    branch: Option<&str>,
    agent: &str,
) -> String {
    let mut body = format!(
        "申請文:\n{request_reason}\n\nコマンド: {cmd_line}\n作業ディレクトリ: {}",
        cwd.to_string_lossy()
    );
    if let Some(branch) = branch {
        body.push_str(&format!("\nブランチ: {branch}"));
    }
    body.push_str(&format!("\nエージェント: {agent}"));
    body
}

/// Temp-approval TTL in seconds: `AI_GUARD_TEMP_APPROVAL_SECONDS` when it
/// parses as a positive integer, else ai_guard's 180-second default.
pub(crate) fn temp_approval_seconds() -> i64 {
    std::env::var(TEMP_APPROVAL_SECONDS_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<i64>().ok())
        .filter(|seconds| *seconds > 0)
        .unwrap_or(DEFAULT_TEMP_APPROVAL_SECONDS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_reason_validation_normalizes_and_rejects_blank() {
        assert_eq!(
            validated_request_reason("  対象: x。\n実施: y。\t").as_deref(),
            Some("対象: x。 実施: y。")
        );
        assert!(validated_request_reason("   \n\t  ").is_none());
        assert!(validated_request_reason("").is_none());
    }

    #[test]
    fn japanese_detection_matches_ai_guard_ranges() {
        assert!(contains_japanese("ひらがな"));
        assert!(contains_japanese("カタカナ"));
        assert!(contains_japanese("日本語"));
        assert!(contains_japanese("aーb"));
        assert!(contains_japanese("a々b"));
        assert!(!contains_japanese("english only"));
        assert!(!contains_japanese("12345"));
    }

    #[test]
    fn block_message_includes_reason_and_no_override_hint() {
        let message = block_message(
            "git-push-main",
            "main への push は禁止",
            "git push origin main",
        );
        assert!(message.contains("❌ ブロック（承認不可）: git push origin main"));
        assert!(message.contains("理由: main への push は禁止"));
        assert!(message.contains("承認では解除できません"));
    }

    #[test]
    fn wildcard_send_block_uses_the_full_ai_guard_wording() {
        let message = block_message("herdr-wildcard-send", "", "herdr send * hi");
        assert!(message.contains("宛先は herdr pane current で確定した親pane ID"));
        assert!(message.contains("別の書き方"));
        assert!(message.contains("承認では解除できません"));
    }

    #[test]
    fn deny_and_approved_messages_carry_the_reason() {
        let deny = deny_message("rm -rf ./build", "やめて");
        assert!(deny.contains("❌ Command cancelled: rm -rf ./build"));
        assert!(deny.contains("理由: やめて"));
        let ok = approved_message("rm -rf ./build", "1日許可");
        assert!(ok.contains("✅ 承認: rm -rf ./build (理由: 1日許可)"));
    }

    #[test]
    fn cwd_short_uses_last_two_components() {
        assert_eq!(
            cwd_short(Path::new("/Users/x/projects/myapp"), None),
            "projects/myapp"
        );
        assert_eq!(cwd_short(Path::new("/"), None), "/");
        assert_eq!(
            cwd_short(Path::new("/Users/x"), Some(Path::new("/Users/x"))),
            "~"
        );
        assert_eq!(cwd_short(Path::new("/tmp"), None), "tmp");
    }

    #[test]
    fn decision_title_and_body_carry_the_context() {
        let title = decision_title("rm", Path::new("/a/b/c"), None);
        assert_eq!(title, "rm @ b/c");
        let body = decision_body(
            "対象: ビルド。理由: 再生成。",
            "rm -rf ./build",
            Path::new("/a/b/c"),
            Some("feat/x"),
            "codex",
        );
        assert!(body.contains("申請文:\n対象: ビルド。理由: 再生成。"));
        assert!(body.contains("コマンド: rm -rf ./build"));
        assert!(body.contains("作業ディレクトリ: /a/b/c"));
        assert!(body.contains("ブランチ: feat/x"));
        assert!(body.contains("エージェント: codex"));
        let no_branch = decision_body("r", "rm x", Path::new("/a"), None, "claude");
        assert!(!no_branch.contains("ブランチ:"));
    }

    #[test]
    fn decision_options_offer_approve_day_and_reject() {
        let options = decision_options();
        assert_eq!(
            options.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(),
            vec![OPTION_APPROVE, OPTION_APPROVE_DAY, OPTION_REJECT]
        );
        assert_eq!(options[0].role, DecisionOptionRole::Approve);
        assert_eq!(options[1].role, DecisionOptionRole::Approve);
        assert_eq!(options[2].role, DecisionOptionRole::Reject);
    }
}
