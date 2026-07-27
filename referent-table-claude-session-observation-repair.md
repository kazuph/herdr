| 出典 | 目的 | 具体対象 | 役割 | 前後関係 | 初出定義 | 候補語 |
|---|---|---|---|---|---|---|
| ユーザー依頼 | 旧forkで動作していたClaude Code復元を回帰修正する | hookを使わずに実行中paneとClaude Codeの正確なsession IDを結び付ける処理 | 手段 | Claude Code起動後、session snapshot保存前 | session観測とは、paneのforeground processとClaude Codeのprocess recordまたはtranscriptを照合して正確なsession IDを得る処理を指す | session観測 |
| SPEC G3 | 同じcwdの別会話を誤復元しない | foreground PID、cwd、process開始時刻、session fileが一意に一致する条件 | 開始条件 | session観測の開始後、session ID採用前 | 一意一致条件とは、paneとsession fileを識別する複数の証拠が一つのsessionだけを指す条件を指す | 一意一致条件 |
| SPEC G3 | 再起動後にもpane固有session IDを残す | workspace、tab、pane、terminal、cwd、agent、session IDを保持するディスク上の記録 | 記録 | session ID採用後、server再起動前 | session台帳とは、pane固有session IDと所有境界を再起動をまたいで保持する記録を指す | session台帳 |
| 現行native restore | 保存された同じ会話を再開する | 保存済みsession IDを既存のagent start argvへ追加したClaude Code起動計画 | 記録 | session snapshot読込後、Claude Code起動前 | 復元計画とは、保存済みsession IDを同じ起動条件でClaude Codeへ渡す引数列を指す | 復元計画 |
| 回帰検証 | p_45とp_101を次回再起動可能にする | 現在agent_sessionが空だがClaude process recordとtranscriptに正確なIDがある2 pane | 状態 | 修正前は復元不能、修正後はsaved session | 復元可能状態とは、pane固有session IDがHerdr snapshotとsession台帳に保存された状態を指す | 復元可能状態 |
