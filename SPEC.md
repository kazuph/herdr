# SPEC.md — kazuph/herdr fork 追加仕様カタログ

このファイルは **kazuph/herdr fork が本家 (ogulcancelik/herdr) に対して追加・変更している仕様の正本**。
戦略・経緯・監査結果は `docs/fork-strategy.md` を参照。

## このファイルの使い方（AI向け）

- **ゴール**: AIがこのSPECと最新のupstream/masterだけを材料に、「本家＋kazuph仕様」のherdrをいつでも再構築（植え替え）できる状態を保つこと。
- 各エントリは fork実装の内部構造ではなく**観測可能な挙動**（キー・メニュー文言・config key・表示・CLI）で書かれている。移植時は最新本家のAPI/構造に合わせて実装してよいが、**受け入れ条件とデグレ判定を満たすこと**が完了条件。
- 新しいこだわり・機能を足すときは、**実装より先にこのSPECへエントリを追記**する。
- エントリの正確さに疑問があれば、元コミット（fork master上のsha）を `git show` して確認する。

## 分類の意味

| 分類 | 意味 |
|---|---|
| CORE-UI | 本家に無く、プラグインでは表現できない。fork patchとして実装する |
| PARTIAL | 本家に部分的な土台がある。本家APIに乗せて差分だけ実装する |
| PLUGIN-ABLE | 本家のCLI/socket/通知APIで再実装可能。ただし**使い心地がデグレするならfork実装**にする |
| POLICY | コードよりfork運用方針。docs/build/テスト衛生 |

## 全体運用ルール

1. **fail-closed原則**: pane解決・session復元で、根拠（明示されたID・観測済みsession）がないとき推測・fallbackで成功させない。`--last`・cwd最新・focused pane推測は全面禁止
2. **デグレ絶対禁止**: 移植・プラグイン化で現forkの使い心地から劣化する選択をしない。各エントリのデグレ判定が基準
3. **本家へのPRをAIが自発的に作成・送信することを禁止**（詳細は `docs/fork-strategy.md`）
4. 変更を完了扱いにする前に release binary を rebuild し `~/.local/bin/herdr` を差し替える（macOSは `just install-local` で署名込み）

---

## G1. レイアウト操作

### pane 右クリックのレイアウト操作
- **元コミット**: e778a18, 634fc6e, 9349d61, 487bfea
- **分類**: CORE-UI
- **目的**: マウス操作だけで pane の分割、配置換え、均等化、zoom 解除まで完結できるようにする。タッチ端末やリモート UI でキーボードショートカットに頼らず pane レイアウトを整えるために必要。
- **UI挙動**:
  - pane の右クリックメニューには、複数 pane がある場合だけレイアウト操作を表示する。単一 pane では `Move to left split` / `Move to right split` / `Move to upper split` / `Move to lower split` / `Equalize pane sizes` / `Cycle pane layout` / `Rotate panes` / `Rotate panes reverse` / `Zoom` / `Unzoom` を表示しない。
  - 手動 pane 名がある pane のメニュー順は `Split vertical`, `Split horizontal`, `--`, `Rename pane`, `Clear pane name`, `--`, agent 起動項目, `--`, レイアウト操作群, `--`, `Zoom` または `Unzoom`, `Close pane`。手動 pane 名がない場合は `Clear pane name` を出さない。
  - `Move to left split` は右クリックされた pane を focus して、既存の他 pane 群を右側に残し、対象 pane を root の左側 split に移動する。
  - `Move to right split` は右クリックされた pane を focus して、既存の他 pane 群を左側に残し、対象 pane を root の右側 split に移動する。
  - `Move to upper split` は右クリックされた pane を focus して、既存の他 pane 群を下側に残し、対象 pane を root の上側 split に移動する。
  - `Move to lower split` は右クリックされた pane を focus して、既存の他 pane 群を上側に残し、対象 pane を root の下側 split に移動する。
  - 対象 pane を root split へ移動するとき、対象 pane 以外の pane の相対順と内側レイアウトは維持する。対象 pane のサイズ比は全 pane 数を N として 1/N、残り pane 群は (N-1)/N。
  - `Equalize pane sizes` は現在の split 方向を変えず、leaf pane 数に応じて各 split の比率を再計算し、表示面積を均等化する。
  - zoom 中の pane 右クリックメニューは `Zoom` ではなく `Unzoom` を表示し、選択すると通常表示へ戻る。
  - どの右クリック操作も適用後は terminal mode に戻り、session は dirty になる。
- **受け入れ条件**:
  - 3 pane で中央 pane を `Move to lower split` すると、focus はその pane のまま、pane 順は `[root, third, clicked]` になり、90x30 の領域では残り 2 pane が上段 20 行を左右 45 幅ずつ、クリック pane が下段 10 行を全幅で占める。
  - 3 pane で中央 pane を `Move to left split` すると、focus はその pane のまま、pane 順は `[clicked, root, third]` になり、90x30 の領域では 3 pane が左右 30 幅ずつ並ぶ。
  - 3 pane で中央 pane を `Move to upper split` すると、focus はその pane のまま、pane 順は `[clicked, root, third]` になり、90x30 の領域ではクリック pane が上段 10 行を全幅で占め、残り 2 pane が下段 20 行を左右 45 幅ずつ占める。
  - split 比率を 0.8 などに崩した 3 pane で `Equalize pane sizes` を実行すると、90x30 の横並び表示で各 pane 幅が 30 になる。
  - 単一 pane の右クリックメニューにはレイアウト操作と `Zoom` / `Unzoom` が出ない。
  - zoom 中の右クリックメニューには `Unzoom` があり、`Zoom` はない。
- **実装方針**: 本家の右クリック context menu 構造に pane 専用アクションを追加する。移動の backend は本家 `pane.move` が root side 指定と「対象 pane だけ移動、他 pane 群を保持」を表現できるならそれに乗せ、足りなければ layout tree に root split side 付きの移動 primitive を追加する。均等化は pane tree の split 比率再計算で実装し、plugin API だけでは menu 表示条件と root split 再構成を再現できない。
- **デグレ判定**:
  - 単一 pane で無効なレイアウト操作が表示される。
  - 右クリックした pane ではなく focus 中 pane が移動、回転、均等化される。
  - `Move to left/right/upper/lower split` の文言、表示順、separator が現 fork と変わる。
  - 移動時に対象外 pane の順序や内側レイアウトが崩れる。
  - zoom 中に `Unzoom` ではなく `Zoom` が表示される。

### レイアウト cycle
- **元コミット**: b452173, 634fc6e
- **分類**: CORE-UI
- **目的**: pane が増えたとき、横一列や縦一列だけでなく、メイン pane とグリッドを含む実用的な配置へ 1 操作で巡回できるようにする。手作業の resize と split 作り直しを減らすための機能。
- **UI挙動**:
  - pane 右クリックメニューの `Cycle pane layout`、または bottom pane action bar の ` CYCLE LAYOUT ` を選択すると cycle が 1 段進む。
  - cycle は pane ID の昇順を基準に pane 順を組み直す。現在 focus 中 pane は cycle 後も focus されたまま維持する。
  - pane が 1 枚以下のときは何も変えない。
  - 巡回順は、横一列 → 縦一列 → 左メイン + 右側 2 行グリッド → 右メイン + 左側 2 行グリッド → 上メイン + 下側横一列 → 下メイン + 上側横一列 → 横一列。
  - メイン pane は pane ID 昇順の先頭 pane。メイン領域と残り領域の split 比率は 0.5。残り領域が 2 行グリッドの場合、上段の数は残り pane 数の切り上げ半分、下段が残り。
- **受け入れ条件**:
  - 9 pane を横一列にして `Cycle pane layout` を 1 回実行すると縦一列になる。180x45 では先頭 pane が y=0 高さ 5、最後 pane が y=40 高さ 5 になる。
  - 2 回目は左メイン + 右 2 行グリッドになる。160x40 では先頭 pane が x=0 幅 80 高さ 40、右側の残り pane は 20x20 の 2 行グリッドになる。
  - 3 回目は右メイン + 左 2 行グリッドになる。160x40 では先頭 pane が x=80 幅 80 高さ 40、残り pane は左側 80 幅に 2 行グリッドで並ぶ。
  - 4 回目は上メイン + 下横一列になる。160x40 では先頭 pane が y=0 高さ 20 全幅、残り pane は下段で横一列に並ぶ。
  - 5 回目は下メイン + 上横一列になる。160x40 では先頭 pane が y=20 高さ 20 全幅、残り pane は上段で横一列に並ぶ。
  - 6 回目は横一列に戻る。180x40 では先頭 pane が x=0 幅 20、最後 pane が x=160 幅 20 になる。
- **実装方針**: 本家に `cycle_layout` 相当がないため fork 側の core UI として持つ。pane tree の leaf を pane ID 昇順で収集し、既知 preset に一致するかを判定して次 preset の tree を生成する。本家 `pane.move` は単発移動の土台にはなるが、この preset cycle 全体は表現できない。
- **デグレ判定**:
  - cycle が pane 作成順や現在の leaf 順に依存し、pane ID 昇順で安定しない。
  - focus pane が cycle 後に先頭 pane へ勝手に移る。
  - 左メイン、右メイン、上メイン、下メイン、2 行グリッドのいずれかが巡回から欠ける。
  - 2 行グリッドの上段数が切り上げ半分にならない。

### pane 回転と target 安定化
- **元コミット**: 978e23c, 447bfa5
- **分類**: PARTIAL
- **目的**: split の形を崩さずに pane の位置だけを前後へ回転し、見やすい位置へ pane を移せるようにする。同時に `%N` で狙う外部 command target と attached terminal の対応を rotation / restore 後も壊さない。
- **UI挙動**:
  - pane 右クリックメニューの `Rotate panes` は forward rotation、`Rotate panes reverse` は reverse rotation を行う。
  - bottom pane action bar の ` ROTATE PANES ` は `Rotate panes` と同じ forward rotation を行う。
  - rotation 実行前に、右クリックされた pane を focus する。実行後もその pane ID が focus 対象として残る。
  - split tree の形と split 比率は変えず、leaf 位置に入る pane ID だけを回転する。forward は leaf 順の pane ID を右回転、reverse は左回転する。
  - pane ID と terminal state の対応は変えない。つまり `%N` target は同じ terminal / agent を指し続け、その pane ID が画面上の別位置へ移動する。
  - session restore は保存済み pane ID を維持し、復元後の public pane number も保存 pane ID に揃える。workspace duplication など「複製」用途では新しい pane ID を割り当て、衝突を避ける。
  - pane が 1 枚以下のときは何も変えない。
- **受け入れ条件**:
  - 3 pane の leaf 順 `[first, second, third]` で `Rotate panes` を実行すると leaf 順は `[third, first, second]` になり、各 pane ID に attached terminal は元のまま残る。
  - 続けて `Rotate panes reverse` を実行すると leaf 順は元の `[first, second, third]` に戻り、terminal 対応も変わらない。
  - 異なる split 形で 3 pane を作り、rotation 前の各 pane ID の表示矩形を記録した場合、forward 後は `third` が元 `first` の矩形、`first` が元 `second` の矩形、`second` が元 `third` の矩形に入る。
  - restore 後に保存 pane ID が別 ID へ remap されず、agent-targeted command が restart 前と同じ pane を指す。
- **実装方針**: 本家 `pane.swap` が pane ID と terminal pairing を保ったまま leaf 位置だけを入れ替えられるなら、それを順に呼んで forward / reverse rotation を作る。`pane.swap` が terminal state の付け替えで `%N` target を壊す場合は使わず、layout tree の leaf pane ID を回転する primitive を core に追加する。restore は本家の restore 構造に「保存 ID を保持する復元」と「複製用 remap」を分ける。
- **デグレ判定**:
  - rotation 後に split 形や比率が変わる。
  - rotation 後に `%N` target が別 terminal / agent を指す。
  - attached terminal を pane ID 間で付け替えて、見た目だけ正しい状態になる。
  - restore 後に保存 pane ID が新規 ID へ remap され、agent command の target が変わる。
  - `Rotate panes` / `Rotate panes reverse` の文言や forward / reverse の向きが逆になる。

### bottom pane action bar と pane title zoom
- **元コミット**: 8cb716c, 4cbc5e9
- **分類**: CORE-UI
- **目的**: よく使う pane レイアウト操作を常時クリック可能にし、右クリックメニューを開かずに cycle / rotate / equalize を実行できるようにする。pane title 自体も zoom toggle として使い、マウス中心の操作を短くする。
- **UI挙動**:
  - desktop layout で active workspace があり、main body の高さが 2 行以上あるとき、terminal area の下に高さ 1 行の bottom pane action bar を表示する。mobile layout では表示しない。
  - tab bar が非表示でも action bar は main area の最下段 1 行を使い、terminal area はその分 1 行短くなる。例: 80x20 で tab bar 非表示なら terminal height は 19、action bar は y=19 高さ 1。
  - action bar 左端の状態ラベルは通常 ` PANES `。`ui.vim_mode = true` で Vim normal mode のときは ` VIM NORMAL `、insert mode のときは ` VIM INSERT ` を表示する。`ui.vim_mode` の default は `false`。
  - action bar の button は右寄せで、左から ` CYCLE LAYOUT `、1 桁 gap、` ROTATE PANES `、1 桁 gap、` EQUALIZE ` の順。右端には 1 桁 margin を置く。幅が足りない button は描画せず hit area も無効にする。
  - action bar 全体と左ラベルは背景 `Color::Reset`、前景 `overlay0`。button は背景 `Color::Reset`、前景 `accent`、太字 + 下線。
  - ` CYCLE LAYOUT ` のクリックは `Cycle pane layout` と同じ操作、` ROTATE PANES ` は `Rotate panes` と同じ forward rotation、` EQUALIZE ` は `Equalize pane sizes` と同じ操作。
  - pane の title 行を左クリックすると、その pane を focus して zoom toggle する。zoom 中の focused pane title は先頭に `ZOOM` を追加し、例として `%5 terminal` は `ZOOM %5 terminal` になる。
  - action bar と pane title の mouse down は terminal への mouse forwarding、workspace press、tab press、drag を発生させない。
- **受け入れ条件**:
  - active workspace がある desktop view で action bar rect が高さ 1 で terminal area の直下にある。
  - action bar button の hit area をクリックすると、それぞれ cycle、forward rotate、equalize が実行され、session が dirty になる。
  - pane title をクリックするとクリックした pane が focus され、zoomed になる。zoomed pane title を再度クリックすると zoom が解除される。
  - zoom 中の title には `ZOOM` prefix が出る。
  - action bar の背景が塗りつぶし surface / panel 色ではなく reset 背景になり、button は accent 色の太字下線で表示される。
- **実装方針**: 本家の render pipeline に、terminal area の下 1 行を予約する view geometry と hit area を追加する。action bar のクリックは本家の mouse input dispatcher から core の cycle / rotate / equalize action に接続する。plugin v1 では terminal area の persistent 1 行 UI と pane title hit area を安全に差し込めないため core UI として実装する。
- **デグレ判定**:
  - action bar が active workspace なし、mobile layout、または高さ不足で表示される。
  - action bar が terminal area と重なり、terminal 内容を隠す。
  - button label が ` CYCLE LAYOUT ` / ` ROTATE PANES ` / ` EQUALIZE ` から変わる。
  - button 背景が panel / surface で塗られ、現 fork の reset 背景 + accent 強調より目立つ配色に戻る。
  - pane title click が terminal に forwarding され、zoom toggle ではなく pane 内 click として扱われる。

