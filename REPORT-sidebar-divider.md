# サイドバー外側と上下セクションの区切り線を明るくする

> 「サイドバーとサイドバー内の上下の表示分ける線ですが、黒すぎて見えません。もっと明るくしほしい。」

## なぜ見えにくかったか

通常表示の外側の縦線 `│` と内部の横線 `─` は、どちらも背景面に使う `surface_dim` で描画されていました。
既定のCatppuccin Mochaでは背景面がRGB `(30, 30, 46)` のため、暗い背景上で境界を判別しにくい状態でした。

## どう直したか

線だけを、各テーマで控えめな文字に使う `overlay0` へ変更しました。
既定のCatppuccin MochaではRGB `(108, 112, 134)` になります。
選択背景、スクロールバー、文字、ナビゲーション中のアクセント色は変更していません。

対象は次の4箇所です。

- 展開表示のサイドバー右端
- 展開表示のワークスペース一覧とエージェント一覧の間
- 折り畳み表示のサイドバー右端
- 折り畳み表示の上下セクション間

## 現在の結果

- 展開・折り畳みのrenderer test: 2 / 2 pass
- Rust nextest: 2,899 / 2,899 pass
- Clippy: pass
- integration assets: 2 / 2 pass
- plugin marketplace: 12 / 12 pass
- maintenance tests: 80 / 80 pass
- release binaryを`~/.local/bin/herdr`へインストール済み
- server restart: exit 0
- 再起動前後ともagent 23件、saved session 22件を維持

GhosttyはこのAPIセッションの画面操作制限対象のため、スクリーンショットは取得していません。
描画セルの記号と前景色をrenderer testで直接検証し、変更済みrelease binaryでserverを再起動しています。

承認後のcommit messageは `fix: brighten sidebar divider lines` とし、`origin/main`へpushします。
