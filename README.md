# dowel（名称暫定）

C を対象とするビルドシステム（C++ 対応は計画段階）。CMake / Bazel / Meson に
対する代替として、次の3点を差別化点に置く。

1. **増分評価** — マニフェスト評価をメモ化クエリのグラフとして構成し、再構成レイテンシを削減する
2. **型・診断・来歴** — 全ての値が型とソース位置と来歴を持ち、`dowel why` で伝播経路を辿れる
3. **開発体験** — 言語サーバ、qemu 等のランナー、デバッガ設定の自動生成を一体で提供する

常駐デーモンを持たない。状態の正本はディスク上のストアに置き、CLI プロセスが
自己完結して動作する。

## クイックスタート

配布物はまだ無い。ソースからビルドする。Rust ツールチェーン、C コンパイラ、
ninja（推奨）が要る。

```sh
git clone https://github.com/sabas0ba/dowel
cd dowel
cargo build --release
export PATH="$PWD/target/release:$PATH"

cd examples/hello/app
dowel check                  # 計画まで走らせ、診断のみ出す。実行はしない
dowel build                  # ninja を生成して実行する
./.dowel/build/*/bin/app

cd ../libgreet
dowel test                   # test ターゲットをビルドして走らせる
dowel why app:app includes   # 値がそこへ来た経路を辿る
```

自分のプロジェクトを作る手順は
[docs/61-getting-started.md](docs/61-getting-started.md) にある。

## ドキュメント

利用者向けは「使い方（howto）」と「仕様（リファレンス）」の2系統に分かれる。

| 知りたいこと | 文書 |
|---|---|
| 導入から最初のビルドまで | [docs/61-getting-started.md](docs/61-getting-started.md) |
| タスク別の使い方（テスト、クロス実行、エディタ、CI 連携） | [docs/62-guides.md](docs/62-guides.md) |
| マニフェストの仕様（`dowel.toml` / `dowel.build`、型と併合） | [docs/10-manifest.md](docs/10-manifest.md) |
| コマンドとオプションの仕様 | [docs/60-cli.md](docs/60-cli.md) |
| いま何が動き、何が未実装か | [docs/91-implementation-status.md](docs/91-implementation-status.md) |

設計文書（動機、内部構造、決定の根拠）を含む全一覧は
[docs/README.md](docs/README.md) にある。文書はこのリポジトリを GitHub Pages で
公開するとそのまま閲覧できる（main ブランチ・`/ (root)`。設定は
[`_config.yml`](_config.yml)）。

## 現在の状態

実装着手済み・開発中。`dowel check` / `build` / `test` / `why` / `graph` /
`schema dump` / `cache` / `lsp` が動く。複数パッケージの C を実際にコンパイルし、
静的ライブラリを作り、リンクして実行できる。クロス実行のランナー
（`[runner.<triple>]`）、増分評価、評価結果の永続化、言語サーバ
（診断とホバー）も動いている。

未実装の主なもの: 依存の取得（現状はローカルパス依存のみ）と `dowel.lock`、
C++、`dowel debug`、既存ビルドシステムからの移行ツール。一覧と計測は
[docs/91-implementation-status.md](docs/91-implementation-status.md) を参照。
実装順序の計画は [docs/90-roadmap.md](docs/90-roadmap.md) にある。

検証はひとつの入口にまとめてある。ローカルでも CI でも同じものが走る。

```sh
make verify      # 全段階を実行し、結果を .work/verify/ に残す
```

## 開発

開発は [sabas0ba/dotfiles](https://github.com/sabas0ba/dotfiles) が定義する
Nix / direnv 環境、およびそこから構築する同一内容のコンテナ環境の上で行う。
ホストへツールを直接導入しない。

手順は [docs/50-development.md](docs/50-development.md)、テストの設計は
[docs/51-testing.md](docs/51-testing.md)、Claude Code 向けの指示は
[CLAUDE.md](CLAUDE.md) を参照。

## 名称について

`dowel`（木材接合用のダボ）は木工の接合具に由来し、FFI と依存の接合を
主題に置く意図による。選定基準、他候補、名前空間と商標の調査結果は
[docs/adr/0006-naming.md](docs/adr/0006-naming.md) を参照。

## ライセンス

[Apache-2.0](LICENSE)