## G2. サイドバー & ワークスペースUI

### サイドバー幅プリセット
- **元コミット**: c5b7668, aff2011, 8a51ff9
- **分類**: CORE-UI
- **目的**: タッチ端末や細かいドラッグ操作が苦手な環境でも、サイドバー幅を即座に狭い・標準・広い状態へ切り替えたい。workspace名、git情報、agent要約が長い時に、最大幅36列では不足するため広い表示を選べる必要がある。
- **UI挙動**:
  - expanded sidebar の幅境界は設定キー `[ui] sidebar_min_width` と `[ui] sidebar_max_width` で制御する。
  - default は `sidebar_min_width = 18`、`sidebar_max_width = 72`。
  - default sidebar width は既存の `[ui] sidebar_width` を使い、設定例では `sidebar_width = 32`、AppState初期値では `26` が使われている。
  - global menu に `sidebar narrow`、`sidebar normal`、`sidebar wide` を表示する。c5b7668時点の並びは `settings`、`keybinds`、`reload config`、`sidebar narrow`、`sidebar normal`、`sidebar wide`、必要なら `update ready` または `what's new`、最後に `detach`。
  - `sidebar narrow` は現在の `sidebar_min_width` に設定する。
  - `sidebar normal` は `sidebar_width` の設定値を `sidebar_min_width..sidebar_max_width` に clamp した幅に戻す。
  - `sidebar wide` は最大幅側へ広げる。c5b7668単体では `sidebar_max_width`、現forkの幅ボタン仕様では `normal` より大きく、`sidebar_max_width` を上限にした wide 幅として扱う。
  - global menu から narrow または wide を選ぶと手動幅として保存し、normal を選ぶと config default 幅として保存する。どの選択でも session snapshot に `sidebar_width` が残る。
  - sidebar 下部の幅トグルボタンは現在幅を ` NARROW `、` NORMAL `、` WIDE ` の大文字ラベルで表示する。ラベルには前後1スペースを含め、ボタン内で詰まって見えないようにする。
- **受け入れ条件**:
  - default config 生成例と設定ドキュメントで `sidebar_max_width` が `72` と説明されている。
  - `sidebar narrow` 選択後、sidebar幅が `sidebar_min_width` になり、session snapshot にその幅が保存される。
  - `sidebar normal` 選択後、sidebar幅が設定値由来の通常幅になり、session snapshot にその幅が保存される。
  - `sidebar wide` 選択後、sidebar幅が通常幅より広くなり、`sidebar_max_width` を超えない。
  - sidebar 下部の表示ラベルが `NORMAL` ではなく ` NORMAL ` のように前後スペース付きで描画される。
- **実装方針**: 本家に `SidebarWidthPreset` 相当と global menu の幅項目が無いため fork移植が必要。既存の config model、session snapshot、global menu構造に幅プリセットを追加し、render側では既存sidebar footer領域に現在プリセットラベルを描く。
- **デグレ判定**:
  - `sidebar_max_width` default が36列へ戻る。
  - global menuから幅を変えられない、または touch-only クライアントで非ドラッグの幅変更経路が無い。
  - `sidebar normal` が config default ではなく直前の手動幅や固定値へ戻る。
  - 幅ボタンの ` NARROW ` / ` NORMAL ` / ` WIDE ` が詰まり、前後スペースなしの `NARROW` / `NORMAL` / `WIDE` になる。

### Workspace Sections
- **元コミット**: 6b5e3ac, a31d120, 3a9a374, 74d5512, 8a51ff9, d5a853a
- **分類**: CORE-UI
- **目的**: workspace数が増えた時に、重要な作業・仕事・個人・未分類を視覚的に分け、見たいまとまりだけを展開したい。agent panel も折りたたみ状態に追従し、隠したworkspaceのagentで注意が散らないようにする。
- **UI挙動**:
  - workspace は4 sectionに属する。表示順は `⭐ favorites`、`💼 work`、`🏠 personal`、`spaces`。
  - 未指定workspaceのsectionは `spaces`。
  - desktop sidebar のworkspace listには、section headerを1行で表示する。展開時は `▾ <label>`、折りたたみ時は `▸ <label>`。
  - section header と中のworkspace cardの間には空行を1行置く。
  - workspace card のhit areaと選択/active highlightはsidebar幅いっぱいに残しつつ、カード内テキストは左に1列インデントする。
  - workspace右クリックメニューには section割り当て項目 `⭐ favorite`、`💼 work`、`🏠 personal`、`No section` を表示する。section項目の前には separator `--` を置き、キーボード上下移動では separator行を選択しない。
  - section headerをクリックすると、そのsectionを折りたたみ/展開する。折りたたむ時は workspace scroll と agent panel scroll を0へ戻す。展開する時も状態をsessionに保存する。
  - workspaceを別sectionへ割り当てた時、その移動先sectionは自動で展開され、workspace scroll と agent panel scroll を0へ戻す。
  - workspace card は section header へドラッグ&ドロップできる。`💼 work`、`🏠 personal`、`⭐ favorites`、`spaces` のheader上へ落とすと、そのworkspaceのsectionが変わる。
  - workspace card はsection内のbody領域へもドラッグできる。別sectionのカード下やsection空白へ落とすと、そのsectionへ移る。
  - drag中に対象sectionへ入った場合、そのsection headerは accent背景で強調表示される。
  - 同じsectionへドラッグしている場合は section変更ターゲットとして扱わない。
  - section内の下端drop slotは、そのsection内の最後のcard直下に出る。別sectionの境界を越えて挿入位置が計算されてはいけない。
  - collapsed section内のworkspace cardは描画しない。headerだけは残す。
  - agent panel は expanded section に属するworkspaceのagentだけを表示する。collapsed section内のagentは非表示。
  - mobile switcher も desktop sidebar と同じ section headerを表示し、header tapで折りたたみ/展開する。collapsed sectionのworkspaceはmobile switcherにも表示しない。
  - workspace名は custom name が無い場合、cwd由来名を使う。root pane がOSC title `0` / `1` / `2` でtitleを報告し、それがcwd由来名と異なる場合は `<cwd由来名>-<pane title>` と表示する。
  - full densityのworkspace rowはgit branchが無くても2行を使い、2行目には `nogit` を表示する。
- **受け入れ条件**:
  - `⭐ favorites` のheaderは `💼 work` より上に表示され、`💼 work` は `🏠 personal` より上、未分類は `spaces` として最後に表示される。
  - `⭐ favorites` を折りたたむと、そのsectionのworkspace cardは消え、headerだけ残る。
  - `⭐ favorites` に属するworkspaceのagentは、そのsectionを折りたたむと agent panel から消える。
  - workspace右クリックメニューで `💼 work` を選ぶと、対象workspaceがwork sectionへ入り、work sectionが展開される。
  - workspace cardを `💼 work` headerへドラッグして離すと、workspace順は変えずにsectionだけがworkになる。
  - workspace cardをwork sectionのbody領域へドラッグして離すと、対象workspaceがwork sectionになる。
  - section下端へドラッグした時の挿入インジケータは、そのsectionの最後のcard直下に `─` として描画される。
  - mobile switcherでwork headerをtapするとwork sectionが折りたたまれ、もう一度tapすると展開される。
  - root paneがOSC title `planner` を出し、cwd由来名が `pion` の時、workspace表示名が `pion-planner` になる。
- **実装方針**: 本家に `WorkspaceSection` 相当が無いため CORE-UI として移植する。既存workspace modelとsnapshotにsectionを保存し、sidebar/mobile switcher/agent panelの表示対象計算を section aware にする。本家の `pane.report_metadata` は将来的にpane title由来のworkspace名更新を縮小できる可能性があるが、section分類・折りたたみ・D&Dは本家APIだけでは足りない。
- **デグレ判定**:
  - section headerが出ない、または表示順が `⭐ favorites`、`💼 work`、`🏠 personal`、`spaces` から変わる。
  - 折りたたんだsectionのworkspaceまたはagentが残って表示される。
  - section headerクリック、mobile header tap、右クリックsection割り当て、sectionへのdrag&dropのいずれかが動かない。
  - drag中のdrop targetがsection境界をまたぎ、別sectionの下端へ誤挿入される。
  - section headerとカードの間の空行、カードhit areaのsidebar全幅、1列インデントのいずれかが失われる。
  - OSC titleがworkspace identityへ反映されず、`pion-planner` のような区別ができなくなる。

### サイドバー空白メニューと危険操作確認
- **元コミット**: d5a853a, 17ad5aa
- **分類**: CORE-UI
- **目的**: workspace listの空白領域から session 全体の操作へ素早く到達したい一方、server停止・restart・agent restore は誤操作すると作業状態を壊すため、即実行せず確認を挟む必要がある。
- **UI挙動**:
  - expanded sidebar の workspace list 空白領域を右クリックすると context menu を開く。
  - 同じ空白セルを500ms以内に左クリック2回しても同じ context menu を開く。
  - 空白判定に含めるのは workspace list 内だけ。workspace card、section header、workspace list scrollbar、workspace density toggle、sidebar footer、agent panelの空白では開かない。
  - d5a853a時点の sidebar blank menu は `New workspace`、`New tab`、`--`、`Settings`、`Keybinds`、`Reload config`、`--`、`Stop server`、`Restart`、`Detach`。
  - 17ad5aa適用後は危険操作のため `Restore agents...` が追加され、groupingは `New workspace`、`New tab`、`--`、`Settings`、`Keybinds`、`Reload config`、`--`、`Restore agents...`、`--`、`Detach`、`--`、`Stop server`、`Restart`。
  - `New workspace` は新規workspace作成を要求し、menuを閉じる。
  - `New tab` は新規tab名入力ダイアログを開く。
  - `Settings` は settings popup を開く。
  - `Keybinds` は keybind help overlay を開く。
  - `Reload config` は config reload を要求し、menuを閉じる。
  - `Detach` はclient detachを要求し、server停止はしない。
  - `Restore agents...`、`Stop server`、`Restart` は選択直後に実行せず、危険操作確認ダイアログを開く。
  - 危険操作確認ダイアログは画面中央、赤い枠/赤いconfirm buttonで表示する。基本サイズは幅64列・高さ5行。
  - `Stop server` の確認titleは `Stop server?`、detailは `Stops the Herdr server and all running panes.`、confirm button labelは `stop`。
  - `Restart` の確認titleは `Restart Herdr?`、detailは `Restarts the Herdr server. The saved session can be restored.`、confirm button labelは `restart`。
  - `Restore agents...` の確認titleは `Restore agents?`、detailは `Types resume commands into panes with recorded agent sessions.`、confirm button labelは `restore`。
  - 確認ボタンには `↵` hint、cancelボタンには `esc` hintと `cancel` labelを表示する。
  - Enter/confirmで `Stop server` はserver quitを要求する。`Restart` はrestart要求とserver quitを両方立てる。`Restore agents...` はagent restore要求だけを立て、server quitはしない。
  - Esc/cancel、またはconfirm/cancel button外のクリックでは pending dangerous action を消して元のmodeへ戻る。
  - agent restore実行後のtoastは title `agent restore`。contextは対象が無ければ `no pending agent sessions`、dry-run相当のwould launchがあれば `would launch <N>, skipped <N>`、実行時は `launched <N>, skipped <N>`。
  - restart要求でserverがclientへshutdownを通知する時のreasonは `restart`。通常shutdownは `server is shutting down`。
- **受け入れ条件**:
  - workspace list空白を右クリックすると `ContextMenuKind::SidebarBlank` 相当のmenuが開く。
  - workspace list空白を500ms以内に同一座標でdouble clickすると同じmenuが開く。
  - agent panel空白、section header、scrollbar、density toggle、footerでは sidebar blank menu が開かない。
  - `Stop server` 選択直後は `should_quit` がfalseのまま確認ダイアログになり、confirm後にtrueになる。
  - `Restart` 選択直後は `should_quit` と `request_restart` がfalseのまま確認ダイアログになり、confirm後に両方trueになる。
  - `Restore agents...` 選択直後は restore要求がfalseのまま確認ダイアログになり、confirm後にrestore要求だけtrueになり、server quitしない。
  - confirmation overlayに上記title/detail/button labelが正確に表示される。
  - restart shutdownを受けたclientは同じsession名で再起動し、standalone restartは `--no-session` 付きで再起動する。
- **実装方針**: 本家に sidebar blank menu と `ConfirmDanger` 相当のmodalが無いため fork移植が必要。既存context menu / modal / server shutdown reason の仕組みに、blank hit-test、danger pending state、confirm overlay、restart relaunch処理を追加する。
- **デグレ判定**:
  - workspace list空白から session 操作menuを開けない。
  - `Stop server`、`Restart`、`Restore agents...` が確認なしで即実行される。
  - `Detach` がserver quitやrestartを伴う。
  - 確認文言、button label、keyboard hintが変わり、危険操作の意味が曖昧になる。
  - `Restart` のshutdown reasonが `restart` ではなくなり、client側の自動再起動経路が動かない。

### Workspace複製
- **元コミット**: 2fe5eb5
- **分類**: CORE-UI
- **目的**: 既存workspaceのpane構成、tab構成、作業ディレクトリを保ったまま、別workspaceとして同じ作業環境をすぐ複製したい。
- **UI挙動**:
  - workspace context menu に `Duplicate` を表示する。2fe5eb5時点では `New worktree`、`Open worktree`、`Remove worktree` の後、`Rename` と `Close` の前に表示される。
  - `Duplicate` を選ぶと、対象workspaceを元に新しいworkspaceを作り、workspace listの末尾に追加する。
  - 複製後は新しく作ったworkspaceへactive/selectedを切り替え、terminal modeへ戻る。
  - 複製される内容は、tab数、active tab、pane split layout、各paneのcurrent working directory、tab label。
  - 複製workspaceには新しいworkspace idを割り当てる。同一workspace idの再利用はしない。
  - 複製に失敗した場合は設定/診断表示として `duplicate workspace failed: <error>` を出す。
- **受け入れ条件**:
  - 2 tab構成、1つ目tabが2 pane split、2つ目tab名が `logs` のworkspaceを複製すると、新workspaceも2 tab、同じactive tab、同じsplit pane数、`logs` tab名を持つ。
  - 元workspace内の各pane cwdが `/tmp/.../one`、`/tmp/.../two`、`/tmp/.../three` のように異なる場合、複製後workspace内のpane cwdにも同じ3つが含まれる。
  - 複製後、workspace数が1増え、active workspaceが複製されたworkspaceになる。
  - 存在しないworkspace indexから複製しようとすると、ユーザーに `duplicate workspace failed: workspace not found` 相当の診断が表示される。
