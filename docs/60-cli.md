# コマンドリファレンス

`dowel` が提供する全コマンドとオプションの仕様。本文書に記載したものは実装済みである。
未実装の項目は [91-implementation-status.md](91-implementation-status.md) に一覧がある。
タスク別の使い方は [63-guides.md](63-guides.md) にある。

## 呼び出しの形

```
dowel <command> [options] [args]
```

- オプションは `--name value` と `--name=value` の双方を受ける
- 未知のコマンド・オプションには編集距離で候補を提示する
  （`--confg` → `did you mean --config?`）
- 引数なしの起動は使い方を表示する

## 全コマンド共通の約束

### 出力先

| 出力 | 内容 |
|---|---|
| stdout | 成果物。JSON 診断、グラフ、スキーマ、`why` の結果 |
| stderr | 進行とログ |

この分担により `dowel graph --format=dot | dot -Tsvg` はログ水準に依らず動作する。

### 終了状態

| 状態 | 意味 |
|---|---|
| 0 | 成功。診断が警告のみの場合も含む |
| それ以外 | 誤りがあった。診断は上の約束どおり stdout / stderr に分かれて出る |

`dowel test` は、テストが1件でも落ちれば 0 以外を返す。
`--fail-fast` で打ち切った場合、走らせなかった件数を要約に出す。

### 共通オプション

| オプション | 値 | 既定 | 意味 |
|---|---|---|---|
| `-C, --directory <path>` | パス | `.` | このディレクトリのパッケージを対象にする |
| `--config <name>` | `debug` / `release` | `debug` | ビルド構成 |
| `--target <triple>` | ターゲットトリプル | ホスト | クロスコンパイル先（[63-guides.md](63-guides.md) 5節） |
| `--features <a,b>` | カンマ区切り | — | 有効化する機能フラグ。繰り返し指定できる |
| `--no-default-features` | — | — | `[features]` の `default` を含めない |
| `--message-format <fmt>` | `human` / `json` | `human` | 診断の形式 |
| `-v, --verbose` | — | — | ログを増やす。1回で info、2回以上で debug |
| `--log-level <level>` | `off` / `error` / `warn` / `info` / `debug` / `trace` | — | ログ水準。明示指定は `-v` より優先する |
| `--log-format <fmt>` | `text` / `json` | `text` | ログの形式（1行1オブジェクト） |
| `--color <when>` | `auto` / `always` / `never` | `auto` | 色。`auto` は現状色なしに倒す（端末判定を持たないため）。必要なら `always` を明示する |
| `-h, --help` | — | — | 使い方を表示する |
| `-V, --version` | — | — | 版を表示する |

### 環境変数

| 変数 | 意味 |
|---|---|
| `DOWEL_LOG` | `--log-level` と同じ。`DOWEL_LOG=trace dowel build` |

ログ水準ごとに出る内容（debug: 段階ごとの所要時間とグラフの規模、
trace: 依存グラフの辺と各アクションの完全なコマンド列）の内訳は
[91-implementation-status.md](91-implementation-status.md) にある。

## `dowel check`

```
dowel check [common options]
```

計画段まで走らせ、診断のみ出す。コンパイルもリンクも実行しない。
glob 展開、パス解決、ツールチェーンの実在まで検査するため、`build` が出す
構成上の診断は `check` でも出る（範囲の根拠は [ADR-0010](adr/0010-check-scope.md)）。
ビルドより速く、保存のたびに回す用途を想定する。

## `dowel build`

```
dowel build [target...] [common options] [build options]
```

ninja ファイルを生成して実行する。ターゲット無指定なら全ての `bin` と `test` を
ビルドする。名指しは `<target>` または `<package>:<target>`。

| オプション | 値 | 既定 | 意味 |
|---|---|---|---|
| `--executor <name>` | `ninja` / `direct` | ninja があれば `ninja` | 実行器。`direct` は逐次実行（depfile を読む mtime 判定） |
| `-j, --jobs <n>` | 数 | ninja の既定 | 並列度。ninja へ渡す |
| `--no-compdb` | — | — | `compile_commands.json` を書き出さない |

- ビルドディレクトリは構成ごとに分かれ、`.dowel/` 配下に置かれる。
  実行ファイルはその `bin/` に出る（`./.dowel/build/*/bin/<name>`）
- コンパイラは `dowel.toml` の `[toolchain]` が指定する（未宣言なら PATH 上の `cc`）。
  ツールチェーンの取得は未実装であり、指定したものは PATH に在る必要がある

## `dowel test`

```
dowel test [target...] [common options] [test options]
```

`test` ターゲットをビルドして起動し、終了状態で合否を判定する（0 = 成功）。
テストハーネスは持たず、C の慣習に従う。作業ディレクトリはパッケージルート。
既定では失敗したテストの出力だけを見せる。

