# 使い方ガイド

タスク別の howto。ここに書いた機能は実装済みである。導入と最初のビルドは
[61-getting-started.md](61-getting-started.md)、オプションの完全な一覧は
[60-cli.md](60-cli.md)、マニフェストの記法は [10-manifest.md](10-manifest.md) にある。

## 1. ビルドする

```sh
dowel build                      # 全ての bin / test をビルドする
dowel build app                  # 名指し。<target> または <package>:<target>
dowel build --config=release
```

- 構成は `--config` で切り替える（`debug` / `release`。既定は `debug`）。
  ビルドディレクトリは構成ごとに分かれ、切り替えても互いの成果物を壊さない
- 実行器は既定で ninja。無い環境では `--executor=direct`（逐次）が使える
- `-j/--jobs` は ninja へ渡す並列度
- `compile_commands.json` は毎回書き出される。抑止は `--no-compdb`

## 2. テストを回す

```sh
dowel test                       # 全ての test ターゲット
dowel test app:unit              # 名指し
dowel test --nocapture           # 出力を素通しする（既定は失敗した分のみ表示）
dowel test --failed              # 前回落ちた分だけ再実行する
dowel test --fail-fast           # 最初の失敗で打ち切る
dowel test --test-jobs=4         # 同時に4本走らせる（既定は逐次）
dowel test --no-run              # ビルドのみ。実行しない
```

- 合否は終了状態で判定する（0 = 成功）。作業ディレクトリはパッケージルート
- 並列の既定が逐次なのは、C のテストが共有資源（同じ作業ディレクトリ、
  固定のポート、書き出し先）を使う場合があるため。表示は常に要求順
- `--failed` の判定はビルドディレクトリに残る。走らせなかったターゲットの判定は消えない

## 3. 構成と機能フラグを切り替える

マニフェスト側の分岐は `match` / `when` で書く（[10-manifest.md](10-manifest.md) 2節）。
CLI 側から与えるのは以下。

```sh
dowel build --config=release
dowel build --features=zlib,png
dowel build --no-default-features
```

機能フラグの値域は `dowel.toml` の `[features]` が決める。未知の名前は
`dowel.build` からの参照でも `--features` でも診断で落ち、候補が提示される。
`--config` / `--target` の切り替えでマニフェスト評価は再実行されない
（分岐の解決は具体化段階まで遅延される）。

## 4. 「なぜ」を調べる

値の来歴。伝播した値がどの記述からどう届いたかを、ソース位置つきで出す。

```
$ dowel why app:app includes

include/                          Path
  ← public.includes of target:foo       libfoo/dowel.build:18
    ← deps of target:app                app/dowel.build:7
```

グラフ。ターゲット依存グラフとアクショングラフを text / dot / json で出す。

```sh
dowel graph                              # ターゲット依存グラフ
dowel graph --kind=action                # アクショングラフ
dowel graph --format=dot | dot -Tsvg -o graph.svg
```

再ビルドの理由や実際のコマンド列はログで追う。

```sh
DOWEL_LOG=debug dowel build      # 段階ごとの所要時間、グラフの規模、最新性判定
DOWEL_LOG=trace dowel build      # 依存グラフの辺、各アクションの完全なコマンド列
```

trace の出所別の内訳は [91-implementation-status.md](91-implementation-status.md) にある。

## 5. クロスコンパイルとランナー

ターゲットトリプルごとに実行ラッパを `[runner.<triple>]` で宣言すると、
`dowel test --target=<triple>` が透過的にラッパ経由で起動する。

qemu の例:

```toml
[runner.riscv64gc-unknown-linux-gnu]
command = "qemu-riscv64"
args    = ["-L", "/usr/riscv64-linux-gnu"]
```

実機（SSH）の例。対象機がビルド機のファイルシステムを参照できない場合は、
転送を宣言する。

```toml
[runner.aarch64-unknown-linux-gnu]
host       = "board.local"
remote_dir = "/tmp/dowel"
transfer   = ["scp", "-q"]
command    = "ssh"
args       = ["board.local"]
```

これは次のように展開される。転送元・転送先のパスはマニフェストに書かず、
実装が末尾に付け足す（[ADR-0008](adr/0008-runner-transfer.md)）。

```
scp -q <build>/bin/unit_test board.local:/tmp/dowel/unit_test
ssh board.local /tmp/dowel/unit_test
```

- `transfer` と `remote_dir` は同時に指定する
- 合否は起動コマンドの終了状態。`ssh` なら対象機側の終了状態がそのまま合否になる
- ホストと異なるトリプルでランナーが未宣言なら、起動前に診断で拒む
  （起動後の `Exec format error` がテストの失敗として報告される事態を避ける）

## 6. エディタで書く

経路は3つある。

| 対象 | 経路 |
|---|---|
| `dowel.build` / `dowel.toml` | `dowel lsp`。診断とホバーを返す言語サーバ |
| C ソース | clangd。`dowel build` が書き出す `compile_commands.json` を供給する |
| VS Code | [`editors/vscode/`](../editors/vscode/README.md) のクライアント。`dowel lsp` の起動と構文強調 |

`dowel lsp` は標準入出力で LSP を話し、エディタが起動主体となる（常駐しない）。
ホバーはプロパティの型と併合規則、組み込み関数の署名、構成キーの値域を出す。
診断は開いているファイル1つを単位とし、ファイルを跨ぐ診断はまだ出さない。

## 7. キャッシュを管理する

評価結果のメモは `.dowel/cache/` に置かれる。

```sh
dowel cache info                 # ストアの規模
dowel cache gc                   # 古い形式のストアを回収する
```

- `cache info` / `cache gc` はマニフェストを読まない。マニフェストが壊れた状態でも掃除できる
- ストアはいつ消しても正しさを失わない。失うのはキャッシュの利得だけ
- 書き手は1プロセスに限る（`flock`）。取得できないプロセスは読み込みのみを行う

## 8. CI・ツールから使う

出力は機械可読を選べる。stdout は成果物、stderr は進行とログという分担が
常に保たれるため、パイプは安全である。

```sh
dowel check --message-format=json    # 1行1診断。安定コード・位置・修正提案つき
dowel test  --message-format=json    # 1件1行のテスト結果
dowel build --log-format=json        # ログも JSON にする
dowel schema dump                    # スキーマと構成語彙（機械可読）
```

- 終了状態は 0 = 成功（警告のみを含む）、それ以外 = 誤り。
  `dowel test` はテストが1件でも落ちれば 0 以外を返す
- 診断コード（`unknown-property` 等）は互換性の対象である
- `dowel schema dump` の出力は、LLM エージェントへ文脈として与える用途も想定している
  （[30-devexp.md](30-devexp.md) 4節）

## 9. 診断を読む

- 人間向けは rustc 書式。重大度、安定コード、位置ラベル、注記、修正提案を持つ
- 未知の名前（プロパティ、関数、構成キー、機能名、`match` のアーム、
  CLI のオプション）には編集距離で候補が出る
- `--message-format=json` の修正提案は span と置換文字列を持ち、機械的に適用できる
- 迷ったら `dowel why`（値の来歴）と `DOWEL_LOG=debug`（実行の理由）で挟み撃ちにする