- **実装方針**: 本家に `duplicate_workspace` 相当が無いため fork移植が必要。既存のsession snapshot capture/restoreとpane runtime生成の仕組みを使い、対象workspaceだけを一時snapshot化して新規workspaceとしてrestoreする。
- **デグレ判定**:
  - `Duplicate` がworkspace context menuに無い。
  - 複製後にactive workspaceが新しいworkspaceへ切り替わらない。
  - tab数、active tab、pane layout、tab label、pane cwdのいずれかが元workspaceと一致しない。
  - 複製workspaceが元workspace idを再利用し、session保存やAPI上で同一workspaceとして扱われる。

### ワークスペースGit統計表示
- **元コミット**: 053bd55, d35f348, f0f4385, 848a8eb
- **分類**: CORE-UI
- **目的**: サイドバーだけで各 workspace のブランチ、upstream 差分、作業ツリー差分を確認できるようにする。狭い表示では workspace 名を最優先に残し、git 情報が名前を押しつぶさないようにする。
- **UI挙動**:
  - workspace カードの git 行または slim 行に、upstream 差分を `↑2` / `↓1`、作業ツリー行数差分を `+123` / `-11` の形式で表示する。
  - `↑N` と `+N` は緑、`↓N` と `-N` は赤で表示する。0 件の値は表示しない。
  - full 密度で branch がある workspace は 2 行表示にし、1 行目は状態アイコン、workspace 番号、workspace 名、2 行目は 4 桁分のインデント後に git 情報を表示する。
  - full 密度の git 行では、表示順を `↑N` / `↓N` / `+N` / `-N` / branch 名にする。例: `    ↑2 ↓1 +123 -11 main`。
  - slim 密度では workspace カードを 1 行に保ち、同じ行に workspace 名と git 情報を表示する。
  - slim 密度の狭い行では workspace 名を優先する。幅が足りない場合は diff stats、upstream 矢印、branch 名の順で落とす。
  - branch 名や workspace 名は幅に収まらない場合、末尾を `…` で省略する。
  - 作業ツリー差分は `git diff --numstat HEAD --` 相当の text file additions/deletions 合計を表示し、binary file の `-` は合計しない。
- **受け入れ条件**:
  - branch `main`、ahead/behind `(2, 1)`、diff stats `(123, 11)` の workspace が `↑2`、`↓1`、`+123`、`-11`、`main` を表示する。
  - 表示順は `↑2` と `-11` が branch 名 `main` より左にある。
  - slim 密度、幅 28、workspace 名 `very-long-space-name`、branch `main`、ahead/behind `(2, 1)`、diff stats `(203, 31)` の行では `very-long-space-name` が残り、`+203` と `-31` は表示されない。
  - branch あり full 密度の workspace では git 行が workspace 名の次行に出る。
  - diff stats が `None` または additions/deletions が 0 の場合、余計な `+0` / `-0` は表示しない。
- **実装方針**: 本家に workspace の branch/ahead/behind 表示がある場合はその取得・更新周期に乗せる。ただし本家 `WorkspaceGitStatus` には作業ツリー行数差分が無いので、workspace git status に additions/deletions を追加し、サイドバー描画で upstream label と同じメタ情報として扱う必要がある。
- **デグレ判定**:
  - workspace 名が狭い slim 行で `…` だけになる。
  - `+N` / `-N` が表示されない、または branch 名より右ではなく左に寄らない。
  - 0 件の diff label が表示される。
  - upstream label と diff label の色が増減で区別されない。

### Slim密度とAgents並び替え
- **元コミット**: b1d8526, 845a538
- **分類**: PARTIAL
- **目的**: workspace 数が多い時にサイドバーの表示密度を上げる。agents 一覧は注意が必要な pane を上へ寄せ、見落としを減らす。
- **UI挙動**:
  - 設定キーは `[ui] workspace_panel_density`。保存値は `"full"` または `"slim"`、default は `"full"`。
  - expanded sidebar の workspace panel ヘッダー右端に密度トグルを表示する。現在値は `[full]` または `[slim]`。
  - 密度トグルを左クリックすると `full` と `slim` を切り替え、workspace scroll を 0 に戻し、設定ファイルにも `workspace_panel_density = "slim"` または `"full"` として保存する。
  - `slim` では branch がある workspace も 1 行カードにする。`full` では branch がある workspace は 2 行カードにする。
  - 設定キー `[ui] agent_panel_scope` は `"all"` または `"sort"` を受け付ける。default は `"all"`。古い `"current"` は起動時に all 相当として扱う。
  - agents panel のヘッダー右端に scope トグルを表示する。現在値は `[all]` または `[sort]`。
  - agents scope トグルを左クリックすると `"all"` と `"sort"` を切り替え、agent scroll を 0 に戻し、設定ファイルへ保存する。
  - `"sort"` では agents を「Blocked」「未確認 Idle」「Working」「確認済み Idle」「Unknown」の順に並べる。同じ bucket 内は元の走査順を保つ。
  - agents panel は collapsed workspace section に属する pane を表示しない。
- **受け入れ条件**:
  - default config で `workspace_panel_density` は full、`agent_panel_scope` は all になる。
  - `workspace_panel_density = "slim"` を読んで起動すると、branch あり workspace のカード高さが 1 行になる。
  - 密度トグルをクリックすると full から slim に変わり、workspace scroll が 0 になる。
  - `agent_panel_scope = "sort"` では Blocked、未確認 Idle、Working、確認済み Idle の順に表示される。
  - `agent_panel_scope = "current"` が既存 config に残っていても、current workspace 限定表示には戻らず all 表示になる。
- **実装方針**: 本家に `ui.agent_panel_sort` など agents の sorting API がある場合はそれを使う。足りない部分は workspace panel density とヘッダー右端トグルで、これは本家 sidebar state/config/render/input に追加する必要がある。
- **デグレ判定**:
  - `[full]` / `[slim]` のトグルが表示されない、またはクリックしても設定に保存されない。
  - slim で branch あり workspace が 2 行のままになる。
  - sort 表示で Blocked や未確認 Idle が Working より下に残る。
  - collapsed section の agents が agents panel に残る。

### Pane target表示
- **元コミット**: 6c0cf33, 78c0f64, e3acfae
- **分類**: CORE-UI
- **目的**: agent sidebar から CLI の target に使う pane ID をその場で読めるようにする。workspace 行にも番号を出し、サイドバー上の対象を人間が短時間で識別できるようにする。
- **UI挙動**:
  - workspace 行の先頭は状態アイコン、半角スペース、workspace 番号、半角スペース、workspace 名の順に表示する。例: `· 1 one`。
  - workspace 番号は 1 始まりで、太字かつ overlay 色で表示する。
  - active workspace の左端には accent 色の `▌` を表示する。
  - agents panel の各 agent 行は、状態アイコンの後に global pane ID を `%22` の形式で表示し、その後に workspace/tab/agent label を表示する。
  - agents panel には workspace-local ID の `2-1` や `1-2` は表示しない。
  - `herdr pane read` など pane command の target として、表示された `%22` 形式をそのまま使える。
- **受け入れ条件**:
  - workspace 1 の行が `· 1 one` の並びで始まり、状態アイコンと `1` の間にスペースがある。
  - pane raw id が 22 の agent 行に `%22` が表示される。
  - 同じ agent 行に `1-1` や `2-1` が表示されない。
  - agent label が長い場合でも、pane ID 表示用の幅が確保され、primary label 側が省略される。
- **実装方針**: 本家に pane target 解決や `pane.list` の global ID がある場合はそれに合わせ、UI では pane の stable/global target を `%<global>` として agent row の固定 prefix に置く。本家が workspace-local short target も持っていても、sidebar 表示は global ID のみに絞る。
- **デグレ判定**:
  - agent 行から `%N` が消える。
  - workspace-local ID が再び agent 行に出る。
  - workspace 番号が状態アイコンと詰まって `·1` のように見える。

### Workspaceメニューとサイドバー操作
- **元コミット**: 5b13801, 845a538
- **分類**: CORE-UI
- **目的**: touch 操作や mouse-first 操作で、workspace 作成・menu 起動・workspace context menu をサイドバーから迷わず実行できるようにする。
- **UI挙動**:
  - active workspace card を左クリックまたはタップして release すると、workspace を切り替えずに、その位置で workspace context menu を開く。
  - inactive workspace card をクリックした場合は従来どおりその workspace に切り替える。
  - workspace panel の footer は左に `[menu]`、右に `[new]` を表示する。
  - footer `[new]` は section 未指定の通常 workspace を作成する。
  - footer `[menu]` は global menu を開く。global menu の主な表示順は `New workspace`, `New tab`, `settings`, `keybinds`, `reload config`, `vim mode on/off`, `what's new` または `update ready`, `Restore agents...`, `detach`, `Stop server`, `Restart`。
  - global menu に attention badge がある場合、footer `[menu]` の左に accent 色の `● ` を付ける。
  - workspace list や agents panel の空白部分を右クリックまたはダブルクリックしても、旧 sidebar blank context menu は開かない。
- **受け入れ条件**:
  - active workspace card を press/release すると mode が context menu になり、menu kind は対象 workspace になる。
  - active workspace card tap 後も active workspace は変わらない。
  - footer の `[menu]` rect は `[new]` rect より左にある。
  - footer `[new]` click で `request_new_workspace = true` になり、requested section は `None` になる。
  - blank sidebar right click と double click で context menu が作られない。
- **実装方針**: 本家の mouse hit area、workspace context menu、global menu action に乗せる。blank area 用の別 context menu を増やさず、global `[menu]` に actions を集約する。
- **デグレ判定**:
  - active card tap が workspace 切り替え扱いになり context menu を開かない。
  - `[menu]` と `[new]` の左右が逆になる。
  - blank area right click で旧式の別メニューが出る。

### Workspaceセクションと幅プリセット
- **元コミット**: 845a538
- **分類**: PARTIAL
- **目的**: workspace を favorites/work/personal/spaces に分けて折りたたみ、サイドバー幅も touch 操作だけで narrow/normal/wide に切り替えられるようにする。
- **UI挙動**:
  - workspace section は `⭐ favorites`, `💼 work`, `🏠 personal`, `spaces` の順に表示する。
  - section header は `▾` または `▸`、半角スペース、section label の順に表示する。
  - section header をクリックすると、その section を collapse/expand する。
  - 空の `⭐ favorites` / `💼 work` / `🏠 personal` section は、`[new]` を見せるためだけには表示しない。
  - section に workspace が 1 件以上ある場合、header 右端に underlined bold の `[new]` を表示する。クリックすると、その section に所属する新規 workspace 作成を要求する。
  - section header と配下 workspace card の間には 1 blank row を置く。workspace card の highlight と hit area は section header 幅と同じ full width を保つ。
  - workspace context menu には section 割り当てとして `Favorite`, `Work`, `Personal`, `No section` を持たせ、section actions は separator の後ろにまとめる。
  - agents panel footer 左端に幅プリセットボタンを表示する。button rect は幅 8、label は ` NARROW `、` NORMAL `、` WIDE `。
  - 幅プリセットボタンをクリックすると、現在幅が narrow 以下なら normal、normal 以下なら wide、それ以外なら narrow に切り替える。
  - narrow 幅は `sidebar_min_width`、normal 幅は `default_sidebar_width` を min/max に clamp した値、wide 幅は `ceil(sidebar_max_width * 2 / 3)` と `normal + 1` の大きい方を min/max に clamp した値にする。
- **受け入れ条件**:
  - favorite と work の workspace がある場合、`⭐ favorites` header が `💼 work` header より上に出る。
  - work section に workspace がある場合、work header の右端 `[new]` click で requested section が Work になる。
  - personal section に workspace が無い場合、personal header は表示されない。
  - collapsed section の workspace card は表示されず、その section の agents も agents panel から消える。
  - `sidebar_min_width = 18`, `default_sidebar_width = 26`, `sidebar_max_width = 36`, 現在幅 26 の時、幅プリセット click は 27、次に 18、次に 26 へ遷移する。
- **実装方針**: 本家に workspace metadata や section/collapse 相当がある場合はその state に乗せる。足りない場合は workspace に section 属性、collapsed section set、section header hit area、新規 workspace 作成時の target section、sidebar width preset state を追加する。
- **デグレ判定**:
  - section 順序が favorites/work/personal/spaces 以外になる。
  - 空 section が header だけで表示される。
  - section header の `[new]` が通常 workspace を作る、または対象 section に入らない。
  - wide preset が `sidebar_max_width` いっぱいまで跳ねる。

### Worktree名とコピー状態
- **元コミット**: 5699ca3, a1dedf2
- **分類**: PARTIAL
- **目的**: linked Git worktree を開いた時も同じ repo の workspace として認識しやすくする。text selection copy 後は、コピーが成功したことと行数を短時間だけ視覚確認できるようにする。
- **UI挙動**:
  - workspace に custom name が無い場合、通常 Git repo は repo root の directory name を workspace 名にする。
  - linked Git worktree の場合、worktree checkout directory name ではなく main repository name を workspace 名にする。
  - root pane の OSC title は、その pane が agent terminal と判定される場合だけ workspace 名に `workspace-OSC title` 形式で付ける。plain shell の `user@host` のような OSC title は workspace 名に付けない。
  - mouse selection copy が clipboard write queue に成功したら、sidebar の最下行に ` Copied 1 line` または ` Copied 3 lines` を表示する。
  - copy status は緑、太字、背景 `surface_dim` で表示し、sidebar 幅から 1 column 引いた rect に描画する。
  - copy status は 2 秒後に消える。persistent server session でも deadline 経過後に残らない。
- **受け入れ条件**:
  - linked worktree の cwd から derive した workspace 名が main repo directory name になる。
  - plain shell root pane title `kazuph@host:pion` は workspace 名に追加されない。
  - agent root pane title `planner` は workspace 名に `pion-planner` として反映される。
  - 3 行 selection copy 後、sidebar 最下行に `Copied 3 lines` が表示される。
  - 1 行 selection copy 後、文言は `Copied 1 line` になり plural `lines` にならない。
  - 2 秒の deadline 後、copy status は消える。
- **実装方針**: 本家に `GitSpaceMetadata.label` がある場合、worktree main repo label はそこへ寄せる。copy status は本家の generic toast がある場合でも、line count と sidebar bottom placement が足りなければ selection copy 専用 status と deadline を追加する。
- **デグレ判定**:
  - worktree checkout directory name が workspace 名になる。
  - plain shell の OSC title が workspace 名に混ざる。
  - copy 後の表示が行数なしの `Copied` だけになる。
  - `Copied N lines` が sidebar 最下行以外に出る、または消えずに残る。

## G3. Agent復元（exact session restore・fail-closed）

