# dowel ドキュメント

利用者向けの文書（使い方と仕様）と、プロジェクト内部の文書（設計・開発・計画）の索引。

## 使い方（How-to）

| 文書 | 内容 |
|---|---|
| [61-getting-started.md](61-getting-started.md) | 導入から、最初のプロジェクトのビルド・テスト・実行まで |
| [62-guides.md](62-guides.md) | タスク別の使い方。ビルド、テスト、構成と機能フラグ、来歴の調査、クロス実行、エディタ、キャッシュ、CI 連携 |

動く現物は [`examples/hello`](../examples/hello) にある。

## 仕様（リファレンス）

| 文書 | 内容 |
|---|---|
| [10-manifest.md](10-manifest.md) | マニフェスト仕様。`dowel.toml` / `dowel.build` に書ける全て、型と併合意味論 |
| [60-cli.md](60-cli.md) | コマンド仕様。全オプション、出力の約束、終了状態、診断の機械可読形式 |
| [91-implementation-status.md](91-implementation-status.md) | いま何が動くか。未実装の一覧、計測、設計文書との差異 |

仕様には設計上の全体像を含むため、一部に未実装の要素がある。未実装のものは
仕様側に注記し、91 に一覧を置く。両者が食い違う場合は 91 を現況とみなす。

## 設計（背景と内部構造）

| 文書 | 内容 |
|---|---|
| [00-overview.md](00-overview.md) | 目標・非目標、既存システムに対する位置づけ |
| [20-architecture.md](20-architecture.md) | 増分クエリエンジン、永続化ストア、言語サーバの内部構造 |
| [30-devexp.md](30-devexp.md) | ランナー抽象、デバッガ連携、エディタ連携の設計 |
| [40-migration.md](40-migration.md) | 既存ビルドシステムからの移行の設計（移行コマンドは未実装） |
| [adr/](adr/README.md) | 決定事項とその根拠（ADR） |
| [90-roadmap.md](90-roadmap.md) | 実装順序と検証計画 |
| [99-open-questions.md](99-open-questions.md) | 未決事項 |

## このリポジトリを開発する

| 文書 | 内容 |
|---|---|
| [50-development.md](50-development.md) | 開発環境（Nix / コンテナ）と規約 |
| [51-testing.md](51-testing.md) | テストスイートの設計。層ごとの責務と、テストを足すときの判断 |

## GitHub Pages での公開

本リポジトリは main ブランチをそのまま GitHub Pages で公開できる
（Settings → Pages → Deploy from a branch → `main` / `/ (root)`）。
設定は [`_config.yml`](../_config.yml) にある。公開時は Markdown 間の相対リンクが
HTML へ解決され、各ディレクトリの README がそのディレクトリの索引ページになる。
文書は追加の変換なしにリポジトリ上でもサイト上でも同じ経路で読めるよう、
相対リンクのみで書く。

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
| `6x` | 利用者向けの文書（リファレンスと howto） |
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
| [60-cli.md](60-cli.md) | コマンド仕様、出力の約束、ログとデバッグ |
| [61-getting-started.md](61-getting-started.md) | 導入から最初のビルドまでの howto |
| [62-guides.md](62-guides.md) | タスク別の使い方ガイド |
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
- 利用者向けの文書（10 / 60 / 61 / 62）に書くのは動くものを基本とし、
  未実装の要素を載せる場合はその旨を明記する

## 検査されるもの

文書の不整合はビルドにもテストにも影響しないため、検査しない限り検出されない。
`crates/dowel-cli/tests/docs.rs` が機械的に判定できる範囲を見る。

| 対象 | 落ちる条件 |
|---|---|
| 相対リンク | 指す先が存在しない |
| ソースとスクリプトが参照する文書 | 文書番号を変えてリンク以外の参照が残った |
| 本一覧 | 文書を追加して記載しなかった。記載を消さずに文書を消した |
| [adr/README.md](adr/README.md) の表 | ADR を追加して記載しなかった。その逆 |
| [91-implementation-status.md](91-implementation-status.md) のクレート表 | クレートを追加して記載しなかった。その逆 |

記述内容の妥当性は検査しない。設計は [51-testing.md](51-testing.md) にある。
