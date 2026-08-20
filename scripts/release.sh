#!/usr/bin/env bash
# リリースを1つ作る。押すのは人である。
#
#   scripts/release.sh 0.1.0
#
# 版を書き換え、検査を通し、コミットしてタグを打つところまでを行う。
# **押さない**——タグを押した時点で資産が公開される（release.yml）ので、
# 取り消せない操作は人の手に残す。最後に押す命令を刷る。
#
# 版はワークスペースの1箇所（Cargo.toml の [workspace.package]）が持つ。
# dowelup だけは独立に版を進めるので、ここでは触らない。
set -euo pipefail

cd "$(dirname "$0")/.."

usage() {
	echo "usage: scripts/release.sh <version>   (for example: scripts/release.sh 0.1.0)" >&2
	exit 2
}

[ $# -eq 1 ] || usage
version="$1"
# 三部からなる版だけを受ける。dowelup が解決に使う形（ADR-0036）である。
if ! printf '%s' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'; then
	echo "error: \`$version\` is not a three-part version" >&2
	usage
fi
tag="v$version"

# 木が汚れていると、何を配ったのかがコミットから読み取れなくなる。
if [ -n "$(git status --porcelain)" ]; then
	echo "error: the working tree is not clean; commit or stash first" >&2
	git status --short >&2
	exit 1
fi
if git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
	echo "error: the tag \`$tag\` already exists" >&2
	exit 1
fi

branch="$(git rev-parse --abbrev-ref HEAD)"
if [ "$branch" != main ]; then
	# 拒みはしない。手元で試すための枝から作りたいことはある。
	echo "note: on \`$branch\`, not \`main\`" >&2
fi

current="$(grep -m1 '^version = ' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')"
if [ "$current" = "$version" ]; then
	echo "the version is already $version"
else
	echo "version $current -> $version"
	# 書き換えるのは [workspace.package] の1行だけである。`sed -i` は
	# GNU と BSD で綴りが違うので使わない——リリースを切る機械は Linux
	# とも限らない。
	awk -v old="$current" -v new="$version" '
		!done && $0 == "version = \"" old "\"" { print "version = \"" new "\""; done = 1; next }
		{ print }
	' Cargo.toml > Cargo.toml.new
	mv Cargo.toml.new Cargo.toml
	# ロックの版も一緒に動かす。`--locked` で組むのはこの後の CI であり、
	# ロックが古いままだとそこで落ちる。
	cargo update --workspace >/dev/null
fi

# 落ちるものを配らない。CI も同じ関門を持つが（release.yml の verify）、
# 押してから知るのでは遅い。
echo "running the full verification"
make verify

# 版が既にその値なら、コミットするものが無い。タグだけを打つ。
if [ -n "$(git status --porcelain)" ]; then
	git add -A
	git commit -m "chore(release): $version"
else
	echo "nothing to commit; tagging $(git rev-parse --short HEAD)"
fi
# 注釈つきのタグにする。誰がいつ切ったかがタグ自身に残る。
git tag -a "$tag" -m "dowel $version"

cat <<EOF

$tag is ready. Nothing has been pushed.

  git push origin $branch
  git push origin $tag

Pushing the tag starts .github/workflows/release.yml, which verifies the
commit again, builds an asset per published triple, and publishes them
beside their .sha256 files. dowelup reads them from there.
EOF
