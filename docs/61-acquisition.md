# dowel の取得と版の切り替え（dowelup）

`dowelup` は dowel 自体を取得し、プロジェクトごとに版を固定し、透過的に
切り替えるためのコマンドである。設計上の決定は
[ADR-0013](adr/0013-self-acquisition.md) にある。

## 導入

リリースは未整備のため、初回は本リポジトリからビルドする。

```sh
cargo build --release -p dowel-up        # target/release/dowelup
dowelup shim ~/.local/bin                # `dowel` という名前のリンクを作る
```

`dowelup shim <dir>` は `<dir>/dowel` を dowelup へのシンボリックリンクとして
作る。この `dowel` は起動のたびに版を選び、選んだ実体へ exec する。

## 版の指定子

| 形 | 意味 |
|---|---|
| `stable` | 上流の最新の release タグ |
| `nightly` | 既定ブランチの先端 |
| `nightly-YYYY-MM-DD` | 既定ブランチに、その日（UTC）の終わりまでに入った最後のコミット |
| `X.Y.Z` | タグ `vX.Y.Z` または `X.Y.Z` |
| `branch:<name>` | ブランチの先端 |
| `tag:<name>` | 任意のタグ |
| `<sha>` | コミット。一意な接頭辞（7桁以上）でよい |

いずれの形も `install` / `pin` / `default` の時点で commit sha に解決され、
以後は sha が正本になる。上流には release タグがまだ無いため、`stable` と
`X.Y.Z` はタグが現れるまで解決できない。

## コマンド

```sh
dowelup install nightly            # 解決してビルドし、versions/<sha>/ へ置く
dowelup install branch:feature     # 上流の特定ブランチ
dowelup install 2915da5ab          # 特定コミット（接頭辞でよい）
dowelup list                       # インストール済みの一覧。`*` が既定
dowelup default nightly            # pin が無い場所で使う版。未取得なら取得する
dowelup pin nightly                # .dowel-version に解決済みの sha を書く
dowelup which                      # ここで実行される実体のパス
dowelup run branch:feature -- check    # 選択を経ずに特定の版を起動する
dowelup uninstall branch:feature   # 取り除く
```

解決と取得は `git` と `cargo` の起動に委譲する。両方が PATH に要る。
上流は既定で `https://github.com/sabas0ba/dowel` であり、
`--upstream <url>` または環境変数 `DOWELUP_UPSTREAM` で差し替えられる。

出力の分担は dowel 本体（[60-cli.md](60-cli.md)）と同じである。stdout は
成果物（解決した sha、一覧、パス）、stderr は進行と誤りである。

## 版の選択

`dowel`（shim）は次の順で版を選ぶ。

1. 先頭引数の `+<指定子>`（例: `dowel +nightly check`）。
   インストール済みの中から選ぶ
2. カレントディレクトリから上へ辿って最初に見つかる `.dowel-version`
3. `dowelup default` で設定した既定

選択はネットワークに触れない。選ばれた sha が未取得の場合は、
`dowelup install <sha>` を促す誤りになる。

## pin ファイル

`.dowel-version` は `dowelup pin <指定子>` が書く。中身は解決済みの sha と、
どの指定子から解決したかのコメントである。

```
# Managed by dowelup. Resolved from "nightly".
2915da5c1f0e3b7a9d2c4e6f8a0b1c2d3e4f5a6b
```

チャネル名やブランチ名を手書きした場合、shim は解決せずに拒み、
`dowelup pin` での解決を促す。ブランチ名のみの参照は固定とみなさない
（[50-development.md](50-development.md) 5節）ための制約である。

## 配置

| パス | 内容 |
|---|---|
| `$DOWELUP_HOME`（既定 `~/.dowel`） | dowelup の状態の根 |
| `versions/<sha>/bin/dowel` | インストール済みの実体 |
| `versions/<sha>/origin` | どの指定子・どの上流から解決したかの記録。同じ sha への install が別の指定子で来たら追記される |
| `upstream.git` | 解決と取得に使う mirror |
| `default` | pin が無い場所で使う sha |
| `tmp/<sha>` | ビルド中の作業木。成功時に消え、失敗時は調査のために残る |
