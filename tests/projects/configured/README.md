# configured — 構成で分岐するプロジェクト

```
app                      json（任意の依存）
├── lib.core  ──private──▶ lib.json   （feature.json のとき）
├── bin.app   ──target───▶ lib.core
└── test.config ─target──▶ lib.core
```

`[features]` の宣言は次のとおり。

```toml
default = ["fast"]
fast    = ["simd"]
simd    = []
trace   = []
json    = []
```

## 何を固定するか

1. **`match cfg.opt` の全てのアーム。** `--config` を切り替えると
   `APP_OPT` が変わる。単一の構成では片方のアームしか通らない
2. **列の要素に書いた `match`。** 具体化した結果は列の中の列になる。
   1段しか解かないと、`check` も `dowel why` も通るのにコンパイル引数にだけ
   現れない（この形で欠陥が発覚した）
3. **機能の連鎖。** `fast` は `simd` を有効にする。`--features=fast` だけを
   渡しても `APP_SIMD` が立つこと
4. **既定の解除。** `--no-default-features` で連鎖ごと消えること
5. **明示した機能は既定に加わる。** `--features=trace` で `fast` も残ること
6. **有効でない任意の依存を読まない。** 既定では `json` が依存グラフに現れない
7. **任意の依存の公開定義が届く。** `--features=json` で `APP_JSON` が立ち、
   `core` からは `json` の公開ヘッダが見えること
8. **非公開は伝播しない。** `json` は `core` の非公開依存であり、
   `core` を使う `bin.app` へ `JSON_API` が漏れてはならない

1〜5 と 8 は C 側の `#error` と終了状態で書いてある。6 と 7 のうち
依存グラフに関わる部分は C から観測できないため、ハーネス側にある。

## 実行ファイルの出力

```
opt=0 fast=1 simd=1 trace=0 json=0
```

構成ごとの期待値は `crates/dowel-cli/tests/fixture.rs` の
`configured_reflects_every_configuration` にある。

`app/tests/config_test.c` は、この行とコンパイル時に見えている識別子が
一致することを確かめる。同じ構成から2通りに導いて突き合わせるため、
期待値をハーネスと二重に持たない。
