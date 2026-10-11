# slack-decisions

Herdr の判断待ち（`herdr decision ask` など）を Slack へ転送し、Slack 上の数字リアクションまたはスレッド返信で回答できるようにするプラグインです。Sabera グラスが繋がっている間は一定時間（既定3分）投稿を遅らせ、グラス優先の動線を崩しません。

## 動作

- 常駐デーモンが `herdr decision watch --all-machines` を購読します。
- `created` イベントで:
  - Sabera グラス未接続 → 即座に Slack へ投稿
  - Sabera グラス接続中 → `post_delay_ms`（既定 180,000ms = 3分）待機し、その間に解決したら投稿しない
- Slack への回答:
  - 数字リアクション（`:one:`〜`:nine:`）→ 対応するオプション
  - スレッド返信の数字（`1`, `２`, ...）→ 対応するオプション
  - `allow_text` の判断待ちへのスレッド返信 → `--text` として転送
  - いずれも `herdr decision answer <id> <option> --machine <machine.id> --responder slack [--text ...]` を実行
- 他所で決定した場合（answered / expired / cancelled）は Slack 投稿を更新し、回答者と結果を表示します。
- `machine_down` 中のマシンの pending は投稿せず、投稿済みのものは「マシン切断」表示に更新します。
- Socket Mode のイベントは1接続のみ。別のコンシューマ（ai_guard など）にイベントが流れた場合に備え、チャンネル履歴のポーリングでも回答を回収します。

## 成果物を Slack に送る（deliver）

デーモンの判断待ち転送とは別に、ファイルをそのまま Slack チャンネルへ送る口があります。

```bash
node apps/slack-decisions/slack-decisions.mjs deliver <ファイル> [--title T] [--comment C]
```

- slack-ask（`slack-send.mjs`）と同じ資格情報・チャンネルを使います（`files.getUploadURLExternal` → バイナリ upload → `files.completeUploadExternal`）。`--comment` を付けるとファイルの前に本文を投稿します。`--title` は省略するとファイル名になります。
- 送信が終わると Sabera の hub（`~/.sabera-relay.sock` のローカル依頼）へ `{"type":"notify"}` で「「<題名>」を Slack に送りました」を出します。hub ソケットが無いときは通知を飛ばします（結果 JSON の `notified` が `false`）。
- 結果は stdout に JSON で返します（`{ ok, file_id, channel, path, message_ts, notified }`）。

### プラグイン action `deliver`

`herdr plugin action invoke deliver --plugin kazuph.slack-decisions`（ペインでテキストを選択しているときに現れます）でも同じ送付を呼べます。プラグイン action は固定 argv なので、ファイルパスは `HERDR_PLUGIN_CONTEXT_JSON.selected_text`（ペインで選択した文字列）から取ります。相対パスはそのペインの cwd から解決します。つまり、成果物のパスをペイン上で選択してから action を呼ぶとそのファイルが送られます。

## 送信は kazuph のマシンのみ

`whoami`（OS ユーザー名）が `kazuph` でないマシン、または Slack 資格情報を解決できない場合は、理由をログに出して安全に終了します。Slack へは一切送信しません。`deliver` / `deliver-context` も同じゲートを通ります。

## インストール

Herdr リポジトリ内から開発リンク:

```bash
herdr plugin link <repo>/apps/slack-decisions
```

リンク後、最初の `decision.created` イベントで `ensure-daemon` が走り、デーモンを `herdr run --label slack-decisions` ジョブとして起動します。手動で起動する場合はプラグインアクション `start`（`herdr plugin action invoke start --plugin kazuph.slack-decisions`）を使います。

前提: Node.js 18 以上（標準ライブラリのみ使用・追加依存なし）。

## 設定

資格情報は slack-ask（`slack-approval.mjs`）と同じ探索順で読みます。リポジトリには保存しません。

1. 環境変数 `SLACK_BOT_TOKEN` / `SLACK_CHANNEL`（または `SLACK_CHANNEL_ID`）/ `SLACK_APP_TOKEN`
2. `~/dotfiles/claude/hooks/.env`、`~/.claude/hooks/.env`、`~/.config/claude-slack/credentials`
3. macOS キーチェーン `security -a claude-slack -s SLACK_BOT_TOKEN`（同 `SLACK_CHANNEL` / `SLACK_APP_TOKEN`）
4. `pass` の `claude/slack-bot-token` / `claude/slack-channel` / `claude/slack-app-token`

プラグイン設定は `<config dir>/config.json`（`herdr plugin config-dir kazuph.slack-decisions` で確認可）:

| キー | 既定 | 意味 |
| --- | --- | --- |
| `enabled` | `true` | `false` でデーモン無効 |
| `post_delay_ms` | `180000` | グラス接続中の投稿遅延 |
| `poll_interval_ms` | `5000` | 履歴ポーリング間隔 |
| `slack_channel` | 資格情報由来 | 投稿先チャンネル |
| `mention` | なし | 投稿ヘッダに載せるメンション |
| `sabera_socket` | `~/.sabera-relay.sock` | Sabera リレーのソケット |
| `socket_mode` | `true` | Socket Mode 接続の有無 |
| `credentials_env_files` | slack-ask と同じ3ファイル | 資格情報 env ファイル |
| `allowed_os_user` | `kazuph` | Slack 送信を許可する OS ユーザー |

環境変数 `SLACK_DECISIONS_*`（`POST_DELAY_MS`, `POLL_INTERVAL_MS`, `API_BASE`, `SABERA_SOCK`, `STATE_DIR`, `CHANNEL`, `MENTION`, `ENABLED`, `SOCKET_MODE`, `ALLOWED_USER`）でも上書きできます。

## 起動方式

`decision.created` のプラグインイベントフックが `ensure-daemon` を呼び、デーモンを `herdr run --label slack-decisions` のジョブとして起動します（saberante hub と同じ仕組み）。`ensure-daemon` は pid ファイルで二重起動を防ぐので、イベントごとに呼ばれてもデーモンは1つだけ走ります。

## テスト

```bash
cd apps/slack-decisions
node --test test/daemon.test.mjs    # ユニット/統合（フェイク Slack・フェイク Sabera）
node --test test/deliver.test.mjs   # deliver CLI（実プロセス + フェイク Slack/Sabera）
node test/e2e.mjs                   # 隔離 herdr サーバー + 実デーモン + フェイク Slack
```

いずれも実 Slack には接続しません。
