#!/usr/bin/env bash
# 検証の唯一の入口。
#
# ローカルでも CI でも同じものを実行する。CI 環境をローカルと別の手順にしない
# （docs/50-development.md 3節の方針）。
#
# 途中で失敗しても止まらず、全ての段階を実行してから結果をまとめる。
# 「どこで落ちたか」だけでなく「他は通っていたか」が同じ実行で分かる方が、
# 修復の反復が速い。
#
# 出力:
#   .work/verify/summary.md     人間と GitHub の要約向け
#   .work/verify/results.json   機械可読
#   .work/verify/logs/<段階>.log
#   .work/verify/startup.json   起動時間の計測
#
# 環境変数:
#   DOWEL_VERIFY_OUT   出力先（既定: <リポジトリ>/.work/verify）
#   DOWEL_VERIFY_SKIP  空白区切りで飛ばす段階名

set -uo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root" || exit 1

OUT="${DOWEL_VERIFY_OUT:-$repo_root/.work/verify}"
SKIP="${DOWEL_VERIFY_SKIP:-}"
rm -rf "$OUT"
mkdir -p "$OUT/logs"

# 段階ごとの記録。TSV（名前 / 状態 / 秒 / 通過数 / 失敗数 / 必須か）。
records="$OUT/records.tsv"
: >"$records"

now_ms() { python3 -c 'import time; print(int(time.time()*1000))'; }

# run <名前> <必須か: gating|advisory> -- <コマンド...>
run() {
    local name=$1 gating=$2
    shift 3 # 名前・必須か・`--`
    local log="$OUT/logs/$name.log"

    for skipped in $SKIP; do
        if [ "$skipped" = "$name" ]; then
            printf '%s\tskipped\t0\t0\t0\t%s\n' "$name" "$gating" >>"$records"
            printf '  \033[90m-\033[0m    %-22s skipped\n' "$name"
            return 0
        fi
    done

    local start end status
    start=$(now_ms)
    "$@" >"$log" 2>&1
    status=$?
    end=$(now_ms)

    # libtest の "test result: ok. N passed; M failed;" を数え上げる。
    # 要約行だけを対象にする。本文中に同じ語が出ても数に混ぜないため。
    # 集計に awk を使うのは、`bc` が入っていない実行環境があるため。
    local summaries passed failed
    summaries=$(grep -E '^test result:' "$log" || true)
    passed=$(printf '%s\n' "$summaries" | grep -oE '[0-9]+ passed' |
        awk '{s += $1} END {print s + 0}')
    failed=$(printf '%s\n' "$summaries" | grep -oE '[0-9]+ failed' |
        awk '{s += $1} END {print s + 0}')

    local state
    if [ $status -eq 0 ]; then
        state=ok
        printf '  \033[32mok\033[0m   %-22s %5sms  %s passed\n' "$name" "$((end - start))" "$passed"
    elif [ "$gating" = advisory ]; then
        state=warn
        printf '  \033[33mwarn\033[0m %-22s %5sms  advisory (does not fail the run)\n' \
            "$name" "$((end - start))"
    else
        state=failed
        printf '  \033[31mFAIL\033[0m %-22s %5sms\n' "$name" "$((end - start))"
    fi
    printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
        "$name" "$state" "$((end - start))" "$passed" "$failed" "$gating" >>"$records"
    return 0
}

echo "running verification (output: ${OUT#"$repo_root"/})"
echo

# --- 静的な検査 ---------------------------------------------------------
run fmt gating -- cargo fmt --all -- --check
run clippy gating -- cargo clippy --all-targets --all-features -- -D warnings

# --- 単体テスト ---------------------------------------------------------
# クレートごとに分けるのは、失敗が「どの層のものか」を記録に残すため。
run unit-support gating -- cargo test -p dowel-support --lib
run unit-syntax gating -- cargo test -p dowel-syntax --lib
run unit-query gating -- cargo test -p dowel-query --lib
run unit-eval gating -- cargo test -p dowel-eval --lib
run unit-model gating -- cargo test -p dowel-model --lib
run unit-build gating -- cargo test -p dowel-build --lib
run unit-cli gating -- cargo test -p dowel-cli --bins

# --- 統合テスト ---------------------------------------------------------
# 壊れた入力に対してパニックせず、CST がロスレスであること。
run syntax-robustness gating -- cargo test -p dowel-syntax --test robustness
# マニフェスト読み込みからインタフェース併合まで。
run model-integration gating -- cargo test -p dowel-model --test model
# 読み直しの増分性。何を計算しなかったかを数え上げで検査する。
run model-incremental gating -- cargo test -p dowel-model --test incremental

# --- e2e（実際に C をコンパイルして実行する）---------------------------
run e2e gating -- cargo test -p dowel-cli --test e2e
# 時間をまたぐ操作列（編集して再ビルド、構成の切り替え、テストの再実行）。
run scenario gating -- cargo test -p dowel-cli --test scenario
# 現実の形をしたプロジェクトを丸ごと通す。
run fixture gating -- cargo test -p dowel-cli --test fixture
# 診断が利用者まで届くこと、および網羅の追跡。
run diagnostics gating -- cargo test -p dowel-cli --test diagnostics
run example gating -- cargo test -p dowel-cli --test example
# 文書のリンクと索引。腐っても誰も落ちないため、落ちる機構を置く。
run docs gating -- cargo test -p dowel-cli --test docs

# --- 計測 ---------------------------------------------------------------
# 起動時間の予算（無操作時 10ms 以下、docs/20-architecture.md 5.4）の追跡。
run release-build gating -- cargo build --release
run startup advisory -- python3 scripts/measure-startup.py --out "$OUT/startup.json"

# --- 集計 ---------------------------------------------------------------
python3 scripts/verify-report.py --records "$records" --out "$OUT"
status=$?

echo
if [ $status -eq 0 ]; then
    echo "verification passed. summary: ${OUT#"$repo_root"/}/summary.md"
else
    echo "verification failed. summary: ${OUT#"$repo_root"/}/summary.md"
    echo "per-step output is under ${OUT#"$repo_root"/}/logs/"
fi
exit $status
