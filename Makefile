# 検査は個別のコマンドではなく make 経由で行う。
# CI とローカルで同じ入口を使うため。

.PHONY: all check fmt fmt-check lint test e2e build clean

all: check

# 提出前に通すもの
check: fmt-check lint test

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

lint:
	cargo clippy --all-targets --all-features -- -D warnings

# 単体テスト
test:
	cargo test --workspace

# 実際に C をコンパイルして実行する検証。時間がかかるため分けてある。
e2e:
	cargo test -p dowel-cli --test e2e --test example -- --nocapture

build:
	cargo build --release

clean:
	cargo clean
