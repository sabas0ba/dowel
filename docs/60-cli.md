# コマンド

`dowel` の表面。ここに書いたものは実際に動く（動かないものは
[91-implementation-status.md](91-implementation-status.md) の未実装表にある）。

## 一覧

```sh
cargo build --release            # target/release/dowel

dowel check                      # 評価と診断のみ。ビルドしない
dowel build                      # 実際にビルドする
dowel build --config=release
dowel test                       # test ターゲットをビルドして走らせる
dowel test --nocapture           # テストの出力を素通しする
dowel test --failed --fail-fast  # 前回落ちた分だけ、最初の失敗で打ち切る
dowel test --test-jobs=4         # 同時に4本走らせる
dowel test --target=<triple>     # 宣言されたランナー経由で起動する
dowel why app:app includes       # 値がそこへ来た経路
dowel graph --format=dot         # 依存グラフ
dowel graph --kind=action        # アクショングラフ
dowel schema dump                # スキーマと構成語彙（機械可読）
```

デバッグ時の観測は環境変数か `--log-level` で行う。

```sh
DOWEL_LOG=debug dowel build      # 段階ごとの所要時間、グラフの規模
DOWEL_LOG=trace dowel build      # 依存グラフの辺、各アクションのコマンド
dowel check --log-format=json    # 1行1オブジェクト
```

出力先は分けてある。**stdout は成果物**（JSON 診断、グラフ、スキーマ、`why`）、
**stderr は進行とログ**。したがって `dowel graph --format=dot | dot -Tsvg` は
ログ水準に関わらず動く。

動く例は [`examples/hello`](../examples/hello) にある。
`crates/dowel-cli/tests/example.rs` が現物をビルドして検査しているため、
構文や意味論を変えた際の更新漏れは検出される。


## 終了状態

| 状態 | 意味 |
|---|---|
| 0 | 成功。診断が警告のみの場合も含む |
| それ以外 | 誤りがあった。診断は上の約束どおり stdout / stderr に分かれて出る |

`dowel test` は、テストが1件でも落ちれば 0 以外を返す。
`--fail-fast` で打ち切った場合、走らせなかった件数を要約に出す。

## 診断の機械可読形式

`--message-format=json` で1行1診断の JSON を stdout に出す。
各診断は安定コード（`unknown-property` など）を持ち、これは互換性の対象である。
コードの一覧と、それぞれを出す最小の入力は
`crates/dowel-cli/tests/diagnostics.rs` の事例表にある。
