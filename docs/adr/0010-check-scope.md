# ADR-0010: `check` は計画段まで走らせる

状態: Accepted

## 文脈

`check` は「評価と診断のみ。ビルドしない」と定めていた
（[60-cli.md](../60-cli.md)）。利用者はこれを、編集中や commit 前に
マニフェストの誤りを洗い出す入口として使う。

しかし次の入力は `check passed`（終了状態 0）となり、`build` で落ちていた。

| 入力 | 従来の `check` | `build` |
|---|---|---|
| `sources = [dir("src")]` | 通る | `invalid-source` |
| `sources = [file("src/nope.c")]` | 通る | `unresolved-path` |
| `sources = glob("nosuchdir/*.c")` | 通る | `empty-glob` + `no-sources` |

いずれも glob 展開とパス解決を伴う判定であり、これらは計画段にある。
評価の段に移すことはできない。評価時にファイルシステムを走査すると、
記録されない入力が評価結果に混ざる（[10-manifest.md](../10-manifest.md) 3節）。

## 決定

`check` は計画段まで走らせる。アクションを生成し、実行しない。

対象は全ターゲットとする。`build` の既定（`bin` と `test`、およびその
推移的依存）では、どこからも参照されないライブラリが検査から漏れる。

## 根拠

`check` の役割は「ビルドせずに誤りを洗い出す」ことであり、覆う範囲が
`build` より狭ければその役割を果たさない。`dowel graph --kind=action` は
既に同じ段まで走っており、新しい経路を作るものではない。

範囲を文書に明記して現状を保つ案もあった。しかし「`check` が通っても
`build` は落ちうる」という規則は、利用者が覚えて運用するものになる。
`check` を通す作業と `build` を通す作業が別になるなら、`check` を
分けて持つ理由が薄い。

## 影響

起動予算（無操作時 10ms、[20-architecture.md](../20-architecture.md) 5.4）に
glob 展開とパス解決の時間が加わる。2パッケージ・2ターゲットの構成で
測った値は次のとおり。

| 実行 | 変更前（中央） | 変更後（中央） |
|---|---:|---:|
| `dowel check` | 2.23ms | 2.49ms |

規模のフィクスチャ（[51-testing.md](../51-testing.md)「今後」）を足した際に
改めて測る。走査の対象が増えれば、この差はソース数に比例して伸びる。

`check` と `build` が同じ診断を出すことは
`check_reports_everything_build_reports`（`diagnostics.rs`）が検査する。
計画段に診断を足して `check` から見えない状態になると、この検査が落ちる。
