# dowel の VS Code 拡張

マニフェスト（`dowel.toml` / `dowel.build`）の編集支援。
`dowel lsp`（[docs/30-devexp.md](../../docs/30-devexp.md) 3.2）を起動して
診断とホバーを受け取り、`dowel.build` には構文強調を付ける。

`dowel` という名前は仮称である（[ADR-0006](../../docs/adr/0006-naming.md)）。
名称が確定するまで市場（Marketplace）へは公開しない（`"private": true`）。

## できること

- **診断** — 開いているファイル1つを単位とした、構文解析と評価の結果。
  `dowel.toml` には厳密な TOML の検査（[ADR-0003](../../docs/adr/0003-manifest-split.md)）が加わる。
  ファイルを跨ぐ診断がまだ出ないことを含め、範囲はサーバ側
  （`dowel_lsp::UNSUPPORTED`）が決める
- **ホバー** — プロパティの型と併合規則、表の見出しの各段、組み込み関数の署名、
  構成キーの値域。出所はスキーマそのもの
- **構文強調** — `dowel.build` のみ。`dowel.toml` は厳密な TOML なので、
  既存の TOML 拡張に任せる（言語登録を奪って衝突させない）

## 必要なもの

`dowel` の実行ファイル。PATH に無い場合は設定 `dowel.server.path` で指す。
サーバは `dowel lsp` として起動され、エディタと共に終了する
（常駐しない。[ADR-0002](../../docs/adr/0002-no-daemon.md)）。

## 設計

実行時依存はゼロである。クライアントライブラリ（`vscode-languageclient`）を
使わず、`src/protocol.ts`（枠付け）と `src/connection.ts`（要求と応答の
対応付け）を自前で持つ。サーバの能力が全文同期・診断・ホバーに絞られている
（`crates/dowel-lsp`）ため、この範囲では自前の方が小さく、npm の供給網に
晒される面も狭い。開発時依存は `typescript`、型定義2つ、E2E 用の
`@vscode/test-electron`（公式ハーネス）に留める。

| ファイル | 責務 |
|---|---|
| `src/protocol.ts` | `Content-Length` の枠付け。vscode 非依存 |
| `src/connection.ts` | JSON-RPC の対応付けと通知の配送。vscode 非依存 |
| `src/extension.ts` | プロセスの起動・再起動、文書の同期、診断とホバーの受け渡し |
| `syntaxes/dowel-build.tmLanguage.json` | 表示のための近似。正確な解析はサーバ |

## 開発

Node と npm はホストに置かず、コンテナ経由で使う（`dev.sh`）。

```sh
cd editors/vscode
./dev.sh npm ci           # 依存の取得
./dev.sh npm test         # ビルドと検査
```

統合テスト（`test/integration.test.ts`）は実物の `dowel lsp` と話す。
バイナリはリポジトリの `target/` から探すか、`DOWEL_LSP_BIN` で指す。
コンテナの libc はホストより古いことがあるため、musl 版を作っておくと確実に動く。

```sh
cargo build -p dowel-cli --target x86_64-unknown-linux-musl
```

実物の VS Code での E2E（`test/e2e/`）は `@vscode/test-electron` が
VS Code 本体を `.vscode-test/` に取得して回す。診断が UI に届き、直せば
消え、ホバーが出るところまでを拡張ホストの API で確かめる。Electron は
表示先と新しめの libc を要するため、この検査だけはコンテナの外で回す。

```sh
xvfb-run -a npm run test:e2e    # 画面のない環境。あるならそのまま npm run test:e2e
```

手元で試すには、このディレクトリを VS Code で開いて F5（Extension
Development Host）を使うのが早い。`.vsix` が要る場合は
`./dev.sh npx @vscode/vsce package` で作れる。
