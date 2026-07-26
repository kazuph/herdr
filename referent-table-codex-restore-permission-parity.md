| 出典 | 目的 | 具体対象 | 役割 | 前後関係 | 初出定義 | 候補語 |
|---|---|---|---|---|---|---|
| ユーザー依頼 | restore後もユーザーが起動したCodexと同じ権限条件を維持する | `[agent_start.commands].codex`に保存された実行ファイルと通常起動オプション | 記録 | 新規paneでCodexを起動する時 | Codex起動argvとは、新規paneとrestoreの両方が共有するCodex実行ファイルと通常起動オプションを指す | Codex起動argv |
| Herdr session restore | 保存済みCodex sessionへ同じ権限条件で再接続する | Codex起動argvの末尾へ`resume`と保存済みsession IDを一度だけ追加したargv | 記録 | Codex sessionをrestoreする時 | Codex restore argvとは、Codex起動argvへsession再接続引数だけを追加したargvを指す | Codex restore argv |
| 既存設定 | agent固有の複雑なresume commandを維持する | `[agent_restore.commands].<agent>`に明示されたlegacy shell template | 記録 | Codex起動argvが未設定の時 | legacy restore templateとは、shell評価を必要とする既存の文字列型resume commandを指す | legacy restore template |
| ユーザー依頼 | 通常起動とrestore起動の権限差を防ぐ | Codex restore argvがCodex起動argvの実行ファイルと通常起動オプションを順序込みで保持する条件 | 目的 | restore commandを構築した後 | Codex起動一致とは、session再接続引数以外のargvが新規pane起動とrestoreで一致することを指す | Codex起動一致 |
| 回帰検証 | shell aliasに依存せず起動一致を証明する | `sh -lc`、zsh alias、PATH上の別wrapperを介さず構造化argvからrestore commandを生成する試験 | 手段 | 実装後 | 構造化argv検証とは、shell初期化ファイルに依存せず引数列の一致を直接検証することを指す | 構造化argv検証 |
