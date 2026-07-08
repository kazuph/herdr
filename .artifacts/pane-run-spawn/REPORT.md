# herdr run spawn レビュー

Created: 2026-07-08
Branch: feature/pane-run-spawn
Status: Claude検分待ち

## 元の依頼

`herdr run` を実装する依頼です。要件は、呼び出し元paneと同一spaceに新paneを即起動し、指定コマンドを実行し、exit時に呼び出し元paneへ完了通知1行を注入し、CLI自体はspawn後すぐreturnすることでした。

作業ルールは、`feature/msg-mailbox` 基点で `feature/pane-run-spawn` を作ること、dogfoodのargv0レイヤーを混ぜないこと、commit/pushしないこと、caller解決はfail closedにすること、長時間コマンドはHerdr通知runner経由にすること、節目を `p_239` へ報告することでした。

## 実装内容

- `herdr run [--label TEXT] [--cwd PATH] [--split right|down] [--caller <pane>] [--close-on-success] -- <command...>` を追加しました。
- callerは `--caller` 明示時に `pane.get` で検証し、省略時は既存の `pane current` と同じ `HERDR_PANE_ID -> process session -> ppid chain` の解決を使います。解決不能時は fail closed で終了し、`--caller <pane>` を案内します。
- 新paneは `pane.split` でcallerを基準に作ります。`workspace_id` は渡さず、既存APIの挙動どおりcallerと同じworkspace/tabに作られます。
- spawn後は `{"pane":"p_N","job":"job-...","label":"..."}` を出力して即returnします。
- 実行は既存の `__pane-notify-run` / job-log 基盤を再利用します。
- exit時はcaller paneへ `[herdr run] exit=0 label=demo pane=p_2 詳細: herdr pane job-log job-...` 形式の1行を `pane.send_input` で注入します。
- `--close-on-success` 指定時は exit 0 の場合だけ新paneを `pane.close` します。

## 検証

- `cargo check --all-targets`: pass
- `cargo test run_`: pass。`run_notification_line`、shell quote、default label のunitを確認。
- `cargo test --test root_help`: pass
- `cargo test`: pass。main binary `1242 passed`、integration suitesもpass。macOSでは `tests/cli_wrapper.rs` は `cfg(not(target_os = "macos"))` のため0本ですが、Linux CI向けに `herdr_run_*` integrationを追加済みです。
- `cargo clippy --all-targets --all-features -- -D warnings`: pass
- `cargo build --release`: pass

## E2E証跡

isolated config/runtime/state で `target/release/herdr server` を起動し、実バイナリで確認しました。

- `run-output.json`: `herdr run --caller 1-1 --label demo --cwd <tmp> --split down -- sh -c 'sleep 1; echo done-from-herdr-run'` が `pane=p_2` と `job=job-1783463157772-37191` を即返却。
- `spawned-pane.json`: 新pane `p_2` の `tab_id` がcallerと同じtabで、labelが `demo`、cwdが指定tmp dir。
- `wait-spawned.json` / `spawned-read.txt`: 新paneで `done-from-herdr-run` が出力されたことを確認。
- `wait-caller-notice.json` / `caller-read.txt`: caller paneに `[herdr run] exit=0 label=demo pane=p_2 詳細: herdr pane job-log job-1783463157772-37191` が注入されたことを確認。
- `job-log.txt`: stdoutと `exit_code: 0` を確認。
- `close-run-output.json` / `close-pane-get.err`: `--close-on-success -- true` で作成された `p_3` が成功後に `pane_not_found` になることを確認。
- `status-after-stop.txt`: isolated server停止後 `status: not running` を確認。

## 既知の制約

- サーバー再起動を跨いだjob完了通知は保証しません。job-log自体はrunnerが書きますが、runnerからAPI socketへ戻す通知は稼働中serverに依存します。
- shell paneをcallerにした場合、注入された通知行はshell入力としてsubmitされるため、shellはその文字列をコマンドとして解釈します。Codex/Claudeなどのagent paneではcomposerへの割り込みとして機能します。
- commit/push、shared checkout fast-forward、本番 `~/.local/bin/herdr` の差し替え、running Herdr restartは未実施です。
