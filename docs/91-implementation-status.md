# 実装状況

[90-roadmap.md](90-roadmap.md) はフェーズ単位の計画である。本文書は
**現に動くもの**を記録する。両者が食い違う場合、本文書が現況を示す。

## 方針: 縦に薄く貫通させる

ロードマップは Phase 1（コア）を完成させてから Phase 2（生成）へ進む順序を
示しているが、実装はこれを一度だけ崩す。パーサ → 評価 → ターゲットグラフ →
アクショングラフ → ninja 生成 → 実際のコンパイル までを最小の幅で先に通す。

理由は2つ。

- **e2e 検証を最初から持てる**。「実際に C をコンパイルして実行し、
  期待した出力が得られる」というテストが最初から存在する状態は、
  以後の全ての変更に対する安全網になる
- **後付け不可能な制約（docs/20-architecture.md 2節）の検証が早まる**。
  ロスレス CST・スパンの全面保持は、下流（アクション生成）まで通してはじめて
  「本当に保持できているか」が分かる

貫通後、増分クエリエンジンと永続化ストアを差し込む。差し込み先は
`dowel_model::session::Session` に閉じてある。

## 使い方

```sh
cargo build --release            # target/release/dowel

dowel check                      # 評価と診断のみ。ビルドしない
dowel build                      # 実際にビルドする
dowel build --config=release
dowel test                       # test ターゲットをビルドして走らせる
dowel test --nocapture           # テストの出力を素通しする
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

## クレート構成

| クレート | 責務 |
|---|---|
| `dowel-support` | スパン、ソースマップ、診断、構造化ログ、JSON 出力 |
| `dowel-syntax` | 字句解析、ロスレス CST、誤り耐性のあるパーサ |
| `dowel-eval` | 型つき値と来歴、式評価、スキーマと併合意味論、構成の具体化 |
| `dowel-model` | パッケージ読み込み、ターゲット、依存グラフ、インタフェース併合、`why` |
| `dowel-build` | glob 展開、アクショングラフ、ninja 生成、`compile_commands.json`、実行 |
| `dowel-cli` | `dowel` バイナリ |

## 実装済み

### 構文（`dowel-syntax`）

- 全バイトがちょうど1トークンに属する字句解析。空白・改行・コメントも残す
- ロスレス CST。木を辿って連結すると入力に戻る（テストで常時検査）
- 誤り耐性。構文誤りで停止せず `Error` ノードを残して継続する。
  復帰は必ず1トークン以上を消費し、ループの前進を保証する
- テーブル見出し、配列テーブル、key-value、配列、インラインテーブル、
  関数呼び出し、`match`、後置 `when`、名前空間参照
- 頑健性テスト: 実マニフェストの全接頭辞・1文字削除・区切り記号の挿入に対し、
  パニックせずロスレス性が保たれること

### 評価（`dowel-eval`）

- `Value = { type, data, provenance }`。来歴は値の構成要素であり根まで辿れる
- `Path` は `Str` と別型。文字列連結によるパス構築を言語として提供しない
- `Cfg<T>`。`match` と後置 `when` の解決は具体化段階まで遅らせる。
  `--release` や `--target` の切り替えでマニフェスト評価をやり直さない
- `glob` の展開も評価では行わない。評価時に走査すると、その時点の
  ファイルシステムという記録されない入力が結果に混ざる
- 併合規則を型に属させる: `union` / `append` / `error_on_conflict` /
  `must_equal` / `replace`
- `match` の網羅性検査。値域が閉じた `cfg` は列挙を要求し、
  値域が開いた `cfg.target` は `_` を要求する
- `dowel.toml` の厳密性は構文ではなく検証で課す

### モデル（`dowel-model`）

- `path` 依存を辿った複数パッケージの読み込み
- スキーマに照らした未知プロパティ・型不一致の診断（候補提示つき）
- `interface(T)` と `compile_env(T)` の分離。`private` の依存は
  自分のコンパイルには効くが依存元へは伝播しない
- 機能フラグによって依存グラフの辺が現れ／消える
- 閉路の検出（反復深さ優先、経路を注記に出す）
- `dowel why` — 伝播経路の表示（text / json）

### ビルド（`dowel-build`）

- `glob` 展開（`*` / `**` / `?`）。走査順に依存しないよう辞書順に並べる
- アクショングラフ（コンパイル / アーカイブ / リンク）
- ninja ファイル生成と `compile_commands.json`（`arguments` 配列形式）
- 実行器2種。ninja（既定）と direct（逐次、depfile を読む mtime 判定）
- 構成ごとに分けたビルドディレクトリ
- `dowel test` — test ターゲットを起動して終了状態で合否を判定する。
  テストハーネスは持たず、「終了状態 0 なら成功」という C の慣習に従う。
  作業ディレクトリはパッケージルート。失敗したものだけ出力を見せる。
  `--no-run` / `--nocapture`、`--message-format=json` で1件1行の結果

### 診断とログ

- 重大度・安定コード・複数ラベル・注記・**機械適用可能な修正提案**
- 人間向け描画（rustc 書式）と `--message-format=json`（1行1診断）
- 未知の名前には編集距離で候補を出す（プロパティ、関数、構成キー、
  `match` のアーム、CLI のオプションとコマンド）
- 段階ごとの所要時間、依存グラフの辺、アクションのコマンド列をログに出す

`--log-level=trace` で出るもの（デバッグ時に「なぜこの引数になったのか」を追う材料）。

| 出所 | 内容 |
|---|---|
| `session` | 読み込んだファイルと大きさ、評価したテーブルとキーの値、ターゲットへ割り当てたプロパティ |
| `graph` | 辺の解決、トポロジカル順 |
| `interface` | プロパティごとに到達した値の件数と併合結果（`interface` と `compile_env` の双方） |
| `specialize` | `match` が選んだアーム、`when` が落とした要素 |
| `glob` | 走査したファイルと一致／不一致、走査から外したディレクトリ、一致件数 |
| `plan` | 解決済みのソース・インクルード・定義・フラグ、各アクションの完全なコマンド列 |
| `exec` | 最新と判定した理由、再実行の理由（どの入力が新しいか） |
| `test` | 起動したテストと、その作業ディレクトリ・コマンド |

## 検証

入口は1つ。ローカルでも CI でも同じものを実行する。

```sh
make verify      # 全段階を実行し、結果を .work/verify/ に残す
```

途中で失敗しても止まらず、最後まで進んでから落ちる。結果は
`summary.md`（人間と GitHub の要約向け）、`results.json`（機械可読）、
`logs/<段階>.log` に残る。CI（`.github/workflows/verify.yml`）はこれを
成果物として保存し、要約をジョブのサマリに出す。詳細は
[50-development.md](50-development.md) 3.1 節。

現在の内訳（テスト 153 件）。

| 段階 | 内容 | 件数 |
|---|---|---|
| `fmt` / `clippy` | 整形検査と静的解析（`-D warnings`） | — |
| `unit-*` | クレートごとの単体テスト | 111 |
| `syntax-robustness` | 壊れた入力に対するパニック不在とロスレス性 | 5 |
| `model-integration` | マニフェスト読み込みからインタフェース併合まで | 10 |
| `e2e` | 実際に C をコンパイルして実行し出力を検査 | 24 |
| `example` | `examples/hello` の現物をビルドし、テストを走らせる | 3 |
| `startup` | 起動時間の計測（参考。実行機の揺れで全体を落とさない） | — |

## 計測

起動時間の予算は無操作時 10ms 以下（docs/20-architecture.md 5.4）。
リリースビルド、2パッケージ・2ターゲットの構成、20回の最小値／中央値。
`make measure` で単独に取れる。

| 実行 | 最小 | 中央 |
|---|---|---|
| `dowel --version` | 1.2ms | 1.5ms |
| `dowel check` | 1.4ms | 1.6ms |
| `dowel graph --format=json` | 1.4ms | 1.6ms |

バイナリ 1.0MB、動的リンクは libc 等 4 件。現時点では予算内にある。
増分エンジンと永続化ストアを入れた後に再測する。

## 未実装（意識的に後回しにしているもの）

| 項目 | 位置づけ |
|---|---|
| 増分クエリエンジン（early cutoff、キャンセル、耐久度階層） | Phase 1。`Session` へ差し込む |
| 永続化ストア（mmap インデックス + 追記ログ、`flock`） | Phase 1。同上 |
| プローブ事実 DB | Phase 2 |
| `bench` / `template` / `toolchain` / `runner` の各種別 | Phase 2 / 4 |
| 移行（`migrate verify` / `import`） | Phase 3 |
| ランナー抽象（qemu / SSH / 実機）、`dowel debug`、言語サーバ | Phase 4。差し込み口は `dowel_build::testing::Launcher` に用意済み |
| 依存の取得（レジストリ / git / tarball）、`dowel.lock` | Phase 5。現状は `path` 依存のみ |
| ABI ラベルの自動算出 | Phase 6。現状は手書きの `abi` に対する `must_equal` 検証のみ |

## 設計文書との差異

実装の都合で文書の記述から外れた点を明示する。文書側を書き換えるかは別途判断する。

| 箇所 | 文書 | 実装 | 理由 |
|---|---|---|---|
| [ADR-0003](adr/0003-manifest-split.md) の帰結 | 「パーサが2系統になる」 | パーサは1系統。`dowel.toml` の厳密性は検証で課す | ADR の根拠（第三者ツールが独自パーサなしで読める）は検証で同じく満たされる。木が1つの方が来歴と診断の経路が単純 |
| [10-manifest.md](10-manifest.md) 3節 | `includes` は「トポロジカル順」 | 自分が先、依存が後 | インクルード探索でもリンク順でも依存元が先に来るのが期待される挙動。トポロジカル順の向きを実装で確定させた |
| 型 | `defines : Map<Ident, Val>` | `Val` を型として実装 | 文書の記法をそのまま型にした |
| `abi` | ABI ラベルは算出される | 現状は文字列で手書き | 算出は Phase 6。`must_equal` の経路だけ先に通してある |
| [50-development.md](50-development.md) 3節 | CI は dotfiles から構築した `--network none` のコンテナ内 | GitHub Actions の実行機（当面はこのままとする） | dotfiles の flake を本リポジトリの CI から評価する経路が未整備であり、現時点で手を入れる必要はないと判断した。検査の定義は `scripts/verify.sh` に一本化してあるため、移行が要るようになった際はワークフローの中身が入れ替わるだけで済む |
