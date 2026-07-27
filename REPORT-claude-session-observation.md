# Claude Codeのhookなしsession ID観測と同一ID復元を修復

> 「はい、実装して。ちゃんと。これは回帰です。わたしの前のfork版では動ていたのえ。」

## 結果

修復できました。以前はhookなしで起動した `claude` / `claude -c` のsession IDがHerdrへ保存されず、同じcwdにClaudeのJSONLが存在してもrestart時に復元対象になりませんでした。

現在は、paneのforeground Claude processからPID・cwd・process startを取得し、Claudeのprocess recordを優先して同一processのsession IDを観測します。process recordがない旧形式だけは、cwd・開始時刻が一致するJSONL候補が一意のときに限って採用します。cwdの「最新session」や `--last` は使いません。

観測したIDはterminal snapshot、`session.json`、`agent-session-ledger.json`へ保存されます。pane closeではledgerから削除し、pane moveでは移動先workspace/tabへkeyを付け替えます。通常起動とlive handoff時には、snapshotにIDがあるのにledgerだけ欠けた状態も一括修復します。

## 実環境で確認した2 pane

| pane | cwd | 観測・保存・復元したID | restart後 |
|---|---|---|---|
| `w655d76dedd2603:pB`（raw pane 45） | `mimamorin-web/.worktree/proposal/graph-redesign-storybook` | `24fcd322-987c-427b-9b7c-31bc6bc98005` | PID 35081で `claude --thinking-display summarized --permission-mode auto --resume 24fcd322-987c-427b-9b7c-31bc6bc98005` |
| `wB:pF`（raw pane 101） | `minecraft-edu-worker` | `7570de77-7886-4b34-9c24-657fd7ef4654` | PID 36353で `claude --thinking-display summarized --permission-mode auto --resume 7570de77-7886-4b34-9c24-657fd7ef4654` |

restart前の元processはPID 12239 / 25454の `claude ... -c` でした。restart後は両PIDが消滅し、上表の新PID・同一session IDで再開しました。両paneの画面も元会話の待機状態へ戻り、復元エラーは表示されていません。

live handoff直後には、ledgerへ次の対応が自動保存されることも確認しました。

- `w655d76dedd2603:t1:45` → Claude ID `24fcd322-987c-427b-9b7c-31bc6bc98005`
- `wB:t1:101` → Claude ID `7570de77-7886-4b34-9c24-657fd7ef4654`

## 主な変更箇所

- `src/agent_sessions.rs:117` — Claude process recordのPID・cwd・startedAt・procStartを検証
- `src/agent_sessions.rs:204` — transcript候補を一意性付きで観測
- `src/platform/mod.rs:23` — PID再利用を避ける安定したcwd・start identityを取得
- `src/pane.rs:595` — foreground job内の対象agent processだけからsession IDを観測
- `src/app/state.rs:1923` — session ownerのsource・agent・IDを同じrecordからledgerへ保存
- `src/app/state.rs:1974` — 起動時に全paneのledger欠損を一括修復
- `src/app/mod.rs:738` / `src/app/mod.rs:898` — 通常起動・live handoffの両方へ起動時同期を接続
- `src/persist/snapshot.rs:449` / `src/persist/restore.rs:818` — exact pane keyのledgerだけをsnapshot/restoreへ利用
- `src/app/actions.rs:1763` / `src/app/api/panes.rs:1079` — close削除・move再key

## 検証

- `just check`: 成功
  - Rust nextest: 2,914 / 2,914 passed
  - Python maintenance: 80 / 80 passed
  - integration: 2 / 2 passed
  - plugin marketplace: 12 / 12 passed
  - Windows `x86_64-pc-windows-msvc` clippy: warnings 0
- focused regression tests:
  - 起動時ledger修復
  - session reportとagent stateの到着順逆転
  - Claude process record / transcript一意候補
  - PID start identity
  - snapshot→restore、close、move lifecycle
- `just install-local`: 成功
  - `target/release/herdr` と `~/.local/bin/herdr` のSHA-256一致
  - `f6545aed3dc60d1cde4b0fbcb8c2b850f69b6bf25f4a0dc5347f9143eca85c4d`
- read-only独立レビュー: P1 / P2指摘なし
- live handoff: 対象2 paneの元processを維持したままledger自動補完に成功
- server restart: 対象2 paneを同一IDの明示的 `--resume` で復元

## 回復手段と残件

restart前の状態は次へ退避済みです。

- `/Users/kazuph/.config/herdr/backups/claude-restore-20260726-232733`
- `/Users/kazuph/.config/herdr/backups/claude-restore-before-restart-20260726-2347`

実装・検証・ローカル導入・実restartは完了しています。commit / pushは依頼されていないため行っていません。安定版docsは未リリース修正のため変更していません。
