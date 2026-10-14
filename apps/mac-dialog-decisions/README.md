# mac-dialog-decisions

Herdr の判断待ち（`herdr decision ask`、guard の `ask`、画面プロンプト検出など）を、macOS の最前面に開くネイティブダイアログで回答できるようにするプラグインです。Slack・Sabera グラス・アプリ・ターミナルと同じ `herdr decision watch` の流れに乗る、もう1つの回答窓口です（後回しではなく、created と同時に並行して出ます）。

## 動作

- 常駐デーモンが `herdr decision watch --all-machines` を購読します。
- `created` イベントで、AppKit のフローティングウィンドウを最前面に開きます。他のアプリの上に重なり、アクティブにもなります。
- ダイアログは `dialog.js`（JXA）を `osacompile` で `.app` アプレットにビルドし、`open` 経由で起動します（バンドルアプリとして起動されるため、正規のアクティベーション・Dock 表示・Accessibility が効きます）。ビルドはデーモン状態ディレクトリで1回だけ行い、`osascript -l JavaScript` 直起動へ自動フォールバックします。結果は spec 内の `result_path` に書かれる JSON 行で返ります。
- ダイアログには判断待ちの全情報を欠落なく表示します（スクロール可能・リサイズ可能・コピー可能。長い本文やプレビューは切り捨てません）:
  - 出どころ: マシン › スペース › タブ › ペイン（エージェント）、ペインID、cwd、ブランチ、コマンド、理由
  - `question_label`（「質問 1/3」など）、題名、本文
  - 全選択肢の番号・ラベル・説明文・プレビュー（読めなかったものは「プレビューあり（画面で確認）」）
  - 期限（絶対時刻＋残り時間）、順番待ち件数
- 回答は `herdr decision answer <id> <option_id>... --machine <machine.id> --responder mac-dialog [--text ...]` を実行します。選択肢と自由入力は同時に送れます。
- 他所で回答・取消・期限切れになった判断待ち（`resolved`）や、マシン切断（`machine_down`）では、開いているダイアログを即座に閉じます。閉じたダイアログからの回答は送られません（仮に送られてもサーバー側が `decision_already_resolved` で弾きます）。
- 判断待ちが複数あるときは1枚だけ開き、残りはキューに積んで順に出します（「ほか N 件」と表示）。
- 「あとで」ボタン・Esc・ウィンドウの閉じるボタンは回答せずに閉じます。その判断待ちはほかの窓口（Slack・ターミナル等）で引き続き回答できます。

## 答え方

- **単一選択**: 選択肢のボタンを押すと即送信（確認ステップなし）。
- **複数選択**（`multi_select`）: 選択肢はチェックボックスになります。チェックして「この選択で回答」を押すと送信します（Tab／Space でも切り替えられます）。
- **自由入力**（`allow_text`）: 下部の入力欄に書いて「送信」。選択肢のボタンを押した場合は入力文も一緒に送られます。
- **あとで**: ダイアログを閉じるだけ。回答にはなりません。

## インストール

Herdr リポジトリ内から開発リンク:

```bash
herdr plugin link <repo>/apps/mac-dialog-decisions
```

リンク後、最初の `decision.created` イベントで `ensure-daemon` が走り、デーモンを `herdr run --label mac-dialog-decisions` ジョブとして起動します。手動で起動する場合はプラグインアクション `start`（`herdr plugin action invoke start --plugin kazuph.mac-dialog-decisions`）を使います。

前提: macOS のみ（`platforms = ["macos"]`）。Node.js 18 以上、`osascript`/`osacompile`/`open`（いずれも macOS 標準搭載・追加依存なし）。

## 設定

プラグイン設定は `<config dir>/config.json`（`herdr plugin config-dir kazuph.mac-dialog-decisions` で確認可）:

| キー | 既定 | 意味 |
| --- | --- | --- |
| `enabled` | `true` | `false` でプラグイン無効（デーモンを起動しません） |
| `herdr_bin` | `herdr` / `HERDR_BIN_PATH` | herdr 実行ファイルのパス |
| `helper_command` | なし | ダイアログ表示プロセスの差し替え（テスト用。argv の配列。アプレット起動の代わりに実行され、引数に spec JSON のパスが渡り、結果を stdout に JSON 行で返します） |
| `present_delay_ms` | `250` | キューの先頭を画面に出すまでの待ち。同時に届いた複数の判断待ちを束ねて、最初のダイアログの「ほかN件」を正確にします |

環境変数でも上書きできます: `MAC_DIALOG_ENABLED`（`0`/`false` で無効）、`MAC_DIALOG_PRESENT_DELAY_MS`、`MAC_DIALOG_HELPER_CMD`（JSON の argv）、`MAC_DIALOG_STATE_DIR`、`MAC_DIALOG_CONFIG`（設定ファイルのパス）。

## オフにする方法

設定ファイルに `{"enabled": false}` を書くか、`MAC_DIALOG_ENABLED=0` を環境に入れます。走っているデーモンは `herdr run list` で `mac-dialog-decisions` ジョブを見つけて `herdr run cancel` で止められます。

## 起動方式

`decision.created` のプラグインイベントフックが `ensure-daemon` を呼び、デーモンを `herdr run --label mac-dialog-decisions` のジョブとして起動します（slack-decisions と同じ仕組み）。pid ファイルとロックで二重起動を防ぐので、イベントごとに呼ばれてもデーモンは1つだけ走ります。ログは `herdr log <job>` で読めます。

## テスト

```bash
cd apps/mac-dialog-decisions
node --test test/daemon.test.mjs test/render.test.mjs   # 単体テスト
node test/e2e.mjs                                      # e2e（隔離 herdr サーバー + スクリプト化フェイクダイアログ）
```

- `daemon.test.mjs`: JSON 行の処理、キュー、resolved/machine_down で閉じる、回答コマンドの形（`--machine`・`--responder mac-dialog`）など。
- `render.test.mjs`: ダイアログに渡す全文の組み立て（情報の欠落がないこと、長文が切り捨てられないこと）。
- `e2e.mjs`: 捨て用の XDG/ソケットで本物の `herdr server` を起こし、本物のデーモンにフェイクのダイアログコマンドを差し込んで、回答・キューイング・他所回答によるクローズを確かめます。証拠は `.artifacts/feat-mac-dialog-decisions/e2e/` に出ます。
- `live-verify.mjs`: 隔離サーバー＋本物のアプレットダイアログで、前面表示・スクリーンショット・Accessibility によるボタン押下/自由入力・他所回答クローズ・キュー表示を確かめます（要 Accessibility 権限・マウスは奪いません）。証拠は `.artifacts/feat-mac-dialog-decisions/live/` と同じく RESULT.md に出ます。