| オプション | 値 | 既定 | 意味 |
|---|---|---|---|
| `--no-run` | — | — | ビルドのみ。実行しない |
| `--nocapture` | — | — | テストの出力を素通しする |
| `--fail-fast` | — | 打ち切らない | 最初の失敗で打ち切る。走らせなかった件数を要約に出す |
| `--failed` | — | — | 前回落ちた分だけ再実行する。判定はビルドディレクトリに残り、走らせなかったターゲットの判定は消えない |
| `--test-jobs <n>` | 数 | 1（逐次） | 同時に走らせる本数。表示は常に要求順 |

- 既定が逐次なのは、C のテストが共有資源（作業ディレクトリ、固定ポート、
  書き出し先）を使う場合があるため
- `--target=<triple>` がホストと異なる場合、宣言されたランナー
  （[10-manifest.md](10-manifest.md) の `[runner.<triple>]`）経由で起動する。
  ランナーが未宣言なら起動前に診断で拒む
- `--message-format=json` で1件1行の結果を stdout に出す

## `dowel why`

```
dowel why <target> <property> [--format <text|json>]
```

値がそのターゲットへ来た経路を、ソース位置つきで根まで表示する。

```
$ dowel why app:app includes

include/                          Path
  ← public.includes of target:foo       libfoo/dowel.build:18
    ← deps of target:app                app/dowel.build:7
```

| オプション | 値 | 既定 |
|---|---|---|
| `--format <fmt>` | `text` / `json` | `text` |

## `dowel graph`

```
dowel graph [--kind <target|action>] [--format <text|dot|json>]
```

グラフを stdout に出す。

| オプション | 値 | 既定 | 意味 |
|---|---|---|---|
| `--kind <kind>` | `target` / `action` | `target` | ターゲット依存グラフ / アクショングラフ |
| `--format <fmt>` | `text` / `dot` / `json` | `text` | 出力形式。`dot` は Graphviz へそのまま渡せる |

## `dowel schema dump`

```
dowel schema dump
```

スキーマと構成語彙を機械可読の形で stdout に出す。全ての `kind` と
プロパティの型・併合規則、構成キー（`cfg` / `host` / `feature` / `tc`）の値域を含む。
言語サーバのホバーと診断が読むのと同じ表であり、二重には持たない。
LLM エージェントへ文脈として与える用途も想定している（[30-devexp.md](30-devexp.md) 4節）。

## `dowel cache`

```
dowel cache info
dowel cache gc
```

| サブコマンド | 意味 |
|---|---|
| `info` | ストアの規模とレコード数を報告する |
| `gc` | 古い形式のストアを回収する |

いずれもマニフェストを読まない。マニフェストが壊れている状態でも掃除できる
必要があるためである。ストアの中身と保証は下記「ストア」を参照。

## `dowel lsp`

```
dowel lsp
```

標準入出力で LSP を話す。エディタが起動主体であり、エディタと共に終了する
（常駐デーモンではない — [ADR-0002](adr/0002-no-daemon.md)）。CLI は言語サーバの
存在に一切依存しない。

- 診断: 全文同期で `publishDiagnostics` を返す。単位は開いているファイル1つ。
  ファイルを跨ぐ診断はまだ出さない（`dowel_lsp::UNSUPPORTED` に理由つきで列挙）
- ホバー: プロパティの型と併合規則、組み込み関数の署名、構成キーの値域
- `dowel.toml` は名前で判別し、厳密な TOML の検証を課す

VS Code 向けクライアントは [`editors/vscode/`](../editors/vscode/README.md) にある。

## 診断の機械可読形式

`--message-format=json` で1行1診断の JSON を stdout に出す。各診断は以下を持つ。

- 重大度と安定コード（`unknown-property` 等）。コードは互換性の対象とする
- ソース位置（複数ラベル）と注記
- 機械適用可能な修正提案（span + 置換文字列）

コードの一覧と、各コードを発生させる最小の入力は
`crates/dowel-cli/tests/diagnostics.rs` の事例表に定義してある。

## ストア

`.dowel/cache/<形式版>/` にメモを保持する（[20-architecture.md](20-architecture.md) 5節）。

書き手は1プロセスに限る。取得できない場合は読み込みのみを行い、結果を書かない。
計算はプロセス内で完結するため、失うのはキャッシュの利得だけであり、結果は変わらない。
ストアを消しても、切り詰めても、外部から書き換えても、結果は変わらず速度のみを失う。

## 例

```sh
dowel check --message-format=json
dowel build --config=release
dowel test --failed --fail-fast
dowel why app:app includes
dowel graph --kind=action --format=dot | dot -Tsvg -o actions.svg
DOWEL_LOG=debug dowel build
```

動く現物は [`examples/hello`](../examples/hello) にある。
`crates/dowel-cli/tests/example.rs` が現物をビルドして検査しているため、
構文や意味論を変えた際の更新漏れは検出される。
