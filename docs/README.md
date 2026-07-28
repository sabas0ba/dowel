# 文書の地図

## 何から読むか

| 目的 | 読むもの |
|---|---|
| これは何か、何をしないか | [00-overview.md](00-overview.md) |
| 実際に何が動くか | [91-implementation-status.md](91-implementation-status.md) |
| コマンドの使い方 | [60-cli.md](60-cli.md) |
| マニフェストの書き方 | [10-manifest.md](10-manifest.md) |
| 手を入れる | [50-development.md](50-development.md) → [51-testing.md](51-testing.md) |

## 番号の規約

十の位が主題、一の位が同じ主題の中の分冊である。

| 帯 | 主題 |
|---|---|
| `0x` | 全体の位置づけ |
| `1x` | マニフェスト言語の仕様 |
| `2x` | 内部構造 |
| `3x` | 開発体験（ランナー、デバッガ、エディタ） |
| `4x` | 既存ビルドシステムからの移行 |
| `5x` | 本リポジトリの開発 |
| `6x` | 利用者向けのリファレンス |
| `9x` | 計画と現況 |
| `99` | 未決事項 |

新しい文書は、既存の帯に収まるならその帯へ足す。収まらない主題が出た場合に
新しい帯を起こす。番号を詰め直さない — 文書は本文中と原典（コード中の
コメント）の双方から参照されており、付け替えると参照が切れる。

## 一覧

| 文書 | 何が書いてあるか |
|---|---|
| [00-overview.md](00-overview.md) | 目標・非目標、既存システムに対する位置づけ |
| [10-manifest.md](10-manifest.md) | マニフェスト仕様（`dowel.toml` / `dowel.build`）、型と併合意味論 |
| [20-architecture.md](20-architecture.md) | 増分クエリエンジン、永続化ストア、言語サーバの内部構造 |
| [30-devexp.md](30-devexp.md) | ランナー抽象、デバッガ連携、エディタ連携 |
| [40-migration.md](40-migration.md) | 既存ビルドシステムからの移行 |
| [50-development.md](50-development.md) | 開発環境（Nix / コンテナ）と規約 |
| [51-testing.md](51-testing.md) | テストスイートの設計。層ごとの責務と、テストを足すときの判断 |
| [60-cli.md](60-cli.md) | コマンド、出力の約束、ログとデバッグ |
| [90-roadmap.md](90-roadmap.md) | 実装順序と検証計画（**計画**） |
| [91-implementation-status.md](91-implementation-status.md) | 実装状況、計測、設計文書との差異（**現況**） |
| [99-open-questions.md](99-open-questions.md) | 未決事項 |
| [adr/](adr/README.md) | 決定事項とその根拠 |

## 文書の扱い

- **決定は [ADR](adr/README.md) に記録する。** 覆す場合は当該 ADR を Superseded とし、
  新しい ADR を追加する。既存の ADR を書き換えない
- **未決は [99-open-questions.md](99-open-questions.md) に集約する。**
  決まったら ADR へ移し、当該項目を削除する
- **計画と現況を混ぜない。** [90-roadmap.md](90-roadmap.md) は「こうする」、
  [91-implementation-status.md](91-implementation-status.md) は「こうなっている」。
  両者が食い違う場合、後者が現況を示す
- **実装が仕様から外れた場合は、隠さず差異として記録する。**
  91 の「設計文書との差異」節がその置き場である

この地図と一覧は
`crates/dowel-cli/tests/docs.rs` が検査している。文書を足して一覧へ書き忘れると落ちる。
