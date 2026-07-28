# マニフェスト仕様

マニフェストは2ファイルに分離する。根拠は [ADR-0003](adr/0003-manifest-split.md)。

| ファイル | 形式 | 主体 | 内容 |
|---|---|---|---|
| `dowel.toml` | 厳密な TOML | 機械が読み書き | パッケージ情報、依存、ツールチェーン、ポリシー |
| `dowel.build` | TOML 風方言 | 人間が記述 | ターゲット定義、伝播プロパティ、条件分岐 |
| `dowel.lock` | 生成物 | 機械 | 解決結果、ハッシュ、推移的依存 |

## 1. `dowel.toml`

```toml
[package]
name    = "libfoo"
version = "0.3.1"
edition = "2026"

[toolchain]
c       = "clang-19"
sysroot = "x86_64-linux-gnu-glibc2.35"

[policy]
cooldown = "7d"
licenses = ["MIT", "Apache-2.0", "BSD-3-Clause"]

[[dependencies]]
name     = "zlib"
version  = "1.3"
optional = true

[[dependencies]]
name = "bar"
git  = "https://github.com/example/bar"
rev  = "9f3c0a1e2b7d4856c0f1a93e5d2b8c4770ae6135"

[[dependencies]]
name = "mylib"
path = "../mylib"

[[dependencies]]
name    = "winsock-shim"
version = "0.2"
when    = { os = "windows" }

[features]
default = ["zlib"]
```

### 規則

- 厳密な TOML として維持する。値の位置に式を許さない。
  外部ツール（SBOM 生成器、脆弱性スキャナ、更新ボット）が独自パーサなしで読めることを保証する
- 依存指定は 4 形態: レジストリ名 / git / https tarball / ローカルパス
- git 依存はブランチ・タグでの解決を禁止し、フル 40 桁の不変オブジェクト参照を要求する
- 条件は `when = { os = "windows" }` のように**閉じた語彙の構造体**で表す。
  Cargo の `[target.'cfg(windows)'.dependencies]` のような文字列埋め込みの小言語は採らない
  （CMake のジェネレータ式と同じ失敗様式のため）

## 2. `dowel.build`

```
# libfoo/dowel.build

[lib.foo]
sources = glob("src/**.c")

[lib.foo.public]
includes = [dir("include")]
deps     = [dep("bar"), dep("mylib")]

[lib.foo.private]
includes = [dir("src")]
defines  = { FOO_BUILDING = 1 }
deps     = [dep("zlib") when feature.zlib]
flags    = match cfg.opt {
    debug   => ["-O0", "-g3"],
    release => ["-O2", "-DNDEBUG"],
}

[test.unit]
sources = glob("tests/*.c")
deps    = [target("foo")]
```

### TOML から継承する構文

テーブルヘッダ `[a.b.c]`、キー = 値、配列、インラインテーブル、
基本文字列・複数行文字列、`#` 行コメント、暗黙のテーブル生成。

### 値の位置でのみ追加する要素

| 要素 | 記法 | 借用元 |
|---|---|---|
| 関数呼び出し | `glob(...)`, `dir(...)`, `dep(...)`, `target(...)` | 一般 |
| 網羅的分岐 | `match cfg.opt { debug => …, release => … }` | Rust |
| 条件付き要素 | `dep("zlib") when feature.zlib` | 独自（後置） |
| 名前空間参照 | `cfg.opt`, `feature.zlib`, `host.os` | 一般 |

式は**純粋かつ全域**とする。副作用なし、変数束縛なし、反復は有限リストに対する
内包表記のみ、再帰なし。これにより停止性を言語仕様として保証する（[ADR-0004](adr/0004-syntax.md)）。

### テーブル種別

`[<kind>.<name>]` の `kind` は閉じた語彙とし、それぞれスキーマを持つ。未知の `kind` は型検査で落とす。

| kind | 意味 |
|---|---|
| `lib` / `bin` / `test` / `bench` | ターゲット |
| `template` | 再利用単位（非再帰） |
| `toolchain` | ツールチェーン記述 |
| `runner` | 実行ラッパ（qemu 等）。`[runner.<triple>]` で名前はターゲットトリプル |

`runner` だけは名前がターゲット名ではなくターゲットトリプルであり、
プロパティの集合も他と別である（`command` と `args`）。成果物を生成せず、
伝播もしないため、ターゲットと同じ語彙を与えると意味のない記述が型検査を通る。

### `public` / `private`

CMake の `INTERFACE` / `PRIVATE` に相当するが、プロパティ名ごとの修飾ではなく
ブロックで区切る。伝播するものとしないものを構文上分離する。

## 3. 型と併合意味論

Dの実質はここにある。プロパティごとに**併合規則を型として宣言**する。

```
schema {
  includes : Set<Path>        merge = union,  order = topological
  defines  : Map<Ident, Val>  merge = error_on_conflict
  flags    : List<Flag>       merge = append
  abi      : AbiLabel         merge = must_equal
}
```

| 併合規則 | 挙動 |
|---|---|
| `union` | 和集合。順序は依存グラフのトポロジカル順 |
| `append` | 連結。順序を保存する |
| `error_on_conflict` | 異なる値が到達したら両方の来歴を提示して失敗 |
| `must_equal` | 一致しなければ失敗。ABI ラベルの検証はこれで表現される |

併合規則を型に属させることで、プロパティ追加時に検証コードを書き足す必要がなくなる。

### 主要な型

- **`Path`** — `string` と別型。基準点（プロジェクトルート / ビルドディレクトリ / sysroot）を
  型に含み、文字列連結によるパス構築を言語として提供しない。
  CMake における事故の多くはここに由来する
- **`List<T>` / `Set<T>`** — セミコロン区切り文字列という表現を持たない
- **`Cfg<T>`** — 構成でパラメタライズされた `T`。`match` の結果はこの型を持つ。
  ジェネレータ式に相当するが、文字列埋め込みの小言語ではなく通常の型として扱う。
  アクション生成時に構成を与えて具体化するため、`--release` と `--target` の切り替えで
  マニフェスト評価が再実行されない

## 4. 抽象化機構

```
[template.cli_tool]
params = ["name", "srcs"]

[template.cli_tool.bin]
sources = srcs
deps    = [dep("cli-common")]
```

- テンプレートは非再帰。呼び出しグラフに閉路があれば静的に検出して失敗させる
- 反復は有限リストに対する内包表記のみ

## 5. 来歴の表示

```
$ dowel why target:app includes

include/                          Path
  ← public.includes of target:foo       libfoo/dowel.build:18
    ← deps of target:app                app/dowel.build:7
```

来歴チェーンはクエリグラフの部分木をそのまま辿ったものであり、
増分評価エンジンを実装していれば追加のデータ構造を要しない。

## 6. TOML との混同への対処

`dowel.build` は TOML の上位方言であり、既存の TOML ツールは値の位置で失敗する。

- 拡張子を `.toml` にしない。エディタが TOML モードを適用しないようにする
- TOML として妥当だが本仕様で不正な記述には、その旨を診断に明記する
- 補完・強調・診断は自前の言語サーバで提供する

## 7. 未確定

`cfg` / `feature` / `host` / `tc` 名前空間の語彙が未定義。
`dowel.toml` の `when` 述語、`dowel.build` の `match` / `when`、
ツールチェーン選択、ABI ラベルのすべてがこれを参照する。
[99-open-questions.md](99-open-questions.md) を参照。
