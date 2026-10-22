# mac-dialog-decisions

Herdr の判断待ち（`herdr decision ask`、guard の `ask`、画面プロンプト検出など）を、macOS の最前面に開くネイティブダイアログで回答できるようにするプラグインです。Slack・Sabera グラス・アプリ・ターミナルと同じ `herdr decision watch` の流れに乗る、もう1つの回答窓口です（後回しではなく、created と同時に並行して出ます）。

## 動作

- 常駐デーモンが `herdr decision watch --all-machines` を購読します。
- `created` イベントで、ネイティブの AppKit パネル（`dialog.swift`）を今いるデスクトップの最前面に1回だけ出します。**作業中のアプリからキーボードを奪いません**（アクティブにしない・何度も前に出し直さない）。パネルを自分でクリックしたときだけ入力先になります（日本語入力もそこから使えます）。
- すべての判断待ちを1アプリの複数窓として少しずつずらして開きます。通常の層なので他アプリの後ろへ下がり、全デスクトップには追従しません。Dock は1アイコンで、開いている窓の数だけをバッジに表示します（あとで中・回答再送中を除く）。0枚で Dock から消えます。⌘Tab・Dock クリックで戻せ、Dock 右クリックの出どころと質問の一覧から個別に選べます。別窓が開閉しても書きかけは保持します。
- パネルの中身（上から）:
  - 出どころ: 🤖 エージェント（保存マシンならマシン名）、**プロジェクト（スペース） › タブ › ペイン**。ウィンドウのタイトルにもプロジェクト名が出ます。
  - 質問の見出し（「質問 1/3」など）と題名を大きく、本文（前提）を全文。
  - コマンド・場所・ブランチ・理由・ペインID。
  - 選択肢ごとのカード: 番号・題名・説明文・プレビュー（読めなかったものは「プレビューあり（エージェントの画面で確認）」）。長い本文やプレビューは切り捨てず、パネル内をスクロールできます。
  - 下に固定: 自由入力欄、期限、ほかに待っている件数、「あとで」と「回答する ⌘↩」。
  - 自由入力欄では ⌘V（貼り付け）・⌘C・⌘X・⌘A・⌘Z / ⇧⌘Z が使えます（パネルにメニューバーが無いので、キーを文字欄へ直接渡しています）。
- 回答は `herdr decision answer <id> <option_id>... --machine <machine.id> --responder mac-dialog [--text ...]` を実行します。
- 他所で回答・取消・期限切れになった判断待ち（`resolved`）や、マシン切断（`machine_down`）では、開いているパネルを即座に閉じます。デーモンが止まった・パネル用ファイルが消えたときもパネルは自分で閉じるので、答えた後に窓が残りません。
- 回答・あとで・他所で解決・取り消しはその1枚だけ、マシン切断はそのマシンの窓だけを閉じます。常駐役が止まると全窓を閉じます。

## 答え方

- **1つ選ぶ質問**: 選択肢のカード（丸印）をクリックして選び（⌘1〜⌘9 でも可）、右下の「回答する」か ⌘↩ で送ります。**選んだだけでは送りません**（作業中に突然出たパネルを誤ってクリックしても答えにならないように）。選んだカードをもう一度押すと選択を外せます。自由入力に書いてあれば一緒に送ります（エージェントの画面には「『選択肢』を選択。書いた文」として自由入力欄に届きます。プレビュー付きの質問ではその選択肢のメモになります）。選択を外せば文だけ送ることもできます。
- **複数選ぶ質問**: カード（四角印）をクリックしてチェックを付け外しし（⌘1〜⌘9 でも可）、「回答する」か ⌘↩。自由入力も一緒に送ります。
- **自由入力だけの質問**: 書いて ⌘↩ か「回答する」。
- **あとで**（または Esc・閉じるボタン）: 答えずにいったん閉じ、**10分後**（設定で変更可）にまだ答えていなければもう一度出します。その間も Slack・アプリ・グラス・ターミナルでいつでも答えられます。

## インストール

Herdr リポジトリ内から開発リンク:

```bash
herdr plugin link <repo>/apps/mac-dialog-decisions
```

リンク後、最初の `decision.created` イベントで `ensure-daemon` が走り、デーモンを `herdr run --label mac-dialog-decisions` ジョブとして起動します。手動で起動する場合はプラグインアクション `start`（`herdr plugin action invoke start --plugin kazuph.mac-dialog-decisions`）を使います。

前提: macOS のみ（`platforms = ["macos"]`）。Node.js 18 以上と Xcode（`swiftc`）。パネルは初回起動時に `swiftc` でプラグインの状態ディレクトリへビルドし、`dialog.swift` が変わったときだけ作り直します（`DEVELOPER_DIR` が CommandLineTools を指していると SDK と合わずに失敗するため、ビルド時は外します）。

## 設定

プラグイン設定は `<config dir>/config.json`（`herdr plugin config-dir kazuph.mac-dialog-decisions` で確認可）:

| キー | 既定 | 意味 |
| --- | --- | --- |
| `enabled` | `true` | `false` でプラグイン無効（デーモンを起動しません） |
| `herdr_bin` | `herdr` / `HERDR_BIN_PATH` | herdr 実行ファイルのパス |
| `helper_command` | なし | パネル表示プロセスの差し替え（テスト用。argv の配列。`--stream` で1プロセスを起動し、stdin の JSON 行 `{command:"open", key, spec_path}` / `{command:"close", key}` で窓を管理。stdout の結果にも同じ `key` を返します） |
| `later_minutes` | `10` | 「あとで」で閉じた判断待ちをもう一度出すまでの分数 |
| `present_delay_ms` | `250` | 同時に届いた複数の判断待ちを束ねて全窓を出すまでの待ち |

環境変数でも上書きできます: `MAC_DIALOG_ENABLED`（`0`/`false` で無効）、`MAC_DIALOG_PRESENT_DELAY_MS`、`MAC_DIALOG_LATER_MS`、`MAC_DIALOG_HELPER_CMD`（JSON の argv）、`MAC_DIALOG_STATE_DIR`、`MAC_DIALOG_CONFIG`（設定ファイルのパス）。

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
- `render.test.mjs`: パネルに渡す内容の組み立て（出どころ・全選択肢の説明とプレビューが欠けないこと、長文が切り捨てられないこと）。
- `e2e.mjs`: 捨て用の XDG/ソケットで本物の `herdr server` を起こし、本物のデーモンにフェイクのダイアログコマンドを差し込んで、回答・キューイング・他所回答によるクローズを確かめます。証拠は `.artifacts/feat-mac-dialog-decisions/e2e/` に出ます。
