# CLAUDE.md

本ファイルは本リポジトリ固有の指示である。
共通規約（[sabas0ba/dotfiles](https://github.com/sabas0ba/dotfiles) の
`CLAUDE.md` および README）を前提とし、衝突する場合は本ファイルを優先する。

## 環境

作業は dotfiles が定義する Nix 開発シェル、またはそこから構築したコンテナ環境の
内部で行う。ホストのグローバル環境を汚染しない。

```sh
cd ~/repos/dotfiles && nix develop     # 開発シェル
make docker-shell                       # コンテナ
```

環境の詳細、ツールの追加手順、規約は以下を参照する。

- [`docs/50-development.md`](docs/50-development.md) — 本プロジェクトの開発環境
- dotfiles の `README.md` — 規約の所在
- dotfiles の `CLAUDE.md` — Claude Code 向けの補足

本プロジェクトの実装に必要なツール（コンパイラ、リンカ、ninja、qemu 等）は
dotfiles 側の `nix/packages.nix` に追記して取得する。
開発シェルの外部でツールを導入しない。

## 現在の状態

設計検討段階。実装は未着手。本リポジトリは設計文書のみを含む。

指示がない限り、ファイルの作成や実装に着手しない。
検討中の内容の具象化を目的とした対話が主である。

## 文書の扱い

- 設計上の決定は `docs/adr/` に ADR として記録する。
  決定を覆す場合は当該 ADR を Superseded とし、新しい ADR を追加する。既存の ADR を書き換えない
- 未決事項は `docs/99-open-questions.md` に集約する。
  決定したものは ADR へ移し、当該項目を削除する
- 仕様の変更が ADR の根拠と矛盾する場合、文書を修正する前に指摘する

## 名称

`dowel` は仮称である（[docs/adr/0006-naming.md](docs/adr/0006-naming.md)）。
確定していないため、識別子やパス名として広範に埋め込む変更は行わない。

## 作業

- 機能追加は branch または worktree で行う
- 一時ファイルは `.work/`（git ignore 対象）に置く
- コミットは Conventional Commits
- 依存パッケージを増やす場合は事前に確認する
