# mac-dialog-decisions

Herdr の判断待ち（`herdr decision ask`、guard の `ask`、画面プロンプト検出など）を、macOS の最前面に開くネイティブダイアログで回答できるようにするプラグインです。Slack・Sabera グラス・アプリ・ターミナルと同じ `herdr decision watch` の流れに乗る、もう1つの回答窓口です（後回しではなく、created と同時に並行して出ます）。

## 動作

- 常駐デーモンが `herdr decision watch --all-machines` を購読します。
- `created` イベントで、ネイティブの AppKit パネル（`dialog.swift`）を今いるデスクトップの最前面に1回だけ出します。**作業中のアプリからキーボードを奪いません**（アクティブにしない・何度も前に出し直さない）。パネルを自分でクリックしたときだけ入力先になります（日本語入力もそこから使えます）。
- パネルの中身（上から）:
  - 出どころ: 🤖 エージェント（保存マシンならマシン名）、**プロジェクト（スペース） › タブ › ペイン**。ウィンドウのタイトルにもプロジェクト名が出ます。
  - 質問の見出し（「質問 1/3」など）と題名を大きく、本文（前提）を全文。
  - コマンド・場所・ブランチ・理由・ペインID。
  - 選択肢ごとのカード: 番号・題名・説明文・プレビュー（読めなかったものは「プレビューあり（エージェントの画面で確認）」）。長い本文やプレビューは切り捨てず、パネル内をスクロールできます。
  - 下に固定: 自由入力欄、期限、ほかに待っている件数、「あとで」と「回答する ⌘↩」。
- 回答は `herdr decision answer <id> <option_id>... --machine <machine.id> --responder mac-dialog [--text ...]` を実行します。
- 他所で回答・取消・期限切れになった判断待ち（`resolved`）や、マシン切断（`machine_down`）では、開いているパネルを即座に閉じます。デーモンが止まった・パネル用ファイルが消えたときもパネルは自分で閉じるので、答えた後に窓が残りません。
- 判断待ちが複数あるときは1枚だけ出し、残りは順番に出します（「ほかに N 件待っています」）。

## 答え方

- **1つ選ぶ質問**: 選択肢のカードをクリックすると、その場で回答します（⌘1〜⌘9 でも可）。自由入力に書いてあれば一緒に送ります。文だけ送るときは ⌘↩ か「回答する」。
- **複数選ぶ質問**: カードをクリックしてチェックを付け外しし（⌘1〜⌘9 でも可）、「回答する」か ⌘↩。自由入力も一緒に送ります。
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
| `helper_command` | なし | パネル表示プロセスの差し替え（テスト用。argv の配列。引数に spec JSON のパスが渡り、結果を stdout に JSON 行で返します） |
| `later_minutes` | `10` | 「あとで」で閉じた判断待ちをもう一度出すまでの分数 |
| `present_delay_ms` | `250` | キューの先頭を画面に出すまでの待ち。同時に届いた複数の判断待ちを束ねて、最初のダイアログの「ほかN件」を正確にします |

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