### paneごとのexact agent session復元
- **元コミット**: 7cf02fa, 573f85e, 6ee56ec, 0c8f7f0
- **分類**: PARTIAL
- **目的**: 同じcwdから起動した複数のClaude Code/Codex paneを、cwd単位の「最新セッション」ではなく、それぞれのpaneで最後に動いていた会話へ戻す。session idが確定できないpaneは復元しないことで、別paneの会話を誤って起動する事故を防ぐ。
- **UI挙動**:
  - 本家のnative agent resume（`agent_resume` / `[session] resume_agents_on_restore`）を土台にする。fork差分は、pane単位で観測済みのsession idだけをresume対象にし、cwd-latestや`--last`へ落とさない点。
  - `herdr pane report-agent <pane_id> --source ID --agent LABEL --state idle|working|blocked|unknown [--message TEXT] [--custom-status TEXT] [--seq N] [--title TEXT] [--session-id ID]` を受け付ける。`--session-id` はそのpaneの復元用session id、`--title` はpane/workspace名に出す短いタスク名。
  - `pane.report_agent` APIは `title` と `session_id` を受け付ける。`title` は空白trim後に空なら無視し、agentのtask titleとしてpane title/workspace title/通知titleに使う。通常のOSC titleだけではworkspace名を変えない。
  - 復元コマンドの組み立ては、agentごとのtemplateに `{session_id}` を必須にする。fork時点のbuiltinは `claude = "claude --resume {session_id}"` と `codex = "codex resume {session_id}"`。
  - session idは、明示報告が最優先。明示報告がないplain processでは、実行中processのcmdlineから `claude --resume <id>` / `claude --resume=<id>` / `codex resume <id>` を読む。
  - plain Claude/Codexでcmdlineにresume idがない場合だけ、実行中processのcwdとprocess start timeに一致するsession fileを探す。Codexは `$HOME/.codex/sessions/**/*.jsonl` の先頭 `session_meta.payload.id` または `session_meta.payload.session_id`、`payload.cwd`、`payload.timestamp` を使う。Claudeは `$HOME/.claude/projects/**/*.jsonl` の先頭64行以内にある `sessionId`、`cwd`、`timestamp` を使う。
  - session file recoveryは、session timestampとprocess start timeの差が120秒以内で、候補が1件だけのときだけ採用する。0件または2件以上ならsession id未確定として扱う。
  - session idはpaneのsession snapshotにも保存し、別途pane ledgerにも保存する。ledger entryはpane id、terminal id、workspace id、tab id、cwd、agent、session id、observed_at、source、titleを持ち、pane/workspace/tabを閉じたら対応entryを削除する。
  - 同じcwdにある複数paneは、snapshot上もledger上も別session idを保持する。pane idやtab/workspaceが違うsession idを混ぜない。
  - `herdr agent restore [--dry-run]` は復元対象paneごとの結果を返す。action itemは `pane_id`、`agent`、`status`、任意の `command`、任意の `reason` を持ち、`status` は `launched` / `would_launch` / `skipped`。
  - dry-runではコマンドをpaneへ入力せず、起動可能なら `status = "would_launch"` と具体的な `command` を返す。通常実行ではpane shellへコマンド文字列を入力し、短いsubmit delay後にEnterを送る。成功時は `status = "launched"`。
  - live agentが既に見えているpaneは二重起動せず、`reason = "agent already running"` でskipし、そのpaneのpending resume状態を消す。
- **受け入れ条件**:
  - 同一cwdの3つのCodex paneにそれぞれ別session idが記録されている状態でsnapshot/restoreしても、3paneが同じ「cwd最新」会話へ潰れない。
  - `herdr agent restore --dry-run` が `codex resume 019ef3a2-749c-7b52-b324-2c20cb0b2379` のようにpane固有id入りコマンドを返す。
  - `pane.report-agent ... --session-id bad;id` のようなunsafe idは記録されず、session dirtyにもならない。
  - terminal側のsession idが消えてもledgerに同じpane/workspace/tab/agentのsafe idがあれば、次のsnapshotには同じsession idが残る。
  - pane死亡後に検出状態が一度消えても、restorable agent paneのsnapshotには同じagent/session idが残り、再度restore planが同じresume commandを作る。
- **実装方針**: 本家 `agent_resume` のpending resume plan、official integrationが報告するsession ref、`[session] resume_agents_on_restore` を利用する。足りない差分は、plain process観測からのpane単位session id補完、workspace/tab/pane keyのledger、session fileのcwd+start-time一意一致、`pane.report_agent`の`title/session_id`相当メタデータの保存である。本家に `pane.report_metadata` がある場合、表示用titleはそちらへ寄せ、lifecycle/session stateとは分離する。
- **デグレ判定**:
  - session idがないpaneをcwd最新、`resume --last`、`--last`、mtime最新sessionで復元したら劣化。
  - 同じcwdの複数paneが同じ会話に復元されたら劣化。
  - `pane.report-agent --session-id` またはcmdline観測で得たpane固有session idがsnapshot後に失われたら劣化。
  - `--dry-run` がpaneごとの具体的command/skip理由を返さない、または実行してしまうなら劣化。

### session id欠落時のfail-closed再起動警告
- **元コミット**: 4e764ca, 8d43e8c
- **分類**: PARTIAL
- **目的**: 復元に必要なsession idが欠けたままHerdr serverをrestartすると、AI paneが別会話へ戻るか復元不能になる。restart前に危険なpaneを明示し、復元時はunsafeなtemplate/idを拒否する。
- **UI挙動**:
  - session idとして許可する文字列は、空でない、128文字以内、先頭が `-` ではない、`last` ではない、`--last` ではない、かつASCII英数字または `-` / `_` / `.` のみ。
  - restore templateは `{session_id}` を含む必要がある。template内に独立tokenとして `--last` が含まれる場合は拒否する。
  - `{session_id}` のないtemplate、unsafe session id、session id欠落はいずれもrestore commandを生成せず、restore actionは `status = "skipped"` / `reason = "no resumable session found"` になる。
  - Herdr restart確認ダイアログで、session idのないagent paneがある場合、通常の `Restart Herdr?` ではなく `Restart with missing agent sessions?` をtitleにする。
  - 同ダイアログの説明文は `These AI panes do not have a recorded session id:`。
  - 欠落pane一覧は最大10件ぶん高さを増やして表示する。各行は ` space <workspace_number> <workspace_label> pane <pane_label> <agent> title=<title-or-> cwd=<cwd> reason=<reason>`。長すぎる行は末尾を `…` でtruncateする。
  - `reason` は、terminalにunsafe idがある場合 `invalid terminal session id`、ledgerに同agentのentryはあるがunsafe idの場合 `invalid ledger session id`、どちらにもsafe idがない場合 `missing session id`。
  - confirm/cancelボタンは既存danger dialogと同じで、confirm側はEnterアイコン `↵` とaction label、cancel側は `esc` / `cancel`。
- **受け入れ条件**:
  - `claude --resume --last`、`codex resume {session_id} --last`、`codex resume --last`、`codex resume last` は復元コマンドにならない。
  - session idなしのClaude paneがある状態でrestart確認を開くと、titleが `Restart with missing agent sessions?` になり、そのpaneのspace/pane/agent/title/cwd/reasonが1行で表示される。
  - safe idを持つterminalまたは同pane ledgerがあるagent paneは、欠落一覧に出ない。
  - 欠落paneがないrestart確認では既存の `Restart Herdr?` / `Restarts the Herdr server. The saved session can be restored.` が維持される。
- **実装方針**: 本家 `agent_resume` のsession ref検証に、forkの「unsafe id拒否」と「missing session id一覧」を上乗せする。本家のnative resumeが既にsession ref限定でも、restart前の可視警告と `--last` / id-less template全面拒否を残す。
- **デグレ判定**:
  - session id欠落paneがあるのにrestart dialogが通常文言のままなら劣化。
  - 欠落pane一覧にcwdとreasonが出ないなら劣化。
  - `{session_id}` なしtemplateや`--last`入りtemplateを許可したら劣化。

### restorable agent paneの保持
- **元コミット**: 7a982bb
- **分類**: PARTIAL
- **目的**: 復元可能なagent paneで子processが終了したとき、pane/workspaceごと消えると、次のresume対象とpane identityが失われる。session idを持つagent paneはshellへ戻った空paneとして残し、後続の復元を可能にする。
- **UI挙動**:
  - agent session idとagent種別がsafeに記録済み、またはpending restoreにsafe session idと解釈可能なagent labelがあるpaneでchild processが終了した場合、paneを閉じない。
  - 残したpaneはagent検出、hook authority、fallback stateをclearし、表示状態は `unknown` 相当に戻す。runtimeはshutdownされるがpane identityとterminal stateは残る。
  - そのpaneのsnapshotには、検出状態が消えた後でもagent/session idが残る。
  - session idだけでなくagent種別も一致していることを保持条件にする。別agentのledger/session idを交差利用しない。
- **受け入れ条件**:
  - Codex paneにsafe session idとagent種別がある状態でPaneDiedが起きても、workspace数とpane stateは残り、terminal stateはUnknownになり、同じsession id/agentが保持される。
  - pending restoreだけを持つpaneでもsafe idとagent labelがあればpaneが残る。
  - snapshot capture後、`agent_restore.session_id` が同じidとして残る。
- **実装方針**: 本家 `agent_resume` が「resumed agent exits -> shell fallback」を持つため、その挙動に合わせる。fork差分は、終了時にもpane identityとexact session idを消さないこと、pending resumeとledgerから次回snapshotへ同じsession refを戻すこと。
- **デグレ判定**:
  - restorable agent paneのchild exitでpane/workspaceが消えたら劣化。
  - child exit後のsnapshotからsession idまたはagent種別が消えたら劣化。
  - agent種別が違うledger entryで復元可能扱いしたら劣化。

### inactive shell paneの通常終了cleanup
- **元コミット**: 9322429
- **分類**: CORE-UI
- **目的**: 古いagent session ledgerが残っているだけの普通のshell paneまで保護すると、ユーザーが終了したshell/workspaceが閉じずに残る。現在agentとして見えていないpaneは通常のclose挙動に戻す。
- **UI挙動**:
  - pane死亡時に保護するのは、terminalのeffective agent labelがあるpaneで、かつledgerまたはrestorable session情報がある場合だけ。
  - terminalに過去のagent session id/agent/ledgerが残っていても、現在のeffective agent labelがないshell paneは保護しない。既存のpane close/workspace close処理に進む。
  - shell paneを閉じても、古いledger entry自体はこのcleanup条件だけでは削除しない。ledger削除はpane/tab/workspaceの明示close操作に従う。
- **受け入れ条件**:
  - 2workspaceある状態で、1つ目のroot paneがshell状態、かつ古いCodex session id/ledgerだけを持っている場合、PaneDiedで1つ目workspaceが閉じ、2つ目workspaceだけが残る。
  - 同条件でledger entryは残る。
  - effective agent labelがあるrestorable paneは、前項どおりPaneDiedで残る。
- **実装方針**: 本家のagent resume shell fallbackと通常pane lifecycleに、保護判定の境界を追加する。保護条件は「過去にagentだった」ではなく「今agent paneとして扱う根拠がある」に限定する。
- **デグレ判定**:
  - 古いledgerだけを持つshell paneが終了しても閉じないなら劣化。
  - 現在agentとして見えているrestorable paneまで閉じるなら劣化。

### agent restore結果toastの空状態明確化
- **元コミット**: bcdaa3e
- **分類**: CORE-UI
- **目的**: 手動restore実行後に対象がなかったとき、「何もしていない」のか「既にagentが動いている」のかをユーザーが区別できるようにする。
- **UI挙動**:
  - 手動restore要求完了時のtoast titleは `agent restore`。
  - restore対象actionが0件かつ実行中agentも0件ならcontextは `no pending restore`。
  - restore対象actionが0件かつ実行中agentがN件ならcontextは `no pending restore, N already running`。
  - dry-runでlaunch可能actionがある場合は `would launch <count>, skipped <count>`。
  - 通常実行では `launched <count>, skipped <count>`。
- **受け入れ条件**:
  - pending restoreがなく、agentも0件ならtoastが `agent restore: no pending restore` になる。
  - pending restoreがなく、agentが3件見えているならtoastが `agent restore: no pending restore, 3 already running` になる。
  - dry-runで2件起動可能/1件skipならcontextが `would launch 2, skipped 1` になる。
- **実装方針**: 本家のnotification/toast APIがある場合でも、restore操作の結果summary文字列はこの仕様に合わせる。表示先は本家のtoast deliveryや`notification.show`に寄せてよい。
- **デグレ判定**:
  - action 0件時に `no pending agent sessions` など、running agent countを含まない曖昧文言へ戻ったら劣化。
  - dry-runなのに `launched` 表記になる、または通常実行なのに `would launch` 表記になるなら劣化。

### Claude spinner化石の15秒失効
- **元コミット**: 007d0c7
- **分類**: CORE-UI
- **目的**: Claude Codeが完了後に古いspinner行やinterrupt hintをtranscriptへ残すと、paneが永遠にworking表示になる。live spinnerだけをworkingとして維持し、動かない化石行はidleへ戻す。
- **UI挙動**:
  - Claude Code paneで、prompt box上にspinner行、`esc to interrupt`、`ctrl+c to interrupt` が見える場合、それらの行をactivity fingerprintとして扱う。
  - spinner行は、先頭がClaude spinner glyph（例: `·`, `✱`, `✢`, `✻`, `✽`, `✨` など）で、直後に空白があり、本文にellipsis `…` と英数字が含まれる行。
  - raw検出がClaudeの`working`でも、activity fingerprintが15秒間変化しない場合は、そのworking evidenceをfossilized scrollbackとみなし、pane状態を`idle`へ落とす。
  - live spinnerはglyphや経過時間counterが毎秒変わるため、fingerprintが変化し続ける限り`working`のまま。
  - fingerprintが変化した新ターンは即座に`working`へ戻る。
  - Claude以外のagent、Claudeでもfingerprintのないworking理由、またはraw stateがworking以外の場合は、この15秒失効を適用しない。
  - `✻ Cooked for 13m 55s` のような完了summaryだけではworking扱いしない。
- **受け入れ条件**:
  - 完了済みtranscriptに `✢ ...… (13m 47s · thinking)` とprompt boxが残っている画面は、初回はworkingでも15秒後にidleへ落ちる。
  - 60秒間spinner glyphまたはtimerが変化し続けるClaude画面はworkingを維持する。
  - 一度idleへ落ちた化石の下に新しいspinner行が増えてfingerprintが変わったら、次の検出でworkingへ戻る。
  - CodexなどClaude以外のworking検出はこのfilterでidleへ落ちない。
- **実装方針**: 本家に同等のactivity fingerprintがない場合、screen detection層にClaude専用のtime-based stale filterを追加する。本家側のagent state/reporting APIはそのまま使い、表示状態の最終確定前にfilterする。
- **デグレ判定**:
  - Claude paneが完了後15秒を超えても古いspinner行だけでworkingのままなら劣化。
  - live spinner中のClaude paneが15秒でidleへ落ちたら劣化。
  - 完了summary行だけでworking表示になるなら劣化。

## G4. 通知

