# はじめる

導入から、最初のプロジェクトのビルド・テスト・実行まで。
タスク別の使い方は [62-guides.md](62-guides.md)、マニフェストの仕様は
[10-manifest.md](10-manifest.md)、コマンドの仕様は [60-cli.md](60-cli.md) にある。

## 1. 導入

配布物はまだ無い。ソースからビルドする。必要なものは以下。

| もの | 用途 |
|---|---|
| Rust ツールチェーン（`cargo`） | `dowel` 自体のビルド |
| C コンパイラ | プロジェクトのコンパイル。既定は PATH 上の `cc` |
| `ninja` | 既定の実行器。無い環境では逐次実行器（`--executor=direct`）が使える |

```sh
git clone https://github.com/sabas0ba/dowel
cd dowel
cargo build --release
export PATH="$PWD/target/release:$PATH"

dowel --version
```

## 2. 動く例を試す

[`examples/hello`](../examples/hello) は、静的ライブラリ（`libgreet`）と
それを使う実行ファイル（`app`）の2パッケージ構成である。

```sh
cd examples/hello/app
dowel check                      # 診断のみ出す。ビルドしない
dowel build                      # ninja を生成して実行する
./.dowel/build/*/bin/app

cd ../libgreet
dowel test                       # test ターゲットをビルドして走らせる
```

## 3. 最小のプロジェクトを作る

必要なファイルは2つ。`dowel.toml`（パッケージ情報。機械が読み書きする厳密な TOML）と
`dowel.build`（ターゲット定義。人間が書く）。分離の理由は
[10-manifest.md](10-manifest.md) にある。

```
myapp/
├── dowel.toml
├── dowel.build
└── src/
    └── main.c
```

`dowel.toml`:

```toml
[package]
name    = "myapp"
version = "0.1.0"
edition = "2026"
```

`dowel.build`:

```
[bin.myapp]
sources = glob("src/*.c")
```

```sh
dowel check
dowel build
./.dowel/build/*/bin/myapp
```

成果物と中間ファイルは `.dowel/` に置かれる（git ignore 対象にする）。
ビルドディレクトリは構成（`--config`）ごとに分かれる。`.dowel/` は
いつ消しても正しさを失わない。失うのはキャッシュの利得だけである
（[60-cli.md](60-cli.md) ストア節）。

## 4. ライブラリに分け、依存する

パッケージ間の依存は `dowel.toml` に宣言する（現状はローカルパスのみ。
レジストリ / git / tarball の取得は未実装 —
[91-implementation-status.md](91-implementation-status.md)）。
どのターゲットがその依存を使うかは `dowel.build` に書く。

`app/dowel.toml`:

```toml
[[dependencies]]
name = "libgreet"
path = "../libgreet"
```

`app/dowel.build`:

```
[bin.app.private]
deps = [dep("libgreet")]
```

ライブラリ側は、依存元へ伝播するもの（`public`）と自分にのみ効くもの
（`private`）をブロックで分ける。

```
[lib.greet]
sources = glob("src/**.c")

[lib.greet.public]
includes = [dir("include")]      # app のコンパイルにも効く

[lib.greet.private]
includes = [dir("src")]          # 自分にのみ効く。app からは見えない
```

伝播した値がどこから来たかは `dowel why` で辿れる。

```sh
dowel why app:app includes
```

## 5. テストを足す

```
[test.unit]
sources = glob("tests/*.c")

[test.unit.private]
deps = [target("greet")]
```

`dowel test` がビルドして起動し、終了状態 0 を成功とする。C の慣習に従い、
専用のテストハーネスは課さない。

```sh
dowel test
dowel test --nocapture           # テストの出力を素通しする
dowel test --failed --fail-fast  # 前回落ちた分だけ、最初の失敗で打ち切る
```

## 6. 日常のループ

- `dowel check` — 保存のたびに。計画まで走らせ診断だけ出す（実行しない）ので速い
- `dowel build` / `dowel test` — 実際に確かめる
- `dowel why <target> <property>` — 「なぜこの値になったのか」を伝播経路で答える
- `DOWEL_LOG=debug dowel build` — 「なぜ再ビルドされたのか」をログで答える

エディタで書くなら言語サーバがある（`dowel lsp`。
[62-guides.md](62-guides.md) 6節）。診断は位置と安定コードを持ち、
未知の名前には候補が提示される。

## 7. 次に読むもの

- タスク別の使い方（構成切り替え、クロス実行、CI 連携）— [62-guides.md](62-guides.md)
- マニフェストに書ける全て — [10-manifest.md](10-manifest.md)
- コマンドとオプションの全て — [60-cli.md](60-cli.md)
- いま何が動き、何が未実装か — [91-implementation-status.md](91-implementation-status.md)
