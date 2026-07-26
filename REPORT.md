# Codex restoreが通常起動と同じ権限条件を維持する

> 「リストアするとCodexの権限が弱まる気がしています。」
>
> 「この問題解決してください。コミット、プッシュ、バイナリ差し替え、再起動まで一貫して行ってください。メインブランチだけでお願いします。」

## 課題

通常のzshから`codex`を起動すると、aliasによってsandbox、network、approvalのオプションが追加されます。一方、Herdrのsession restoreはlegacy templateを`sh -lc`で実行していたためzsh aliasを読まず、同じCodex binaryを`codex resume <session-id>`だけで起動していました。

実sessionでは、通常起動直後の`approval_policy`は`never`でしたが、restore後は`on-request`へ変わっていました。`sandbox_policy`は両方とも`danger-full-access`であり、体感していた権限低下は承認条件の差でした。

## 対策

Codexの新規pane起動で既に使っている構造化設定`[agent_start.commands].codex`を、session restoreでも同じ順序のまま使います。restore時はそのargvの末尾へ`resume`とpane固有session IDだけを追加します。

Codexの構造化起動argvがある時はlegacy restore templateより優先するため、`sh -lc`やshell aliasへ依存しません。構造化argvが無い既存環境では、legacy templateを従来どおり維持します。他agentのrestore経路は変更しません。

修正前のrestoreは、実際には次のコマンドでした。`sh -lc`はzshのaliasを読まないため、Codexへsandbox・network・approvalのオプションが渡りません。

```sh
sh -lc 'codex resume <session-id>'
```

現在の新規pane起動は、`[agent_start.commands].codex`から次のargvを使います。

```sh
codex --sandbox workspace-write --config sandbox_workspace_write.network_access=true --dangerously-bypass-approvals-and-sandbox
```

修正後のrestoreは、同じargvの末尾へ`resume`と保存済みsession IDだけを追加します。

```sh
codex --sandbox workspace-write --config sandbox_workspace_write.network_access=true --dangerously-bypass-approvals-and-sandbox resume <session-id>
```

## 達成したこと

- `RestoreOptions`へ構造化agent起動argvを渡し、Codex restore planへ反映
- sandbox、network、approval、wrapperの全オプションを順序込みで維持
- legacy templateが併存しても構造化Codex argvを優先する回帰テスト
- 構造化Codex argvが無い場合にlegacy templateを維持する回帰テスト
- `SPEC.md`へ新規paneとrestoreの起動条件一致を恒久契約として追加
- next CHANGELOG、英語・日本語・簡体字中国語configuration、config referenceを更新
- Codex restore重点テスト: 2 / 2 pass
- 既存restore plan重点テスト: 3 / 3 pass
- SPEC / config reference / docs translation tests: 20 / 20 pass
- native fmt / clippy / Rust nextest: 2891 / 2891 pass
- integration assets: 2 / 2 pass
- plugin marketplace: 12 / 12 pass
- maintenance scripts: 80 / 80 pass
- fork distribution docs: 11 current guidance files / pass
- `ZIG=/usr/bin/true just build`: release build成功
- Mac上のWindows cross-clippyだけは、`libsqlite3-sys`のCコンパイル時にWindows SDKの`stdlib.h`が存在せず停止。native実装・テスト完了後の環境依存停止であり、今回のコード失敗ではない

## 未達成のこと

- `main`へのcommitと`origin/main`へのpush
- `/Users/kazuph/.local/bin/herdr`のrelease binary差し替えとSHA-256・codesign・version検証
- Herdr server再起動
- restore後のCodex実process argvとsession権限が通常起動条件と一致するlive検証
- Herdr repoの`main`以外のworktree・local branch・origin branch削除
- 作業前に退避したmainの既存未コミット変更の復元

`[agent_start.commands].codex`をCodexの新規起動とrestore起動の共通argvにし、session再接続引数だけを追加する修正として承認できるか確認してください。承認後、上記の未達成項目を一貫して完了します。
