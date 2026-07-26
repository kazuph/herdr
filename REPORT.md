# Codex・Claude復元とpane通知の回帰を修正

> 「修正して」
>
> 「claudeが立ち上がっていた部分でまったく復元されませんでした。必ず復元させて。」
>
> 「それぞれのpaneの右上にsaved sessionという表記がでなくなってます。」
>
> 「ゴミなので一旦これが全く止まらない、飛ばない状況まで戻してほしい」

## 修正前

- Codex restore commandをinteractive zshへ入れると、先頭の`codex` aliasが通常起動オプションを再追加し、`--sandbox`などが二重指定された。
- Claude restoreは`[agent_start.commands].claude`の環境変数・permission・thinking設定を継承しなかった。
- Windows cmd向けcommand生成はcmdメタ文字をliteral argvとして保持できなかった。
- pane borderから`saved session`表示が失われていた。
- 通常roomの古いmailbox nudgeが、再利用されたagent名を解決して無関係paneへ注入された。

## 修正後

- Unixは`command 'codex' ...`でalias/function展開を避ける。Windows PowerShellはcall operatorとsingle quote、cmdはUTF-16LE PowerShell scriptの`-EncodedCommand`を使う。
- CodexとClaudeは各`[agent_start.commands]`をそのまま使い、native resume引数を一度だけ追加する。
- 復元commandはpane shell内で実行し、即時失敗後もshellと保存session IDを残して再試行経路を維持する。
- 保存session参照またはpending restore planがあるpane border右上へ`saved session`を表示する。
- `herdr-jobs`だけをpaneへ直接通知し、通常roomはqueuedのままinbox/historyへ保持してpaneへ注入しない。

## 実再起動結果

- `/Users/kazuph/.local/bin/herdr`へrelease binaryをad-hoc署名付きでインストールし、server restartはexit 0。
- 再起動前のClaude 5 IDとCodex 16 IDは、再起動後も全件同じIDでagent listと`session.json`に存在する。
- session ID差分は、read-only監査で新規作成されたCodex 2 IDの追加だけ。既存IDの欠落・置換は0件。
- Claude 5件は`--thinking-display summarized --permission-mode auto --resume <id>`で稼働。
- Codex 16件は`--sandbox workspace-write --config sandbox_workspace_write.network_access=true --dangerously-bypass-approvals-and-sandbox resume <id>`で稼働し、同一flagの二重指定はない。
- Claude paneの実画面には再起動前の会話本文が残り、全agent paneの画面末尾にrestore errorは0件。
- 通常room 158件をqueuedとしてinboxへ戻し、一時DB triggerは削除済み。全paneの画面末尾に`📬 未読` nudgeは0件。

## 検証

- focused restore・notification: 30 / 30 pass
- Rust nextest: 2,897 / 2,897 pass
- native Clippy: pass
- integration assets: 2 / 2 pass
- plugin marketplace: 12 / 12 pass
- selected maintenance tests: 38 / 38 pass
- `cargo fmt --check`、`git diff --check`: pass
- 実zsh alias環境で`command 'codex' ... --version`: `codex-cli 0.145.0`、exit 0
- `saved session`のrenderer testとインストール済みbinary文字列: pass

## 残る確認

- GhosttyはComputer Useの安全制限対象だったため、pane borderの実スクリーンショットは未取得。実serverでは保存session参照を持つ全agent paneと、インストール済み描画コードを確認済み。
- macOSからのWindows cross-ClippyはWindows CRT/SDKの`stdlib.h`不在で、Herdr sourceの診断前に停止。Windows用shell文字列のunit testはpassしたが、Windows実機実行は未確認。
- commit・pushは依頼されていないため実施していない。

復元・`saved session`表示・通常room nudge停止の結果を確認してください。
