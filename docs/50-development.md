# 開発環境

本プロジェクトの開発は、[sabas0ba/dotfiles](https://github.com/sabas0ba/dotfiles) が
定義する Nix / direnv 環境、およびそこから構築する同一内容のコンテナ環境の上で行う。

ホストへツールを直接導入しない。`apt install` / `brew install` / `npm install -g` /
`pip install --user` 等は再現性を損なうため用いない。

## 1. 前提

- [Nix](https://nixos.org/download/)（flakes を有効化すること）
- [direnv](https://direnv.net/)（任意。導入すると `cd` のみで環境に入る）
- Docker（任意。コンテナ環境を使用する場合のみ）

導入手順、バージョン固定の方針、チェックサム検証の手順は dotfiles の README を参照する。
インストーラを検証せず直接実行する方式（`curl ... | sh`）は用いない。

## 2. 環境の構築

```sh
git clone https://github.com/sabas0ba/dotfiles.git ~/repos/dotfiles
cd ~/repos/dotfiles
nix develop
scripts/check-env.sh
```

direnv を使用する場合の設定、および home-manager によるホームディレクトリ構成の
適用手順（`make hm-dry` / `make hm-switch`）も dotfiles の README に従う。

作業は開発シェルの内部で行う。環境変数 `DOTFILES_ENV` が `nix-develop` であれば
開発シェル内である。

## 3. コンテナ環境

ホストと同一の環境をコンテナ内に構築できる。Dockerfile はツール一覧を持たず、
dotfiles の `flake.nix` を評価するため、内容はホストと一致する。

```sh
make docker-build   # イメージの構築
make docker-shell   # コンテナ内の開発シェルに入る
make docker-check   # コンテナ内でのスモークテスト
```

CI を `--network none` のコンテナ内で回すことは、CI 環境をホストおよびコンテナと
別の環境にしないための方針である。

**当面はこの形を採らない。** CI は GitHub Actions の実行機で回す
（[`.github/workflows/verify.yml`](../.github/workflows/verify.yml)）。
dotfiles の flake を本リポジトリの CI から評価する経路が未整備であり、
現時点でそこに手を入れる必要はないと判断した。

移行が要るようになった場合の費用を小さく保つため、**検査の内容は実行環境から
切り離してある**。ローカルも CI も `scripts/verify.sh` という同じ入口を叩き、
ワークフローがするのはそれを起動することだけである。実行環境を移す際は
ワークフローの中身が入れ替わるだけで、検査の定義は動かない。

## 3.1 検証

検証の入口は1つである。

```sh
make verify      # 全段階を実行して結果を .work/verify/ に残す
```

途中の段階が失敗しても止まらず、最後まで進んでから落ちる。「どこで落ちたか」
だけでなく「他は通っていたか」が同じ実行で分かる方が、修復の反復が速いため。

| 生成物 | 内容 |
|---|---|
| `.work/verify/summary.md` | 段階ごとの結果、通過数、所要時間、起動時間の計測 |
| `.work/verify/results.json` | 同じ内容の機械可読な形 |
| `.work/verify/logs/<段階>.log` | 各段階の出力そのまま |
| `.work/verify/startup.json` | 起動時間の計測 |

CI はこの `.work/verify/` を成果物として保存し、`summary.md` をジョブの要約に
出す。失敗した実行の結果こそ残るようにしてある。

段階は次の通り。`fmt` / `clippy` / クレートごとの単体テスト /
パーサの頑健性 / モデルの統合 / e2e / 例 / リリースビルド / 起動時間の計測。
起動時間だけは参考扱いとし、実行機の揺れで全体を落とさない
（明らかな退行のみ拾う緩い上限を置いてある）。

一部だけ回したい場合は段階を飛ばせる。

```sh
DOWEL_VERIFY_SKIP="e2e example" make verify
```

`make check`（整形検査 + 静的解析 + テスト）は素早い確認用であり、記録は残らない。

## 4. ツールの追加

本プロジェクトの実装に必要なツール（コンパイラ、リンカ、qemu、ninja 等）は、
dotfiles 側の `nix/packages.nix` に追記して取得する。手順は以下。

1. `nix/packages.nix` にパッケージ名を追記する
2. コマンドとして使用するものは `scripts/check-env.sh` の `required_commands` にも追記する
3. `make check` が成功することを確認する

`Dockerfile` にツール名を追記しない。定義が重複し不整合が生じるため。

依存パッケージが増えること自体を事前に確認する。

## 5. 規約

dotfiles の README が規約の所在である。本プロジェクト固有の事項のみ以下に記す。
記載のない事項は dotfiles の規約に従う。

### 継承する事項

- コミットは Conventional Commits。1 コミット 1 目的
- 機能追加は branch または worktree で行う
- 一時ファイルはリポジトリ内の git ignore された `.work/` に置く。
  `/tmp` 等リポジトリ外部に作成しない
- 外部の成果物はすべて一意に固定する。タグやブランチ名のみによる参照は固定とみなさない
- 秘密情報をコミットしない。マシン固有の設定は `.envrc.local`（git 管理外）に置く
- 整形は手作業ではなく `make fmt` で行う
- コメントは実装内容ではなく、その選択の理由を記述する

### 本プロジェクト固有

- 実装言語は Rust（[ADR-0007](adr/0007-implementation-language.md)）。
  コアは標準ライブラリのみに依存する。外部 crate の追加は都度合意する
- **プログラムが扱う言語は英語**とする。識別子（テスト名を含む）、文字列リテラル、
  診断・ログ・CLI の出力、生成物（`build.ninja` 等）、メタデータ
  （`Cargo.toml` の `description`、ワークフローの step 名）が対象。
  例外は、非 ASCII の扱いそのものを検査するテストデータのみ。
  **コメントと doc コメント、および `docs/` は日本語**とする。
  「コメントは実装内容ではなく、その選択の理由を記述する」という規約は、
  母語で書いたほうが密度が上がる
- 整形は `make fmt`（`cargo fmt`）、静的解析は `make lint`（`cargo clippy -D warnings`）
- 提出前に `make check`（整形検査 + 静的解析 + テスト）を通す
- 設計上の決定は `docs/adr/` に ADR として記録する。
  決定を覆す場合は当該 ADR を Superseded とし、新しい ADR を追加する

## 6. Claude Code に作業を引き継ぐ場合

以下を明示すること。

- dotfiles の環境（Nix 開発シェルまたはコンテナ）の内部で作業すること
- dotfiles の [`CLAUDE.md`](https://github.com/sabas0ba/dotfiles/blob/main/CLAUDE.md)
  および README を参照すること
- 本リポジトリの `CLAUDE.md` はリポジトリ固有の指示であり、
  共通規約より優先されること

リポジトリルートの [`CLAUDE.md`](../CLAUDE.md) にこの旨を記載してある。