### paneコマンド終了通知
- **元コミット**: 2650c3b
- **分類**: PLUGIN-ABLE
- **目的**: 長時間コマンドを別paneで実行したまま、依頼元paneへ終了・exit code・末尾ログ・job log参照を戻せるようにする。AIが別paneへ作業を投げたあと、完了確認のために目視巡回しなくてよい状態を作る。
- **UI挙動**:
  - CLIに `herdr pane run-notify <pane_id> <command>` を追加する。`<command>` は `<pane_id>` 以降の引数を空白結合した文字列で、`herdr pane run-notify <pane_id> -- <command>` のように先頭 `--` がある場合はそれを区切りとして無視する。
  - 引数不足、または `--` の後にコマンドがない場合は stderr に `usage: herdr pane run-notify <pane_id> <command>` を出して exit code 2。
  - 実行元paneは `HERDR_PANE_ID` 優先、なければ呼び出しプロセスのsessionから解決する。解決できない場合は失敗し、別paneへ推測通知しない。
  - 指定した target pane へ、現在の `herdr` 実行ファイルを使った内部runner `__pane-notify-run --parent <parent> --target <target> --job-id <job-id> -- <command>` を入力し、Enterまで送る。target pane上ではコマンド出力が通常の stdout/stderr として流れる。
  - job id は `job-<unix_ms>-<process_id>` 形式。job id に許可する文字は ASCII 英数字、`-`、`_` のみ。
  - job log は `$XDG_STATE_HOME/herdr/job-logs/<job_id>.log`、または `HOME/.local/state/herdr/job-logs/<job_id>.log` に保存する。先頭に `job_id:`, `target_pane:`, `parent_pane:`, `command:`, `started_unix_ms:` を書き、出力は `[stdout] ` / `[stderr] ` prefix付きで記録し、末尾に `finished_unix_ms:` と `exit_code:` を書く。
  - CLIに `herdr pane job-log <job_id>` を追加する。job id がない、複数ある、または path 文字を含む場合は stderr に `usage: herdr pane job-log <job_id>` を出して exit code 2。正常時は該当log全文を stdout に出す。
  - runner終了時、parent paneに `pane.notify` を送ってクリック可能な Herdr toast を表示する。toast title は `pane job exited: <exit>`、signal終了は `pane job exited: signal`。
  - run-notify toast context は `<target> · <job_id> · <command最大80文字> · tail: <末尾sample最大120文字> · log: <log path最大120文字>`。tail sample が空なら `tail:` 部分は出さない。古い `[herdr] pane job exited` 形式のshell payloadは通知本文に出さない。
  - 実行中の末尾sampleは最後の 1200 文字だけ保持する。
  - `herdr pane help` に `herdr pane run-notify <pane_id> <command>`、`herdr pane job-log <job_id>`、`pane run-notify streams output in the target pane and reports exit with a Herdr toast plus job log` を表示する。
- **受け入れ条件**:
  - `herdr pane run-notify p_2 -- printf '%s' hello` が target pane にrunnerを送信し、target pane上で `hello` が表示される。
  - コマンド終了後、parent pane側に title `pane job exited: 0` の toast が出て、context に target pane id、job id、`tail: hello`、`log:` が含まれる。
  - `herdr pane job-log <job_id>` が metadata、stdout/stderr prefix、exit_code を含むlog全文を表示する。
  - `../secret` や空文字の job id は `pane job-log` で拒否される。
  - 1300文字以上の出力でも通知sampleは最後の1200文字を元に作られ、toast contextは各フィールド上限で切り詰められる。
- **実装方針**: 本家の `pane run` 相当の入力送信、`notification.show`、可能なら `pane.notify` 相当のtarget付き通知APIに乗せる。plugin化する場合も、parent pane解決、target pane実行、job log保存、終了時通知、job log参照コマンドの一式を外部runnerとしてまとめればよい。
- **デグレ判定**:
  - コマンド出力がtarget paneにstreamされず、通知だけになる。
  - parent paneが明示解決できない時にfocus中paneや最新paneへ通知する。
  - job logが保存されない、または `herdr pane job-log <job_id>` で読めない。
  - 通知がexit code、target pane、job id、tail sample、log pathのいずれかを欠く。
  - shell payloadや内部runner文字列がユーザー向け通知本文に露出する。

### agent通知タイトルと本文抽出
- **元コミット**: ef2a4ba, 6f638ec, 72ad4ad
- **分類**: CORE-UI
- **目的**: background agent通知が `pi finished` のようなagent種別だけの表示や罫線だけの本文にならず、「どのworkspace/paneの、最新のAI応答なのか」を通知だけで判断できるようにする。
- **UI挙動**:
  - agent状態変化通知は active tab では抑制する。background paneで `Blocked` になった時は `NeedsAttention`、作業中から `Idle` へ完了遷移した時は `Finished` のtoast/desktop通知を出す。
  - 通知タイトルは agent label ではなく `<workspace番号> <workspace表示名>`。例: 2番目のworkspace `herdr` のroot pane OSC title が `planner` なら `2 herdr-planner`。
  - workspace表示名にroot pane OSC titleを足す時は `workspace-OSC title` 形式で、hyphenの前後に空白を入れない。
  - 通知本文は対象paneの recent unwrapped text 直近80行から作る。本文最大長は120文字で、超過時は最後の1文字を `…` にする。
  - 本文抽出ではまず各行の連続空白を1つに畳み、box drawing / block element文字を行頭行末から取り除く。`────…` のような罫線だけの行は空行扱い。
  - composer prompt 行 `❯`、`›`、`>` が見つかったら、その行以降を入力欄・statusline chrome として丸ごと捨てる。
  - Codexの `• ` と Claude Codeの `⏺ ` をagent応答markerとして扱い、最後のmarkerから始まる本文を最優先で使う。marker prefix は本文から外し、後続行を空白区切りで連結する。次のagent応答markerまたはchrome行に当たったら収集を止める。
  - 応答markerが見つからない場合は、最後の非空blockからchromeではない最初の行を本文にする。
  - chromeとして本文から除外する行は、通知タイトルと完全一致する行、`gpt-` で始まる行、`›` / `❯` / `>` / `⎿` / `└` / `⏵⏵` / `※` で始まる行、Claude spinner/summary先頭文字 `✻✽✶✢✳✣✤❋✺` の行、`<数字> task(s) (` 形式のtask summary、`worked for `、`% left`、`esc to interrupt`、`ctrl+c to interrupt`、`ctrl+o to expand`、`? for shortcuts`、`/ps to view`、`tokens used`、`disable recaps in /config` を含む行。
  - Herdr内toastは枠付きの4行高で右下に表示する。title行は状態色の `●` と太字title、context行は本文または fallback context を表示する。
  - `[ui.toast] delivery = "off"` がdefault。`herdr` はHerdr内toast、`terminal` は外側terminal通知、`system` はOS通知。legacy `[ui.toast] enabled = true` は `delivery = "herdr"`、`enabled = false` は `off` に読むが、`delivery` があればそれを優先する。
- **受け入れ条件**:
  - Claude Code画面の composer罫線とstatuslineだけが末尾にある場合、通知本文はその上の実本文になる。chromeだけなら本文なしになる。
  - Claude Codeの最後の `⏺ ` 応答とCodexの最後の `• ` 応答は、marker以降の複数行を連結した本文になる。
  - Codexの `gpt-... Context 71% left`、`─ Worked for ...`、入力prompt、quota行は通知本文に出ない。
  - 通知タイトルは `agent finished` / `agent needs attention` ではなく、`2 background` や `2 herdr-planner` のようなworkspace番号付きタイトルになる。
  - 121文字以上の本文は120文字以内になり、末尾が `…` になる。
- **実装方針**: 本家の通知発火点と `notification.show` / toast構造を使いつつ、本文抽出器はfork仕様として移植する。本家に `pane.report_metadata` がある場合、workspace表示名やOSC title取得はそのmetadataに寄せてよいが、通知本文のmarker/chrome除去ルールはUI core側に必要。
- **デグレ判定**:
  - 通知本文に `────`、composer prompt、statusline、quota/context、tool result prefix、recap、task summary が出る。
  - 最新のAI応答ではなく、古いblockや最初のscreen行が本文になる。
  - 通知タイトルがagent labelのみになり、workspace番号とworkspace表示名で場所を特定できない。
  - `workspace -title` のようにhyphen前に空白が戻る。

### agent通知rate limit
- **元コミット**: 515f5a7
- **分類**: PARTIAL
- **目的**: hook報告とscreen検出が短時間に競合してagent状態が揺れた時、同じ質問通知が連発されたり、直後の完了通知が質問通知を上書きしたりしないようにする。
- **UI挙動**:
  - 通知発火判定後、in-app Herdr toast、terminal/system desktop notification、headless server forwarding の全経路で同じrate limitを1回適用する。
  - 同一paneかつ同一kindの通知は10秒以内ならdropする。kindは `NeedsAttention`、`Finished`、`UpdateInstalled` のtoast種別。
  - 任意paneの `NeedsAttention` 通知後10秒間は、同一paneでも別paneでも `Finished` 通知をdropする。質問toastをユーザーがクリックする前に完了toastで埋めないため。
  - 新しいpaneからの `NeedsAttention` は常に通す。別paneの質問は直前の質問や完了通知で抑制しない。
  - 抑制された通知は表示しないだけで、agent状態そのものは更新される。
- **受け入れ条件**:
  - 同じpaneが `Blocked -> Working -> Blocked` と数秒内にflapしても、`NeedsAttention` toast/system通知は1回だけ。
  - pane A の `NeedsAttention` から10秒以内に pane B が `Finished` になっても、表示中の質問toastは置き換わらない。
  - pane A の `NeedsAttention` 後10秒以内でも、pane B / pane C の新しい `NeedsAttention` は表示される。
  - 同じpaneの `Finished` が10秒以内に繰り返されても2回目は表示されない。
  - 10秒を超えた同一pane同一kind通知は再表示できる。
- **実装方針**: 本家には遅延通知系の基盤があるが、同一pane同一kind cooldown と attention shield は不足している。`notification.show` 直前、または通知dispatcherの共通層に pane id / toast kind / timestamp を持つthrottleを置き、UI toast・desktop・headless forwardingが同じ判定を通るようにする。
- **デグレ判定**:
  - in-app toastだけ抑制され、desktop通知やheadless client通知が連発する。
  - `NeedsAttention` の直後に `Finished` がtoastを上書きする。
  - 別paneの新しい `NeedsAttention` まで10秒抑制される。
  - cooldownがpane単位でなく全体単位になり、無関係paneの通知が欠落する。

### 通知クリックでpaneへ移動
- **元コミット**: b64fb1b
- **分類**: PARTIAL
- **目的**: 通知を見てから手動でworkspace/tab/paneを探す手間をなくし、質問や完了が起きたpaneへ1操作で戻れるようにする。Antigravity CLI (`agy`) も通知対象agentとして同じ扱いにする。
- **UI挙動**:
  - CLIに `herdr pane focus <pane_id>` を追加する。引数がない、または余分な引数がある場合は stderr に `usage: herdr pane focus <pane_id>` を出して exit code 2。
  - `pane.focus` は指定paneを含むworkspaceへ切り替え、そのpaneを含むtabへ切り替え、そのpaneへfocusし、modeをTerminalへ戻す。paneが見つからない場合は API error code `pane_not_found`、message `pane <pane_id> not found`。
  - Herdr内toastに target がある場合、toast領域を左クリックすると target workspace/tab/paneへ移動し、toastを消し、modeをTerminalにする。
  - `keys.open_notification_target` はdefault `prefix+o`。Navigate modeでこのactionを押すと現在表示中toastのtargetへ移動し、toastを消し、Terminal modeへ戻る。targetなしtoastでは何もしない。
  - headless/client通知の `ServerMessage::Notify` は `target_pane_id` を持つ。`SystemToast` で target がある場合、client側はOS通知にclick commandを付ける。
  - macOS system通知は `terminal-notifier` が使える場合、`-execute '<current_exe> pane focus <pane_id> >/dev/null 2>&1'` を付ける。terminal activation用に `-activate <terminal bundle id>` も付ける。`terminal-notifier` がない場合は `/usr/bin/osascript` fallbackになり、click focus actionは付かない。
  - terminal通知 (`delivery = "terminal"`) はtitle/bodyだけを外側terminalへ送り、click commandは付けない。
  - `agy` / `antigravity` / `antigravity-cli` を process name と agent label としてAntigravity agentに認識し、表示labelは `agy`。
  - Antigravity画面で `requesting permission for:`、`do you want to proceed?`、または `tab amend` と `edit command` の両方を含む場合は `Blocked`。`ctrl+o to expand`、`managetask`、`task(s)` を含む場合は `Working`。それ以外のAntigravity promptは `Idle`。
  - `[ui.sound.agents]` に `antigravity` のagent別sound overrideを持てる。defaultは `default`。
- **受け入れ条件**:
  - `herdr pane focus <pane_id>` が別workspace・別tabのpaneをactive workspace、active tab、focused paneにする。
  - Herdr内toastをクリックすると対象paneへ移動し、toastが消える。
  - `keys.open_notification_target = "prefix+g"` のように変更した場合、そのkeyで対象paneへ移動できる。
  - macOS `terminal-notifier` 経由のsystem通知コマンド引数に `-execute` と `pane focus '<public pane id>'` が含まれる。
  - `identify_agent("agy")`、`identify_agent("antigravity")`、`identify_agent("antigravity-cli")` がAntigravityを返し、permission prompt画面は `Blocked`、tool activity画面は `Working`、起動直後promptは `Idle` になる。
- **実装方針**: 本家に吸収済みの `pane.focus` とAntigravity検出があるならそれを使う。不足分は、通知payloadへ `target_pane_id` を保持し、Herdr toast click / `keys.open_notification_target` / macOS `terminal-notifier -execute` の3経路を target付きfocus に接続する部分。
- **デグレ判定**:
  - 通知をクリックしてもworkspace/tab/paneが切り替わらない。
  - target pane idが通知payloadから失われ、system通知にfocus commandを付けられない。
  - `keys.open_notification_target` のdefault `prefix+o` または設定上書きが効かない。
  - macOSで `terminal-notifier` があるのに `-execute` が付かない。
  - `agy` がunknown agentになり、permission promptで質問通知が出ない。

### render drain通知の正確化
- **元コミット**: 4fd07f9
- **分類**: CORE-UI
- **目的**: client writer がまだframeを書き終えていないのに「描画queueがdrainされた」とserverへ通知し、通知・clipboard・shutdownなどのcontrol message順序が実際の描画完了より先行する状態をなくす。
- **UI挙動**:
  - client writer は render frame をsocketへ `write_all` で書き込み成功した後にだけ `ClientWriterDrained { client_id }` をserverへ送る。
  - `try_recv` 経路でも、control channel close後に `recv_timeout(5ms)` でrender frameを拾う経路でも同じ順序にする。
  - socket書き込みに失敗した場合、そのframeについて `ClientWriterDrained` を送らずwriter loopを終了する。
  - ユーザーから見える効果は、通知・clipboard・shutdownなど信頼性が必要なcontrol messageが、未送信render frameを追い越したかのように扱われないこと。
- **受け入れ条件**:
  - socket pair testでrender frameをclient側が読めた後に `ClientWriterDrained { client_id: 7 }` がserver eventとして届く。
  - write失敗時にdrained eventが先に送られない。
  - render channelを通常pollする経路とcontrol閉鎖後に短時間待つ経路の両方で、drained通知はwrite成功後に送られる。
- **実装方針**: 本家のclient writer / render transport層に、write成功後だけdrain eventを送る小さな共通関数を置く。通知APIではなくtransport correctnessなのでplugin化しない。
- **デグレ判定**:
  - render frameのwrite前に `ClientWriterDrained` が送られる。
  - write失敗でもdrained扱いになる。
  - 片方の受信経路だけ修正され、もう片方で早期drainが残る。

