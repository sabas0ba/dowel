#!/bin/sh
# VS Code 拡張の開発をコンテナに閉じ込める入口。
#
# npm と Node の実行は全てここを経由する。拡張の開発は npm の依存を伴い、
# その供給網をホスト環境（CLAUDE.md の言う開発シェル）へ持ち込まないため。
# 依存の実体は editors/vscode/node_modules に、npm の書き込みは .work/ に閉じる。
#
#   ./dev.sh npm ci            # 依存の取得（package-lock.json どおり）
#   ./dev.sh npm run build     # tsc
#   ./dev.sh npm test          # 単体＋統合（dowel バイナリがあれば）
#
# 統合テストはリポジトリの target/ から dowel を探す。コンテナの libc は
# ホストより古いことがあるため、musl 版を先に作っておくと確実に動く。
#
#   cargo build -p dowel-cli --target x86_64-unknown-linux-musl
#
# TLS を検査するプロキシの下では、CA バンドルを NODE_EXTRA_CA_CERTS で
# 指しておくとコンテナへ引き継がれる。
set -eu

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
image=${DOWEL_VSCODE_NODE_IMAGE:-node:22-bookworm-slim}

# npm のキャッシュ等は一時ファイル置き場（.work/、git ignore 対象）へ。
work="$root/.work/vscode-home"
mkdir -p "$work"

# ここから先は docker run の引数を前へ積んでいく。
set -- "$image" "$@"

if [ -n "${NODE_EXTRA_CA_CERTS:-}" ] && [ -f "$NODE_EXTRA_CA_CERTS" ]; then
  set -- \
    -v "$NODE_EXTRA_CA_CERTS:/etc/ssl/extra-ca.crt:ro" \
    -e NODE_EXTRA_CA_CERTS=/etc/ssl/extra-ca.crt \
    -e npm_config_cafile=/etc/ssl/extra-ca.crt \
    "$@"
fi

exec docker run --rm -i \
  -u "$(id -u):$(id -g)" \
  -v "$root:$root" \
  -w "$here" \
  -e HOME="$work" \
  -e npm_config_update_notifier=false \
  "$@"
