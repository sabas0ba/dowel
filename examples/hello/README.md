# hello — 動く最小の例

静的ライブラリ（`libgreet`）と、それを使う実行ファイル（`app`）の2パッケージ。
`dowel` が実際に C をコンパイルできることを示す。

```sh
cd app
dowel check
dowel build
./.dowel/build/*/bin/app
```

見どころ:

- `libgreet/include` は `public.includes`、`libgreet/src` は `private.includes`。
  `app` から前者のヘッダは見えるが、後者は見えない
- `public.defines` の `GREET_API` は `app` のコンパイルにも効く
- `flags` は `match cfg.opt` で構成ごとに切り替わる。
  `dowel build --config=release` で確かめられる

伝播の経路は `dowel why` で辿れる。

```sh
dowel why app:app includes
dowel graph --kind=action
```

この例は `crates/dowel-cli/tests/example.rs` がビルドして検査している。