## G5. Agent起動・pane識別・CLI安全性

### 右クリックからのAgent起動とpane移動
- **元コミット**: 95b812a
- **分類**: PARTIAL
- **目的**: AI agentを起動するたびに「paneを作る→shellでCLI名を打つ→agent扱いにする」という手作業をなくす。pane配置も右クリック対象を基準に左・右・上・下へ明示的に寄せられるようにし、複数agentを並べる初動を短くする。
- **UI挙動**:
  - workspace右クリックメニューの先頭に `New Claude Code agent`、`New Codex agent`、`New Gemini agent` をこの順で表示する。
  - workspaceメニューで上記項目を選ぶと、そのworkspace内に右方向splitの新規paneを作り、対応CLIを起動してagent targetとして登録する。起動コマンドはそれぞれ `claude`、`codex`、`gemini`。
  - pane右クリックメニューには `New Claude Code agent`、`New Codex agent`、`New Gemini agent` を表示する。選択時は右クリックされたpaneを一度focusし、そのpaneを基準に右方向splitを作ってagentを起動する。
  - pane右クリックメニューには `Move to left split`、`Move to right split`、`Move to upper split`、`Move to lower split` を表示する。
  - `Move to left split` は対象paneをroot水平splitの左側、`Move to right split` は右側へ移動する。`Move to upper split` はroot垂直splitの上側、`Move to lower split` は下側へ移動する。
  - 移動後も対象paneがfocusされたままになる。対象pane以外のpaneは相対順を保ち、残りpane群として反対側にまとめる。
  - pane数が1つだけ、またはlayout actionが使えない状態では移動・equalize・cycle・rotate系の項目を出さない。
  - 同じメニュー内の周辺項目は `Split vertical`、`Split horizontal`、`Rename pane`、`Clear pane name`、`Equalize pane sizes`、`Cycle pane layout`、`Rotate panes`、`Rotate panes reverse`、`Zoom` / `Unzoom`、`Close pane`。
- **受け入れ条件**:
  - workspace右クリックメニューの先頭3項目が `New Claude Code agent`、`New Codex agent`、`New Gemini agent` である。
  - pane右クリックメニューで `New Codex agent` を選ぶと、クリック対象paneを基準に新規splitが作られ、`codex` が起動し、agent listでagent targetとして見える。
  - 3 pane構成で中央paneを `Move to left split` すると、中央paneが左root splitに移動し、focusは中央paneのままである。
  - 3 pane構成で中央paneを `Move to upper split` すると、中央paneが上root splitに移動し、残りpaneは下側に保持される。
  - 1 pane構成では `Move to left split` / `Equalize pane sizes` / `Cycle pane layout` / `Rotate panes` がpane menuに出ない。
- **実装方針**: 本家の `pane.move` / `pane.swap` / `pane.split` / `agent.start` 相当APIを土台に使う。足りないのは「右クリックmenu項目としてagent presetを表示し、クリック対象pane/workspaceからagent.startへつなぐUI」と「root splitの指定sideへpaneを寄せる操作」なので、APIがある場合もUI統合とside指定moveはfork仕様として追加する。
- **デグレ判定**:
  - menuから起動したpaneがagent listに出ない。
  - `New Claude Code agent` / `New Codex agent` / `New Gemini agent` の文言や順序が変わる。
  - pane右クリック時にクリック対象ではなく現在focus中paneを基準にagentが起動する。
  - left/right/upper/lower移動後に対象paneのfocusが失われる。
  - pane移動が単なるfocus移動やswapになり、root splitの指定sideへ寄らない。

### paneタイトル常時表示とgit branch表示
- **元コミット**: 80da299, 047c240
- **分類**: CORE-UI
- **目的**: paneが1枚だけの時やagentでない時でも、ユーザーとAIが対象paneを `%N` で常に識別できるようにする。作業dirがGit repoならbranch名も同じtitleに出し、paneの文脈を画面だけで判別できるようにする。
- **UI挙動**:
  - すべてのpane枠にtitleを常時表示する。single-pane terminalでも表示する。
  - titleの基本形は `%<global_pane_number> <label>`。labelは手動pane名、agent名、検出agent label、起動argv由来labelの順に選び、なければ `terminal`。
  - zoom中のpane titleは先頭に `ZOOM` を付け、例として `ZOOM %5 terminal` になる。
  - terminal OSC titleまたはagent task titleがある場合は、pane labelの後ろに追加する。例: `%81 codex thinking`。
  - agent task titleがある場合はOSC titleより優先する。例: OSC titleが `herdr`、agent task titleが `restore pane sessions` なら `%81 codex restore pane sessions`。
  - pane cwdがGit repoの場合、branch名をtitle末尾に追加する。例: `%81 codex thinking feature`。
  - 設定 `ui.show_agent_labels_on_pane_borders` はdefault `false` のまま残すが、「pane title自体を出す/出さない」のtoggleとしては使わない。
  - Settings popupから旧pane label toggleを削除する。設定画面のtab順は Theme → Sound → Toast → Integrations。
- **受け入れ条件**:
  - single-pane workspaceでもpane枠に `%1 terminal` のようなtitleが出る。
  - 手動pane名 `reviewer` があるpaneは `%N reviewer` と表示される。
  - zoom時はtitleが `ZOOM %N ...` で始まる。
  - Git branch `feature` のrepoをcwdに持つCodex paneでOSC title `thinking` がある場合、titleが `%N codex thinking feature` になる。
  - Settingsにpane title表示toggleが存在しない。
- **実装方針**: 本家のpane render/title領域に乗せる。branch取得はworkspace側のGit status cacheがあればそれを使えるが、pane titleはpane cwd単位で表示する必要がある。`pane.report_metadata` がある本家ではagent task titleの入力経路をそこに寄せられるが、常時title表示とgit branch末尾表示はcore UIとして追加が必要。
- **デグレ判定**:
  - single-pane時にtitleが消える。
  - `%N` が表示されず、AIがpane targetを画面から読めない。
  - branch名がworkspace sidebarにだけ出てpane titleに出ない。
  - agent task titleよりOSC titleが優先される。
  - `show_agent_labels_on_pane_borders = false` にするとpane title全体が消える。

### agent task titleとfooter labelの整理
- **元コミット**: 54e57e0
- **分類**: PARTIAL
- **目的**: shellやOSC titleの機械的な文字列と、AI agentが報告する短い作業名を分けて扱う。pane title・workspace名・footer labelで、何のagentが何をしているかを短く読めるようにする。
- **UI挙動**:
  - `herdr pane report-agent <pane_id> --source ID --agent LABEL --state idle|working|blocked|unknown [--message TEXT] [--custom-status TEXT] [--seq N] [--title TEXT] [--session-id ID]` を受け付ける。
  - `--title TEXT` は前後空白をtrimし、空文字なら未設定として扱う。
  - pane titleでは `--title` 由来のagent task titleをOSC titleより優先して表示する。
  - root paneがagent paneの場合、workspace表示名は `<workspace-cwd名>-<agent task title>` を使う。agent task titleがなければOSC titleを使う。shell paneのOSC titleはworkspace名に混ぜない。
  - footer/action barのpane領域ラベルは通常時 ` PANES `、button labelは ` CYCLE LAYOUT `、` ROTATE PANES `、` EQUALIZE ` の大文字表記にする。
- **受け入れ条件**:
  - `pane report-agent ... --title " restore pane sessions "` 後、対象pane titleに `restore pane sessions` が表示される。
  - 同じpaneにOSC titleがあっても、`--title` が表示に優先される。
  - root agent paneのworkspace名が `repo-restore pane sessions` の形になる。
  - shell root paneのOSC title `user@host:repo` はworkspace名に混入しない。
  - bottom pane action barに ` PANES ` / ` CYCLE LAYOUT ` / ` ROTATE PANES ` / ` EQUALIZE ` が表示される。
- **実装方針**: 本家の `pane.report_metadata` または `pane.report_agent` にtitle fieldを持たせ、semantic stateとは別の表示用短縮titleとして扱う。agent状態・通知・resume用session idとは結合しすぎず、titleは表示用metadataとしてpane/workspace title生成に渡す。
- **デグレ判定**:
  - `--title` がCLI/APIで受け付けられない。
  - `--title` がagent stateやcustom statusとして表示され、pane title/workspace名に出ない。
  - shell OSC titleがworkspace名に混ざる。
  - footer labelが旧表記 ` panes ` / ` Cycle layout ` などに戻る。

### fail-closedなcurrent pane解決
- **元コミット**: 32d7de2, b8aa2b7
- **分類**: PARTIAL
- **目的**: AIやskillが「自分が入っているpane」を間違えて別paneへ送信する事故を防ぐ。現在focus中pane・active tab・pane list順から推測せず、呼び出し元processに対応するpaneだけを返す。
- **UI挙動**:
  - CLI `herdr pane current` を追加する。
  - 引数付きで呼ばれた場合はstderrに `usage: herdr pane current` を出してexit code 2。
  - `HERDR_PANE_ID` が空でない場合は、その値をpane targetとしてserverに検証させ、存在すれば標準出力に正規化済みpane idを1行で出す。
  - `HERDR_PANE_ID` がない場合は、CLI自身のprocess idをserverへ送り、serverが同じprocess sessionに属するpaneを解決する。
  - 解決できない場合はexit code 1で失敗する。focused pane、active tab、pane list、UI selectionへfallbackしてはならない。
  - 古いserverが `pane.current` を知らない場合は `running herdr server does not support \`pane.current\`; restart the server so the installed herdr binary takes effect` をstderrに出して失敗する。
  - docs/helpでは `pane current uses HERDR_PANE_ID first, then resolves the calling process session` と説明する。
- **受け入れ条件**:
  - Herdr pane内で `HERDR_PANE_ID=1-2 herdr pane current` を実行すると、server検証後にそのpane idだけがstdoutへ出る。
  - `HERDR_PANE_ID` なしでHerdr pane内のshellから実行すると、そのshell process sessionに対応するpane idだけがstdoutへ出る。
  - Herdr外や対応pane不明のprocessから実行すると失敗し、focused pane idを返さない。
  - `herdr pane current extra` はexit code 2でusageを出す。
  - 古いserver相手ではrestartを促す専用messageになる。
- **実装方針**: 本家の `pane.current` がある場合はそれを使う。ただし本家にfocused fallbackが残るなら、fork仕様ではfallbackを削除してfail-closedにする。server側にはprocess idからpane runtime/process sessionへ逆引きするAPIが必要で、CLIは `HERDR_PANE_ID` 検証を優先する。
- **デグレ判定**:
  - `HERDR_PANE_ID` なしの時にfocused paneを返す。
  - pane listの先頭やactive tabから推測する。
  - staleな `HERDR_PANE_ID` をserver検証なしでそのまま成功扱いする。
  - 未対応serverでJSON errorをそのまま出し、restart案内が出ない。

### 短いpane targetとAI向けhelp
- **元コミット**: a5f2d9d, 130f5d7, 87d2c49
- **分類**: PARTIAL
- **目的**: AIがpane targetを短く確実に読み書きできるようにする。CLI helpも人間向け一覧ではなく、agentが守るべきpane識別ルールを含む再利用可能な指示として読める形にする。
- **UI挙動**:
  - pane targetとして `1-2` のようなworkspace-local short id、global pane number `23`、tmux風global pane number `%23`、stable id `w...-2` / `p_...` を受け付ける。
  - `herdr pane list`、pane作成系response、agent infoには短いpane idとglobal idを含める。pane infoは `pane_id`、`short_id`、`global_id`、`global_number`、`workspace_number`、`pane_number` を含む。
  - agent infoは `pane_id` に加え `short_pane_id`、`global_pane_id`、`global_pane_number` を含む。
  - workspace作成responseは `workspace`、`tab`、`root_pane` を返し、root paneの `short_id` は最初のworkspaceなら `1-1`、`global_id` は `p_<global_number>`。
  - root helpは `herdr help` と `herdr --help` で同一のstdoutを返す。
  - root help先頭はYAML front matter風に `---`、`name: herdr`、`description: Terminal workspace manager for AI coding agents...` を含む。
  - root helpには `## When To Use`、`## Agent Rules`、`## Usage`、`## Commands`、`## Essential Agent Recipes`、`## Options`、`## More Help` を含む。
  - `## Agent Rules` には `Do not infer the requester pane from the focused pane, active window, pane list order, or UI selection.` を含める。
  - root helpに内部コマンド `herdr client` を広告しない。
  - protocol bump後、`herdr status` / `herdr status server` / `herdr status client` はprotocolを表示し、client/serverのprotocol不一致時はcompatibleを `no` と判定する。
- **受け入れ条件**:
  - 2 pane構成で `herdr pane get 1-2`、`herdr pane get 23`、`herdr pane get %23` が同じpaneを指せる。
  - `herdr pane list` の各paneにshort idとglobal idが出る。
  - `herdr agent list` の各agentに `short_pane_id` と `global_pane_id` が出る。
  - `herdr help` と `herdr --help` のstdoutが完全一致する。
  - root helpに `HERDR_PANE_ID`、`calling process session`、`Do not infer the requester pane from the focused pane`、`herdr pane current` が含まれる。
  - root helpに `herdr client` が含まれない。
- **実装方針**: 本家の新APIでpane id体系がある場合はそれに乗る。足りないのは `%23` 形式やshort/global idを全responseへ明示する互換層、そしてAI-readable root helpの本文である。protocol versionはwire/API response shapeが変わる時だけ明示的に上げ、test fixtureも固定値で更新する。
- **デグレ判定**:
  - AIが画面で見た `%23` をCLI targetとして使えない。
  - `pane.list` だけにshort idがあり、`agent.list` や作成responseに出ない。
  - root helpが一般的な短いusageに戻り、fail-closed pane識別ルールが消える。
  - `herdr help` がunknown command扱いになる。
  - protocol不一致がstatusで分からない。

### agent sendとheadless入力の確実な送信
- **元コミット**: f87cc86, 60036e2
- **分類**: PARTIAL
- **目的**: AIへのmessage送信やheadless client入力がprompt欄に残ったり、長いIME/音声入力が途中で欠けたりする事故をなくす。agent向け送信は「文字列を書くだけ」ではなく、submitまで完了した操作として扱う。
- **UI挙動**:
  - `herdr agent send <target> <text>` は対象agent terminalへtextを書き込み、500ms待ってからEnterを送る。
  - `agent send` 成功時はAPI response `ok`。textだけが入力欄に残る状態を成功扱いしない。
  - helpには `agent send writes text and submits it with Enter` と表示する。
  - `herdr pane send-text <pane_id> <text>` はliteral textだけを書き、Enterは押さない。
  - `herdr pane run <pane_id> <command>` はcommand textとEnterを同一API requestで送る。help/docsでは `pane send-text writes literal text without Enter; pane run submits command text with Enter` と説明する。
  - `pane.send_input` APIでtextとkeysの両方を送る場合、text送信後に500ms待ってからkeysを送る。
  - headless server/client modeではclientから来たraw input bytesをparseし、focused paneのnegotiated keyboard protocolで再エンコードして送る。host terminalのescape sequenceをそのまま流して、pane側protocolとずれる状態にしない。
  - raw LineFeedはterminal modeでそのままpaneへforwardする。
  - 多言語IMEや長い音声入力のようなCJK/emoji混在textを文字単位で欠落なくforwardする。
