# dowel（名称暫定）

C/C++ を主対象とするビルドシステム。CMake / Bazel / Meson に対する代替として、
次の3点を差別化点に置く。

1. **増分評価** — マニフェスト評価をメモ化クエリのグラフとして構成し、再構成レイテンシを削減する
2. **型・診断・来歴** — 全ての値が型とソース位置と来歴を持ち、`why` で伝播経路を辿れる
3. **開発体験** — 言語サーバ、qemu 等のランナー、デバッガ設定の自動生成を一体で提供する

常駐デーモンを持たない。状態の正本はディスク上のストアに置き、CLI プロセスが自己完結して動作する。

## 現在の状態

実装着手済み。[docs/90-roadmap.md](docs/90-roadmap.md) の Phase 1〜2 を、
**縦に薄く貫通させる**方針で進めている。すなわち、増分クエリエンジンや
永続化ストアを完成させる前に、パーサから実際の C のコンパイルまでを
一度通し、そのうえで各層を厚くする。

現時点で `dowel check` / `dowel build` / `dowel why` / `dowel graph` /
`dowel schema dump` が動く。複数パッケージの C を実際にコンパイルし、
静的ライブラリを作り、リンクして実行できる。

```sh
cargo build --release

dowel check            # 評価と診断のみ。ビルドしない
dowel build            # ninja を生成して実行する
dowel why app:app includes
dowel graph --kind=action --format=dot | dot -Tsvg -o actions.svg

DOWEL_LOG=trace dowel build   # 依存グラフと各アクションのコマンドをログに出す
```

検証はひとつの入口にまとめてある。ローカルでも CI でも同じものが走る。

```sh
make verify      # 全段階を実行し、結果を .work/verify/ に残す
```

実装状況と計測結果は
[docs/91-implementation-status.md](docs/91-implementation-status.md) を参照。

## 文書構成

| 文書 | 内容 |
|---|---|
| [docs/00-overview.md](docs/00-overview.md) | 目標・非目標、既存システムに対する位置づけ |
| [docs/10-manifest.md](docs/10-manifest.md) | マニフェスト仕様（`dowel.toml` / `dowel.build`） |
| [docs/20-architecture.md](docs/20-architecture.md) | クエリエンジン、永続化ストア、言語サーバ |
| [docs/30-devexp.md](docs/30-devexp.md) | ランナー、デバッガ連携、エディタ連携 |
| [docs/40-migration.md](docs/40-migration.md) | 既存ビルドシステムからの移行 |
| [docs/50-development.md](docs/50-development.md) | 開発環境（Nix / コンテナ）と規約 |
| [docs/90-roadmap.md](docs/90-roadmap.md) | 実装順序と検証計画 |
| [docs/91-implementation-status.md](docs/91-implementation-status.md) | 実装状況、計測、設計文書との差異 |
| [docs/99-open-questions.md](docs/99-open-questions.md) | 未決事項 |
| [docs/adr/](docs/adr/) | 決定事項とその根拠 |

## 開発

開発は [sabas0ba/dotfiles](https://github.com/sabas0ba/dotfiles) が定義する
Nix / direnv 環境、およびそこから構築する同一内容のコンテナ環境の上で行う。
ホストへツールを直接導入しない。

手順は [docs/50-development.md](docs/50-development.md)、
Claude Code 向けの指示は [CLAUDE.md](CLAUDE.md) を参照。

## 名称について

`dowel`（木材接合用のダボ）は木工の接合具に由来し、FFI と依存の接合を
主題に置く意図による。選定基準、他候補、名前空間と商標の調査結果は
[docs/adr/0006-naming.md](docs/adr/0006-naming.md) を参照。
