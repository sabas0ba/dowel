# 検査は個別のコマンドではなく make 経由で行う。
# CI とローカルで同じ入口を使うため。

.PHONY: all verify check fmt fmt-check lint test e2e measure build clean

all: verify

# 検証の入口。全ての段階を実行し、途中で失敗しても最後まで進んで結果を残す。
# 結果は .work/verify/（summary.md / results.json / logs/）に置かれる。
# CI（.github/workflows/verify.yml）が叩くのもこれ。
verify:
	scripts/verify.sh

# 提出前の素早い確認。verify より速いが、記録は残らない。
check: fmt-check lint test

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

lint:
	cargo clippy --all-targets --all-features -- -D warnings

test:
	cargo test --workspace

# 実際に C をコンパイルして実行する検証。
e2e:
	cargo test -p dowel-cli --test e2e --test example -- --nocapture

# 起動時間の計測のみ。リリースビルドを要する。
measure: build
	python3 scripts/measure-startup.py --out .work/verify/startup.json

build:
	cargo build --release

clean:
	cargo clean
	rm -rf .work/verify .work/startup