- **受け入れ条件**:
  - `agent send` の送信先runtimeには `hello agent` のbytesの後に `\r` が届く。
  - `pane send-text` はEnterを送らず、`pane run` はEnterを送る。
  - `pane.send_input` でtextと`Enter`を指定した時、textとEnterが別chunkで順に届く。
  - headless client経由の日本語・中国語・韓国語・emoji混在長文が欠落せずpaneへ届く。
  - Shift/modified Enterなどはpaneのkeyboard protocolに従ってencodingされる。
- **実装方針**: 本家の `pane.send_input` / `agent.send` / headless client input pipelineを使う。足りない場合は、agent sendをtext-onlyではなくtext+delayed Enterにし、headless inputはraw bytes直通ではなく既存のterminal input処理へ通す。
- **デグレ判定**:
  - `agent send` 後にmessageがAI CLIの入力欄へ残り、submitされない。
  - `pane send-text` がEnterまで押してしまう。
  - `pane run` がtext送信とEnter送信を別requestに分け、間に他入力が割り込める。
  - headless clientでCJK/emoji長文が途中で切れる、または文字化けする。
  - headless inputがfocused paneのkeyboard protocolを無視する。

### leaked HERDR_ENVを無視するnested guard
- **元コミット**: ab4a7c8
- **分類**: PARTIAL
- **目的**: Herdr内で起動したshellから環境変数だけが外へ漏れた時に、通常のHerdr CLIやclient起動を誤ってnested launchとして拒否しない。一方、本当にHerdr processの子孫として再帰起動している場合は従来通り止める。
- **UI挙動**:
  - `experimental.allow_nested` のdefaultは `false`。
  - nested guardは `HERDR_ENV=1` だけでは発火しない。現在processのancestorに `herdr` がいる場合だけ、nested Herdrとしてblockする。
  - block時のstderrは `error: nested herdr is disabled by default.`、`detected HERDR_ENV=1 with a herdr parent process.`、`set [experimental] allow_nested = true if you want to enable it.` を含む。
  - `HERDR_ENV=1` とstale `HERDR_PANE_ID` があっても、Herdr ancestorがなければ通常の接続処理へ進む。socketがない場合は通常通り `failed to connect to server` で失敗し、`nested herdr` とは表示しない。
  - unknown flagやremoved flagはnested guardより前にCLI argument errorとして処理する。
- **受け入れ条件**:
  - Herdr processの子孫で `HERDR_ENV=1` の状態から `herdr` を起動するとnested guardで止まる。
  - Herdr ancestorがないprocessで `HERDR_ENV=1 HERDR_PANE_ID=p_stale herdr client` を実行するとnested guardでは止まらず、socket未接続なら `failed to connect to server` になる。
  - `--show-changelog` のような未知/削除済みflagはnested guard messageを出さず、unknown optionとして失敗する。
  - `[experimental] allow_nested = true` の場合はHerdr ancestorがいてもnested guardで止まらない。
- **実装方針**: 本家のnested launch guardが環境変数だけを見る場合、platform層にprocess ancestor判定を追加して、`HERDR_ENV=1 && has_herdr_ancestor && !allow_nested` の時だけblockする。macOS/Linuxでprocess tree取得が必要だが、取得不能時に環境変数だけで過剰blockしない。
- **デグレ判定**:
  - stale `HERDR_ENV=1` だけで `herdr client` やCLI helperがnested guardに止められる。
  - Herdr内で本当に再帰起動した時にguardされない。
  - argument errorよりnested guardが先に出る。
  - stderrから `detected HERDR_ENV=1 with a herdr parent process.` が消え、何を検出したか分からない。

## G6. 入力・コピー・vim mode

### Vim風ターミナル操作モード
- **元コミット**: da3e541
- **分類**: CORE-UI
- **目的**: キーボード中心で複数 workspace / pane を移動する利用者が、tmux prefix やマウスに戻らず Vim の Normal/Insert に近い感覚で Herdr 内の焦点を動かせるようにする。pane 内アプリへの入力と Herdr 自体のナビゲーションを明示的に分ける。
- **UI挙動**:
  - 設定キーは `[ui] vim_mode`。default は `false`。
  - `vim_mode = true` で起動した terminal mode は Vim Normal mode から始まる。
  - global menu に `vim mode off` または `vim mode on` という項目を表示する。選択すると有効/無効をトグルし、menu を閉じて terminal mode に戻る。トグル直後は Insert mode ではなく Normal mode になる。
  - Vim mode 有効時、下部 pane action bar の左ラベルは通常の ` panes ` ではなく、Normal mode では ` vim normal `、Insert mode では ` vim insert ` を表示する。
  - Normal mode で修飾なし `h` は左 pane、修飾なし `l` は右 pane へ focus を移動する。
  - Normal mode で修飾なし `j` は次の workspace、修飾なし `k` は前の workspace へ移動する。
  - Normal mode で修飾なし `i` または修飾なし `Enter` は Insert mode に入る。このキー自体は pane へ送らない。
  - Normal mode で `Ctrl+[` は pane focus history を戻り、`Ctrl+]` は進む。履歴は pane focus が変わった時だけ記録し、存在しなくなった pane は飛ばす。workspace / tab / pane をまたいだ履歴を扱う。
  - Normal mode で Herdr の prefix key を押すと通常の Prefix mode に入る。
  - Normal mode の上記以外の key press / repeat は pane に送らず消費する。key release も pane に送らない。
  - Insert mode では通常の key input を pane にそのまま送る。Herdr の direct navigation keybind や custom command keybind も横取りしない。
  - Insert mode で `Ctrl+[` または `Ctrl+]` を押すと Normal mode に戻る。このキー自体は pane へ送らない。
  - Insert mode の `Esc` は pane へ送られ、Insert mode のまま残る。
- **受け入れ条件**:
  - `[ui] vim_mode = false` または未指定の状態では、既存の terminal input forwarding と Herdr keybind 挙動が変わらない。
  - `[ui] vim_mode = true` で 2 pane を左右分割し、左 pane focus から `l` を押すと右 pane に focus が移る。続けて `Ctrl+[` で左 pane、`Ctrl+]` で右 pane に戻る。
  - 2 workspace ある状態で Normal mode の `j` が次 workspace、`k` が前 workspace を選ぶ。
  - Normal mode の `i` または `Enter` で action bar が ` vim insert ` になり、`Ctrl+c` は byte `0x03` として pane に届く。
  - Insert mode の `Esc` は byte `0x1b` として pane に届き、action bar は ` vim insert ` のまま。
  - Insert mode の `Ctrl+[` と `Ctrl+]` は pane に届かず、action bar が ` vim normal ` に戻る。
  - Insert mode で Herdr の direct binding に割り当てた `alt+h` などを押しても Herdr 側の pane focus は動かず、pane へその key sequence が届く。
  - global menu の項目 label は有効前が `vim mode off`、有効後が `vim mode on`。
- **実装方針**: 本家に同等の Vim Normal/Insert terminal control はないため core input / state / render に載せる。既存の workspace switching、pane directional focus、prefix mode、copy mode とは競合させず、terminal mode の input routing 層で Normal/Insert を判定する。pane focus history は本家の pane focus 操作に追随して記録する。
- **デグレ判定**:
  - `vim_mode = true` で `h/j/k/l` が pane や shell に入力される。
  - Insert mode で Herdr の direct keybind が横取りされ、pane 内 Vim / shell / agent CLI の入力が壊れる。
  - `Ctrl+[` / `Ctrl+]` が Normal mode で履歴移動せず、Insert mode で mode exit しない。
  - action bar の ` vim normal ` / ` vim insert ` が表示されず、現在の mode が見分けられない。
  - global menu の `vim mode off` / `vim mode on` が表示されない、またはトグル後に Insert mode が残る。

### 右角括弧でもcopy modeに入る
- **元コミット**: c3d0853
- **分類**: CORE-UI
- **目的**: copy mode 本体は本家実装を採用しつつ、左右どちらの角括弧でも copy mode に入れる fork の操作感を維持する。`prefix+[` が打ちにくい端末・配列でも `prefix+]` を同じ入口にする。
- **UI挙動**:
  - default keybind は `[keys] copy_mode = ["prefix+[", "prefix+]"]`。
  - `prefix+[` と `prefix+]` はどちらも focused pane の copy mode に入る。
  - copy mode 内の移動・選択・yank・終了挙動は本家 copy mode の仕様をそのまま使う。この entry は copy mode 本体を再実装対象にしない。
- **受け入れ条件**:
  - default config で `prefix+]` を押すと mode が Copy になる。
  - default config で `prefix+[` も引き続き mode が Copy になる。
  - keybind help / menu / docs に表示される copy mode binding が複数 binding として扱われ、片方だけに戻らない。
  - ユーザーが `[keys] copy_mode` を明示設定した場合は、本家の keybind override ルールに従う。
- **実装方針**: 本家の copy mode 実装と keybind parser を使い、default `copy_mode` binding だけを `prefix+[` / `prefix+]` の複数値にする。本家の copy mode 入力処理には fork 固有の分岐を足さない。
- **デグレ判定**:
  - default config で `prefix+]` が copy mode に入らない。
  - `prefix+]` 対応のために本家 copy mode の選択・yank・scroll 挙動が変わる。
  - `prefix+[` だけが help / docs に出て、`prefix+]` が discoverable でない。

### tmux風split default keybind
- **元コミット**: bbf6c05
- **分類**: PARTIAL
- **目的**: tmux 利用者が `prefix+%` と `prefix+"` の身体記憶で pane split できるようにする。既存 fork の `prefix+v` / `prefix+minus` も残して、移行コストを増やさない。
- **UI挙動**:
  - default keybind は `[keys] split_vertical = ["prefix+percent", "prefix+v"]`。
  - default keybind は `[keys] split_horizontal = ["prefix+double_quote", "prefix+minus"]`。
  - `prefix+percent` は実キー `prefix+%` として扱い、side-by-side split を作る。
  - `prefix+double_quote` は実キー `prefix+"` として扱い、stacked split を作る。
  - `prefix+v` と `prefix+minus` は従来どおり使える。
  - 生成される default config のコメントも `# split_vertical = ["prefix+percent", "prefix+v"]`、`# split_horizontal = ["prefix+double_quote", "prefix+minus"]` にする。
- **受け入れ条件**:
  - default keybind で `prefix+%` と `prefix+v` が同じ vertical split action に一致する。
  - default keybind で `prefix+"` と `prefix+minus` が同じ horizontal split action に一致する。
  - key parser が `percent` を `%`、`double_quote` を `"` として扱う。
  - `prefix+%` / `prefix+"` 追加後も既存の user config override が二重発火しない。
- **実装方針**: 本家は `percent` / `double_quote` の parser と複数 binding 構造を持つため、移植対象は default keybind と default config 表示の変更だけ。本家の split API / layout mutation はそのまま使う。
- **デグレ判定**:
  - default config で `prefix+%` または `prefix+"` が split しない。
  - `prefix+v` または `prefix+minus` が default から消える。
  - keybind help や default config が単一 binding 表示に戻り、tmux 風 key が discoverable でなくなる。

### Copilot CLI向けraw LFとGhostty入力互換
- **元コミット**: 2ae1078, 480a8f6
- **分類**: PARTIAL
- **目的**: GitHub Copilot CLI など、`Shift+Enter` / raw line feed / FixTerm keyboard query に依存する pane 内 TUI が Herdr 内で改行入力や状態表示を壊さないようにする。特に Ghostty 経由で Copilot CLI を動かす時に、複数行入力と working 状態検出を安定させる。
- **UI挙動**:
  - raw input byte `\n` は terminal mode では focused pane に byte `\n` としてそのまま送る。`Ctrl+j` / Enter に正規化しない。
  - terminal mode 以外で raw input byte `\n` を受けた場合は、従来どおり `Ctrl+j` 相当の Herdr key input として扱う。
  - Legacy keyboard protocol で `Shift+Enter` press / repeat を受けた場合、pane へ byte `\n` を送る。release event は何も送らない。
  - host terminal の `TERM_PROGRAM` が `ghostty` / `Ghostty` の場合、modifyOtherKeys mode 1 を有効化対象にする。
  - pane 内アプリが FixTerm keyboard query `ESC [ ? u` を出した場合、Herdr は `ESC [ ? 0 u` を返す。
  - FixTerm keyboard query を見た Ghostty pane では、明示的な kitty keyboard protocol が有効でない限り、Legacy `Shift+Enter` を byte `\n` として送る。
  - pane 内アプリが明示的 keyboard protocol request `ESC [ >` を出したら、FixTerm query による Legacy `Shift+Enter` 特例を解除する。
  - GitHub Copilot CLI の screen detection は、`esc to cancel` だけでなく `esc cancel` も Working とみなす。
  - Copilot の status footer は、trim 後の先頭が `●` / `◉` / `◎` / `○` のいずれかで、かつ `thinking` と `esc cancel` を含む行、または `loading:` を含む行を Working とみなす。例: `● Thinking esc cancel`、`◉ Loading: 1 instruction, 5 hooks, 62 skills`。
  - workspace / sidebar / mobile summary の Working 状態は固定 dot ではなく spinner frame を表示する。
- **受け入れ条件**:
  - terminal mode の focused pane に raw byte `\n` を route すると、runtime が受け取る bytes は `b"\n"` だけになる。
  - non-terminal mode の raw byte `\n` は Herdr の `Ctrl+j` key path に入る。
  - Legacy protocol の `Shift+Enter` press は `b"\n"`、release は空 bytes になる。
  - Ghostty host では modifyOtherKeys mode 1 が選ばれ、未知 host では勝手に有効化されない。
  - pane output に `ESC[?u` が含まれると response `ESC[?0u` が返る。
  - `ESC[?u` 後、kitty flags が 0 の Legacy `Shift+Enter` は `b"\n"` になる。`ESC[>` 後はこの特例に依存しない。
  - `● Thinking esc cancel` と `◉ Loading: 1 instruction, 5 hooks, 62 skills` は Copilot Working と検出される。
  - Working workspace / agent summary が spinner 表示になる。
- **実装方針**: 本家は raw LF preservation、modified Enter preservation、Copilot integration / manifest、host terminal keyboard protocol 周辺を既に大きく持つため PARTIAL。最新 upstream の input parse / encode / pane terminal protocol / agent detection の既存経路に、fork 固有として不足している Ghostty FixTerm query 互換、Copilot footer wording、summary spinner 表示だけを足す。
- **デグレ判定**:
  - Copilot CLI で `Shift+Enter` や raw LF が Enter / `Ctrl+j` として扱われ、複数行入力が送信やキャンセルに化ける。
  - Ghostty 上で Copilot CLI が keyboard query 後も `Shift+Enter` を LF として受け取れない。
  - `esc cancel` 表記や `Loading:` footer 中の Copilot が Idle / Unknown 扱いになる。
  - Working 状態の workspace / agent summary が spinner ではなく静的 dot に戻る。

