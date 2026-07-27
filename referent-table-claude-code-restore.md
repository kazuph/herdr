| 出典 | 目的 | 具体対象 | 役割 | 前後関係 | 初出定義 | 候補語 |
|---|---|---|---|---|---|---|
| ユーザー依頼 | Claude Code の復元失敗を説明する | 復元操作後に以前の Claude Code 会話へ戻れる pane | 状態 | 「復元できるやつ」 | 復元成功とは、保存された会話を Claude Code が同じセッションとして再開できる状態を指す | 復元成功 |
| ユーザー依頼 | Claude Code の復元失敗を説明する | 復元操作後に以前の Claude Code 会話へ戻れない pane | 状態 | 「復元できないやつ」 | 復元失敗とは、Herdr が再開を試みても Claude Code が保存された会話を再開できない状態を指す | 復元失敗 |
| Herdr 実装と保存データ | 成功例と失敗例を分ける | Herdr が pane に保存し、再起動時に Claude Code の再開引数へ渡す Claude session id | 値 | pane 保存の後、Claude Code 起動の前 | 復元用セッション ID とは、Herdr が Claude Code の保存済み会話を指定するために保持する識別子を指す | 復元用セッション ID |
| Claude Code transcript | 成功例と失敗例を分ける | 復元用セッション ID に対応する transcript ファイルと、その project path | 記録 | Claude Code が会話を保存した後、次の復元操作より前 | transcript 記録とは、Claude Code が project ごとに保存した会話履歴ファイルを指す | transcript 記録 |
| 実行ログ | 失敗地点を確定する | Herdr の復元起動から Claude Code が再開可否を返すまでの処理とエラー | 事象 | 復元開始の後、復元成功または復元失敗の判定より前 | 復元試行とは、Herdr が保存値から Claude Code の再開を実行し結果を受け取る一連の処理を指す | 復元試行 |
