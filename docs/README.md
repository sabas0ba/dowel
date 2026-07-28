# 文書一覧

## 目的別の参照先

| 目的 | 文書 |
|---|---|
| 目標と非目標 | [00-overview.md](00-overview.md) |
| 実装済みの機能 | [91-implementation-status.md](91-implementation-status.md) |
| コマンドの仕様 | [60-cli.md](60-cli.md) |
| マニフェストの記述方法 | [10-manifest.md](10-manifest.md) |
| 開発への参加 | [50-development.md](50-development.md) → [51-testing.md](51-testing.md) |

## 番号の規約

十の位が主題を、一の位が同一主題内の分冊を表す。

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

新しい文書は、既存の帯に収まるならその帯に追加する。収まらない主題が生じた場合に
新しい帯を追加する。番号の再割り当ては行わない。文書番号は Markdown のリンクと
ソースコード中のコメントの双方から参照されており、変更すると参照が解決しなくなる。

## 一覧

| 文書 | 内容 |
|---|---|
| [00-overview.md](00-overview.md) | 目標・非目標、既存システムに対する位置づけ |
| [10-manifest.md](10-manifest.md) | マニフェスト仕様（`dowel.toml` / `dowel.build`）、型と併合意味論 |
| [20-architecture.md](20-architecture.md) | 増分クエリエンジン、永続化ストア、言語サーバの内部構造 |
| [30-devexp.md](30-devexp.md) | ランナー抽象、デバッガ連携、エディタ連携 |
| [40-migration.md](40-migration.md) | 既存ビルドシステムからの移行 |
| [50-development.md](50-development.md) | 開発環境（Nix / コンテナ）と規約 |
| [51-testing.md](51-testing.md) | テストスイートの設計。層ごとの責務と、テストを足すときの判断 |
| [60-cli.md](60-cli.md) | コマンド、出力の約束、ログとデバッグ |
| [90-roadmap.md](90-roadmap.md) | 実装順序と検証計画 |
| [91-implementation-status.md](91-implementation-status.md) | 実装状況、計測、設計文書との差異 |
| [99-open-questions.md](99-open-questions.md) | 未決事項 |
| [adr/](adr/README.md) | 決定事項とその根拠 |

## 文書の扱い

- 決定は [ADR](adr/README.md) に記録する。決定を覆す場合は当該 ADR を Superseded とし、
  新しい ADR を追加する。既存の ADR は書き換えない
- 未決事項は [99-open-questions.md](99-open-questions.md) に集約する。
  決定したものは ADR へ移し、当該項目を削除する
- 計画と現況を分離する。[90-roadmap.md](90-roadmap.md) は計画、
  [91-implementation-status.md](91-implementation-status.md) は現況を記述する。
  両者が食い違う場合は後者を現況とみなす
- 実装が仕様と異なる場合は、91 の「設計文書との差異」節に記録する

本一覧と `docs/` の中身が一致することは `crates/dowel-cli/tests/docs.rs` が検査する。
文書を追加して一覧に記載しなかった場合、検証が失敗する。