### pane内NO_COLORを引き継がない
- **元コミット**: f4ac406
- **分類**: PARTIAL
- **目的**: Herdr を起動した外側環境に `NO_COLOR` があっても、Herdr pane は対話的な color-capable TTY として Claude Code、Codex、その他の nested TUI status helper に色を出させる。外側の非対話用途向け `NO_COLOR` が pane 内 UI を単色化する事故を防ぐ。
- **UI挙動**:
  - Herdr が pane child process を起動する時、child environment から `NO_COLOR` を削除する。
  - pane child process には従来どおり `TERM = "xterm-256color"` と `COLORTERM = "truecolor"` を設定する。
  - ユーザー向けの追加 menu、toast、label はない。observable behavior は pane 内コマンドから見える environment と、nested TUI が色付き表示になること。
  - f4ac406 に含まれる「child process exit 後に OSC 10/11 default color を host theme に戻す」挙動は最新本家側に吸収済みとして扱い、この entry の fork 差分は `NO_COLOR` 削除に限定する。
- **受け入れ条件**:
  - 外側環境に `NO_COLOR=1` がある状態で Herdr pane を起動し、pane 内で環境変数を確認すると `NO_COLOR` が未設定である。
  - 同じ pane 内で `TERM` は `xterm-256color`、`COLORTERM` は `truecolor` として見える。
  - Claude Code / Codex などの color-capable nested TUI が、外側の `NO_COLOR` だけを理由に monochrome 表示にならない。
- **実装方針**: 本家は pane default color / host terminal color restoration 周辺を持つため PARTIAL。最新 upstream の pane command environment setup に `NO_COLOR` removal だけを追加する。OSC color owner restoration は本家側挙動を優先し、fork 独自に重複実装しない。
- **デグレ判定**:
  - 外側 `NO_COLOR=1` が pane child process に残る。
  - `NO_COLOR` 削除のために `TERM` / `COLORTERM` の pane identity が壊れる。
  - nested Claude Code / Codex / Copilot CLI の status helper が Herdr pane 内で単色表示に戻る。

## G7. worktree操作

fork独自実装（92edda7, e7338e4）は本家 0148c13 の git worktree workspace management（CLI/socket API付き）に完全吸収済み。**本家実装をそのまま採用**し、fork差分は持たない。

## G8. fork運用ポリシー

### hook 統合を持たない agent 状態運用
- **元コミット**: de0d64a
- **分類**: PARTIAL
- **目的**: fork は agent 側の hook/plugin 設定ディレクトリを書き換えず、Herdr 本体の process detection と screen heuristic を基本にする。状態を明示したい場合も、agent config への組み込みではなく信頼済みローカルツールから Herdr socket API へ報告する。
- **UI挙動**:
  - README と docs の導線から `integrations` ページへのリンクを出さない。
  - Agents docs の検出説明は「foreground process detection」「terminal output heuristics」の 2 つだけを built-in signal として説明し、「hooks」「plugins」「integrations provide precise semantic state」のような表現を出さない。
  - first-run onboarding の `continue` ボタン、Enter、右矢印、`l` は `onboarding = false` を保存したうえで通常画面へ戻る。settings の `integrations` tab は開かない。
  - settings modal の tab は `theme`、`sound`、`toasts` の 3 つだけ。`integrations` tab、integration badge、recommended integration list、`install` action、`installed <label>` / `<label>: <err>` の結果表示を出さない。
  - root help と CLI reference に `herdr integration install pi|claude|codex|opencode|hermes`、`herdr integration uninstall ...`、`herdr integration status [--outdated-only]` を出さない。
  - socket API method 一覧に `integration.install` / `integration.uninstall` を出さず、API request と response schema も受け付けない。
  - 状態報告の残存導線は `herdr pane report-agent <pane_id> --source ID --agent LABEL --state idle|working|blocked|unknown [--message TEXT] [--custom-status TEXT] [--seq N]` と raw method `pane.report_agent`。docs 上の主体は `custom hooks` / `integrations` ではなく `custom tools` / `trusted local tools` と表現する。
- **受け入れ条件**:
  - `herdr --help` に `integration` subcommand と `herdr update` が表示されない。
  - settings を開いても `integrations` tab が存在せず、Tab / Shift-Tab / `h` / `l` の巡回対象が `theme`、`sound`、`toasts` のみになる。
  - onboarding の continue 後に `Mode::Navigate` 相当の通常画面へ戻り、settings modal が開かない。
  - socket API で `integration.install` / `integration.uninstall` を送っても正式 method として成功しない。
  - `herdr pane report-agent ...` は引き続き成功し、pane の agent label/state/custom status を更新できる。
- **実装方針**: 本家に `pane.report_metadata`、`pane.report_agent`、`notification.show` などの API がある場合は、状態報告はそれらに乗せる。足りないのは「hook/plugin installer をあえて持たない」という product policy なので、本家が integrations を拡充している場合は fork で削除するか、少なくとも fork build では UI/CLI/docs から隠す要検討事項にする。
- **デグレ判定**: fork build が agent の `~/.claude`、`~/.codex`、OpenCode、Pi、Hermes などの config/plugin/hook directory を自動変更する。`integrations` tab や `herdr integration ...` が復活する。docs が「hooks/plugins を入れるとより正確」と案内する。`pane.report-agent` まで消えて、信頼済みローカルツールから状態報告できなくなる。

### fork からの build/install を正とする配布
- **元コミット**: b2ea0fe, 73e429c
- **分類**: POLICY
- **目的**: kazuph/herdr fork は upstream の hosted installer / release manifest を正にせず、fork repository の source build と fork releases を配布導線にする。
- **UI挙動**:
  - README の install 手順は次の 4 行を primary path として表示する。
    ```bash
    git clone https://github.com/kazuph/herdr
    cd herdr
    just build
    install -m 0755 target/release/herdr ~/.local/bin/herdr
    ```
  - README の binary download link は `https://github.com/kazuph/herdr/releases`。
  - README の building from source は `git clone https://github.com/kazuph/herdr` を使う。
  - docs install page も `curl -fsSL https://herdr.dev/install.sh | sh` ではなく同じ fork build/install 手順を出す。
  - docs install page の description は「Build, install, and verify Herdr on Linux and macOS.」で、hosted installer / self-update を使わないことを明記する。
- **受け入れ条件**:
  - public docs / next docs / README に upstream install script を primary install path として出さない。
  - fork から rebuild する AI が `just build` 後に `target/release/herdr` を `~/.local/bin/herdr` へ入れる手順を迷わない。
  - 手動 download link が `ogulcancelik/herdr/releases` ではなく `kazuph/herdr/releases` を指す。
- **実装方針**: 本家 docs の install section を fork docs overlay として差し替えるだけでよい。実行時 API は不要。
- **デグレ判定**: README/docs が `herdr.dev/install.sh`、`ogulcancelik/herdr/releases`、または upstream clone を fork の標準導線として案内する。

### self-update と release download の無効化
- **元コミット**: 73e429c
- **分類**: CORE-UI
- **目的**: fork の実行中 binary を network update や upstream/fork manifest download で勝手に置き換えない。更新は source build と明示的な local install に限定する。
- **UI挙動**:
  - root help の Usage と Common commands に `herdr update` を表示しない。
  - `auto_updates_enabled` は session mode / debug build に関係なく false。起動時に update check thread を開始せず、30 分ごとの background check も予約しない。
  - 起動時に saved preview release notes や pending release notes があっても `update_available = None`、`latest_release_notes_available = false` として扱い、update ready badge / release notes availability を出さない。
  - `update_install_command()` は `build from source and install target/release/herdr` を返す。もし内部 event `UpdateReady { version }` を受けた場合の toast context は `detach, then run `build from source and install target/release/herdr``。
  - `self_update()` は成功せず、エラー文字列は `self-update is disabled in the kazuph/herdr fork; build from source and install target/release/herdr explicitly` を含む。
  - remote attach で local platform と remote platform が一致する場合は current local binary をコピーできる。platform が違い、`HERDR_REMOTE_BINARY` が未指定の場合、release manifest から download せず、`automatic release downloads are disabled in the kazuph/herdr fork. Build herdr for the remote platform and pass HERDR_REMOTE_BINARY.` を含むエラーで失敗する。
- **受け入れ条件**:
  - `herdr --help` に `herdr update` がない。
  - app 起動後に network update check が走らず、`latest.json` / Homebrew formula / GitHub release へアクセスしない。
  - stale release notes store があっても global menu に update attention badge が出ない。
  - cross-platform remote attach は `HERDR_REMOTE_BINARY` なしで fail closed し、manifest fallback download をしない。
- **実装方針**: 本家 update subsystem が存在する場合でも fork build では network update path を no-op / explicit error にする。remote は本家の remote attach structure を使い、install source resolution の download fallback だけを無効化する。
- **デグレ判定**: `herdr update` が実行可能な成功 path として復活する。起動中に update toast / badge が出る。remote attach が `latest.json` から binary を取得する。更新案内が `herdr update` や package-manager update を fork の正手順として示す。

### macOS 開発 binary の ad-hoc 署名更新
- **元コミット**: 6f62ce7
- **分類**: CORE-UI
- **目的**: macOS でコピー・置換された開発 build が無効な Mach-O 署名のまま SIGKILL されるのを防ぎ、fork の local runtime 置換を安定させる。
- **UI挙動**:
  - `just install-local` を提供する。recipe は `just build` 後、`target/release/herdr` を macOS では `codesign -s - -f` で ad-hoc sign し、`${HERDR_INSTALL_DIR:-$HOME/.local/bin}/herdr` へ `install -m 755` で配置し、配置後 binary も macOS では再度 `codesign -s - -f` する。最後に `installed <dest>` を stdout に出す。
  - app 起動直後、現在の executable の署名が無効なら `codesign -s - -f <exe>` 相当で更新する。失敗時は stderr に `warning: failed to refresh code signature for <path>: <err>` を出して起動処理は続ける。
  - session relaunch 前は強制的に current executable を ad-hoc sign する。失敗時 error には `failed to refresh macOS code signature for <path>: <err>. run `codesign -s - -f <path>` and try again` を含める。
  - Linux と unsupported platform では署名更新は no-op。
- **受け入れ条件**:
  - macOS で `just install-local` が成功すると `~/.local/bin/herdr` または `HERDR_INSTALL_DIR/herdr` が存在し、`codesign --verify --strict <dest>` が成功する。
  - macOS で署名が壊れた local binary を起動しても、可能なら起動時に署名が更新される。
  - Linux build では `codesign` を呼ばず、同じ API が成功 no-op になる。
- **実装方針**: 本家 platform abstraction に macOS-only の signature refresh を追加し、build/install workflow は `just` recipe に集約する。update subsystem を無効化していても、local install と relaunch のための署名 refresh は残す。
- **デグレ判定**: macOS の local build/install 後に Gatekeeper / kernel による即時終了が再発する。`just install-local` が署名しない。署名失敗時の手動 recovery command が表示されない。

### agent 変更後の local runtime 置換必須
- **元コミット**: f99d42f
- **分類**: POLICY
- **目的**: fork workspace では source code の変更だけで完了扱いにせず、実際にユーザーが使う `herdr` runtime へ反映することを agent 運用ルールにする。
- **UI挙動**:
  - AGENTS.md の Testing section に、agent-driven change を commit または handoff するたびに release binary を rebuild し、local runtime `~/.local/bin/herdr` を置き換えてから completion report する、と明記する。
  - このルールは code change がこの環境に fully applied とみなされる条件であり、`just check` だけでは完了ではない。
- **受け入れ条件**:
  - AI が fork change を完了報告する前に `just check` 相当の検証と local binary replacement を行う運用になっている。
  - runtime replacement の対象 path は `~/.local/bin/herdr`。macOS では上記 `just install-local` を使うと署名更新も満たせる。
- **実装方針**: 本家 feature ではなく fork AGENTS/SPEC の運用要件として保持する。自動化するなら `just install-local` を標準 command として使う。
- **デグレ判定**: agent が source diff / test pass だけで完了報告し、実際の `~/.local/bin/herdr` が古いまま残る。

### テスト fixture の個人情報匿名化
- **元コミット**: fd1e1ac
- **分類**: POLICY
- **目的**: fork 固有のユーザー名、メールアドレス、ローカルパス、実モデル名を test fixture に残さず、upstream へ載せ替える AI が private details に依存しないようにする。
- **UI挙動**:
  - ユーザー可視機能の挙動は変えない。変更対象は test fixture 文字列だけ。
  - fixture の個人パスは `/tmp/herdr-fixtures/...` や `/home/user/...` に置き換える。
  - fixture のメールアドレスは `user@example.com` に置き換える。
  - fixture の shell title user は `user@host:<name>` に置き換える。
  - fixture の local agent/model label は必要なら `sample-agent` のような一般名に置き換える。
- **受け入れ条件**:
  - `rg -n "kazuph|kazu\\.homma|/Users/kazuph|qwen-3\\.5-chat" src --glob '*.{rs,md}'` が、仕様上必要な fork 名や docs 以外の test fixture 個人情報を返さない。
  - 匿名化後も detection / notification excerpt / workspace title tests が同じ期待状態を検証する。
- **実装方針**: 本家の test fixtures に移植する際は behavior assertion を維持し、private literal だけを synthetic value に変える。production parser の分岐や UI 表示仕様は変えない。
- **デグレ判定**: fixture に個人メール、home directory、実 private project path、個人利用モデル名が再混入する。匿名化により test が実挙動ではなく過度に抽象化された空文字や placeholder だけを見るようになる。

### pane current の fail-closed 解決説明
- **元コミット**: 659637e
- **分類**: PARTIAL
- **目的**: `herdr pane current` は「今 UI で focus されている pane」ではなく「呼び出し元プロセスが属する pane」を返す command であることを docs 上も明確にし、automation が focused pane / active tab / pane list に fallback しないようにする。
- **UI挙動**:
  - changelog には `Added `herdr pane current` to safely print the calling pane from `HERDR_PANE_ID` or the calling process session without falling back to UI focus.` と記載する。
  - CLI reference の pane section には、`pane current` が running server で検証した calling pane を出力し、優先順は `HERDR_PANE_ID`、次に calling process session 解決であると記載する。
  - 同じ説明で、focused pane、active tab、pane list へ fallback しないことを明記する。
- **受け入れ条件**:
  - docs を読んだ AI が `herdr pane current` を focus lookup command と誤解しない。
  - `HERDR_PANE_ID` がある場合はその値を server validation した pane が返る。
  - `HERDR_PANE_ID` がない場合は server が呼び出し元 process session から pane を解決できる時だけ返る。
  - どちらも解決できない場合は focused pane / active tab / first pane を返さず失敗する。
- **実装方針**: 本家には `pane current` や current pane lookup が部分的に存在するが、focused fallback が残る場合は fork spec と衝突する。API は本家の pane lookup / process session resolution に乗せ、fallback policy を fail-closed にする。
- **デグレ判定**: `HERDR_PANE_ID` 不在時に focused pane、active tab、pane list の先頭、最近使った pane を返す。docs が `HERDR_PANE_ID` だけに限定して process session 解決を説明しない、または focus fallback 可能に読める。
